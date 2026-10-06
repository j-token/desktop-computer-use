#include "capture.hpp"

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>
#include <roapi.h>
#include <wincodec.h>
#include <d3d11.h>
#include <dxgi1_2.h>
#include <wrl/client.h>

#include <winrt/base.h>
#include <winrt/Windows.Foundation.h>
#include <winrt/Windows.Graphics.Capture.h>
#include <winrt/Windows.Graphics.DirectX.Direct3D11.h>
#include <windows.graphics.capture.interop.h>
#include <windows.graphics.directx.direct3d11.interop.h>

#include <algorithm>
#include <cmath>
#include <chrono>
#include <condition_variable>
#include <cstdint>
#include <mutex>
#include <cstring>
#include <filesystem>
#include <vector>

using Microsoft::WRL::ComPtr;
using winrt::Windows::Graphics::Capture::Direct3D11CaptureFrame;
using winrt::Windows::Graphics::Capture::Direct3D11CaptureFramePool;
using winrt::Windows::Graphics::Capture::GraphicsCaptureItem;
using winrt::Windows::Graphics::Capture::GraphicsCaptureSession;
using winrt::Windows::Graphics::DirectX::DirectXPixelFormat;
using winrt::Windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;

namespace dcu::windows {
namespace {

std::wstring wide(const std::string& value) {
    if (value.empty()) return {};
    const int count = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, value.data(),
                                          static_cast<int>(value.size()), nullptr, 0);
    if (count <= 0) throw Error("invalid_path", "Capture path is not valid UTF-8");
    std::wstring result(static_cast<std::size_t>(count), L'\0');
    MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, value.data(),
                        static_cast<int>(value.size()), result.data(), count);
    return result;
}

std::int64_t unix_time_milliseconds() {
    return std::chrono::duration_cast<std::chrono::milliseconds>(
               std::chrono::system_clock::now().time_since_epoch())
        .count();
}

std::int64_t qpc_time_100ns() {
    constexpr std::int64_t intervalsPerSecond = 10000000;
    LARGE_INTEGER counter{}, frequency{};
    QueryPerformanceCounter(&counter);
    QueryPerformanceFrequency(&frequency);
    // WGC SystemRelativeTime uses the same QPC epoch in 100 ns intervals.
    return static_cast<std::int64_t>(static_cast<long double>(counter.QuadPart) *
                                     intervalsPerSecond / frequency.QuadPart);
}

GUID container_for(const std::string& format) {
    return format == "png" ? GUID_ContainerFormatPng : GUID_ContainerFormatJpeg;
}

class RoScope final {
public:
    RoScope() : result_(RoInitialize(RO_INIT_MULTITHREADED)) {}
    ~RoScope() {
        if (SUCCEEDED(result_)) RoUninitialize();
    }
    bool ok() const { return SUCCEEDED(result_); }

private:
    HRESULT result_;
};

} // namespace

struct WindowCapture::Impl {
    ComPtr<ID3D11Device> device;
    ComPtr<ID3D11DeviceContext> deviceContext;
    ComPtr<IWICImagingFactory> wic;
    ComPtr<ID3D11Texture2D> staging;
    int stagingWidth = 0;
    int stagingHeight = 0;

    GraphicsCaptureItem item{nullptr};
    Direct3D11CaptureFramePool framePool{nullptr};
    GraphicsCaptureSession session{nullptr};
    winrt::event_token frameToken{};
    HWND target = nullptr;
    int targetWidth = 0;
    int targetHeight = 0;
    bool wgcReady = false;
    bool hasLastFrame = false;
    std::vector<std::uint8_t> lastPixels;
    int lastWidth = 0;
    int lastHeight = 0;
    std::int64_t lastTimestamp100ns = 0;
    std::int64_t lastCapturedAtUnixMs = 0;

    std::mutex frameMutex;
    std::condition_variable frameCv;
    bool frameSignalled = false;
    Direct3D11CaptureFrame latestFrame{nullptr};
    std::atomic<std::int64_t> minimumFrameTimestamp100ns{0};

    ~Impl() {
        reset_wgc();
    }

    void reset_wgc() noexcept {
        try {
            if (framePool && frameToken.value) framePool.FrameArrived(frameToken);
            if (session) session.Close();
            if (framePool) framePool.Close();
        } catch (...) {
        }
        frameToken = {};
        session = nullptr;
        framePool = nullptr;
        item = nullptr;
        target = nullptr;
        targetWidth = targetHeight = 0;
        wgcReady = false;
        hasLastFrame = false;
        lastPixels.clear();
        lastWidth = lastHeight = 0;
        lastTimestamp100ns = 0;
        lastCapturedAtUnixMs = 0;
        // Staging textures belong to their creating D3D device. A restarted
        // capture creates a new device even when the window size is unchanged.
        staging.Reset();
        stagingWidth = 0;
        stagingHeight = 0;
        std::lock_guard lock(frameMutex);
        if (latestFrame) latestFrame.Close();
        latestFrame = nullptr;
        frameSignalled = false;
    }

    void ensure_wic() {
        if (wic) return;
        HRESULT hr = CoCreateInstance(CLSID_WICImagingFactory2, nullptr,
                                      CLSCTX_INPROC_SERVER, IID_PPV_ARGS(&wic));
        if (FAILED(hr)) {
            winrt::check_hresult(CoCreateInstance(CLSID_WICImagingFactory, nullptr,
                                                  CLSCTX_INPROC_SERVER,
                                                  IID_PPV_ARGS(&wic)));
        }
    }

    bool ensure_wgc(HWND hwnd, int width, int height) {
        if (!IsWindow(hwnd) || width <= 0 || height <= 0) {
            return false;
        }
        if (wgcReady && target == hwnd && targetWidth == width && targetHeight == height) {
            return true;
        }
        reset_wgc();

        UINT flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT;
        D3D_FEATURE_LEVEL featureLevels[] = {D3D_FEATURE_LEVEL_11_1,
                                             D3D_FEATURE_LEVEL_11_0,
                                             D3D_FEATURE_LEVEL_10_1};
        D3D_FEATURE_LEVEL selected{};
        HRESULT hr = D3D11CreateDevice(
            nullptr, D3D_DRIVER_TYPE_HARDWARE, nullptr, flags, featureLevels,
            static_cast<UINT>(std::size(featureLevels)), D3D11_SDK_VERSION, &device,
            &selected, &deviceContext);
        if (FAILED(hr)) {
            hr = D3D11CreateDevice(nullptr, D3D_DRIVER_TYPE_WARP, nullptr, flags,
                                   featureLevels,
                                   static_cast<UINT>(std::size(featureLevels)),
                                   D3D11_SDK_VERSION, &device, &selected,
                                   &deviceContext);
        }
        if (FAILED(hr)) return false;

        ComPtr<IDXGIDevice> dxgiDevice;
        if (FAILED(device.As(&dxgiDevice))) return false;
        winrt::com_ptr<IInspectable> inspectableDevice;
        hr = CreateDirect3D11DeviceFromDXGIDevice(dxgiDevice.Get(),
                                                  inspectableDevice.put());
        if (FAILED(hr)) return false;
        IDirect3DDevice directDevice = inspectableDevice.as<IDirect3DDevice>();
        if (!directDevice) return false;

        try {
            auto factory = winrt::get_activation_factory<GraphicsCaptureItem,
                                                         IGraphicsCaptureItemInterop>();
            GraphicsCaptureItem newItem{nullptr};
            winrt::check_hresult(factory->CreateForWindow(
                hwnd, winrt::guid_of<GraphicsCaptureItem>(), winrt::put_abi(newItem)));
            const auto size = newItem.Size();
            if (size.Width <= 0 || size.Height <= 0) return false;

            auto pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
                directDevice, DirectXPixelFormat::B8G8R8A8UIntNormalized, 2, size);
            auto captureSession = pool.CreateCaptureSession(newItem);
            captureSession.IsCursorCaptureEnabled(false);
            {
                std::lock_guard lock(frameMutex);
                frameSignalled = false;
            }
            auto token = pool.FrameArrived([this](auto const& sender, auto const&) {
                try {
                    std::lock_guard lock(frameMutex);
                    // Drain continuously, including while a drag is executing.
                    // Keeping only the newest frame avoids an early-action backlog.
                    for (;;) {
                        auto frame = sender.TryGetNextFrame();
                        if (!frame) break;
                        if (latestFrame) latestFrame.Close();
                        latestFrame = std::move(frame);
                        frameSignalled = true;
                    }
                } catch (...) { return; }
                frameCv.notify_one();
            });
            item = std::move(newItem);
            framePool = std::move(pool);
            session = std::move(captureSession);
            frameToken = token;
            target = hwnd;
            targetWidth = width;
            targetHeight = height;
            session.StartCapture();
            wgcReady = true;
            return true;
        } catch (...) {
            reset_wgc();
            return false;
        }
    }

    bool next_frame(Context& context, Direct3D11CaptureFrame& result,
                    std::int64_t& timestamp100ns, int waitMilliseconds,
                    std::int64_t minimumTimestamp100ns) {
        if (!framePool) return false;
        const auto deadline = std::chrono::steady_clock::now() +
                              std::chrono::milliseconds(std::max(0, waitMilliseconds));
        for (;;) {
            context.check();
            try {
                std::lock_guard lock(frameMutex);
                if (latestFrame) {
                    timestamp100ns = latestFrame.SystemRelativeTime().count();
                    frameSignalled = false;
                    if (timestamp100ns < minimumTimestamp100ns) {
                        latestFrame.Close();
                        latestFrame = nullptr;
                    } else {
                        result = std::move(latestFrame);
                        return true;
                    }
                }
            } catch (...) {
                return false;
            }
            const auto now = std::chrono::steady_clock::now();
            if (now >= deadline) return false;
            std::unique_lock lock(frameMutex);
            frameCv.wait_for(lock, std::min(std::chrono::milliseconds(30),
                                            std::chrono::duration_cast<std::chrono::milliseconds>(
                                                deadline - now)),
                             [this] { return frameSignalled; });
        }
    }

    bool copy_frame(const Direct3D11CaptureFrame& frame, std::vector<std::uint8_t>& pixels,
                    int& width, int& height) {
        auto surface = frame.Surface();
        auto access = surface.as<
            ::Windows::Graphics::DirectX::Direct3D11::IDirect3DDxgiInterfaceAccess>();
        ComPtr<ID3D11Texture2D> source;
        if (FAILED(access->GetInterface(IID_PPV_ARGS(&source)))) return false;
        D3D11_TEXTURE2D_DESC desc{};
        source->GetDesc(&desc);
        width = static_cast<int>(desc.Width);
        height = static_cast<int>(desc.Height);
        if (width <= 0 || height <= 0) return false;

        if (!staging || stagingWidth != width || stagingHeight != height) {
            D3D11_TEXTURE2D_DESC stagingDesc = desc;
            stagingDesc.Usage = D3D11_USAGE_STAGING;
            stagingDesc.BindFlags = 0;
            stagingDesc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
            stagingDesc.MiscFlags = 0;
            staging.Reset();
            if (FAILED(device->CreateTexture2D(&stagingDesc, nullptr, &staging))) {
                return false;
            }
            stagingWidth = width;
            stagingHeight = height;
        }
        deviceContext->CopyResource(staging.Get(), source.Get());
        D3D11_MAPPED_SUBRESOURCE mapped{};
        if (FAILED(deviceContext->Map(staging.Get(), 0, D3D11_MAP_READ, 0, &mapped))) {
            return false;
        }
        pixels.resize(static_cast<std::size_t>(width) * height * 4);
        const auto rowBytes = static_cast<std::size_t>(width) * 4;
        for (int y = 0; y < height; ++y) {
            std::memcpy(pixels.data() + static_cast<std::size_t>(y) * rowBytes,
                        static_cast<const std::uint8_t*>(mapped.pData) +
                            static_cast<std::size_t>(y) * mapped.RowPitch,
                        rowBytes);
        }
        deviceContext->Unmap(staging.Get(), 0);
        return true;
    }

    bool capture_gdi(HWND hwnd, const RECT& rect, std::vector<std::uint8_t>& pixels,
                     int& width, int& height) {
        width = rect.right - rect.left;
        height = rect.bottom - rect.top;
        if (width <= 0 || height <= 0) return false;
        HDC screen = GetDC(nullptr);
        HDC memory = screen ? CreateCompatibleDC(screen) : nullptr;
        HBITMAP bitmap = screen ? CreateCompatibleBitmap(screen, width, height) : nullptr;
        if (!screen || !memory || !bitmap) {
            if (bitmap) DeleteObject(bitmap);
            if (memory) DeleteDC(memory);
            if (screen) ReleaseDC(nullptr, screen);
            return false;
        }
        HGDIOBJ old = SelectObject(memory, bitmap);
        const bool copied = BitBlt(memory, 0, 0, width, height, screen, rect.left,
                                   rect.top, SRCCOPY | CAPTUREBLT) != FALSE;
        SelectObject(memory, old);
        if (copied) {
            BITMAPINFO info{};
            info.bmiHeader.biSize = sizeof(info.bmiHeader);
            info.bmiHeader.biWidth = width;
            info.bmiHeader.biHeight = -height;
            info.bmiHeader.biPlanes = 1;
            info.bmiHeader.biBitCount = 32;
            info.bmiHeader.biCompression = BI_RGB;
            pixels.resize(static_cast<std::size_t>(width) * height * 4);
            GetDIBits(memory, bitmap, 0, static_cast<UINT>(height), pixels.data(),
                      &info, DIB_RGB_COLORS);
        }
        DeleteObject(bitmap);
        DeleteDC(memory);
        ReleaseDC(nullptr, screen);
        return copied;
    }

    // Encodes the BGRA frame at outputWidth x outputHeight; a size different
    // from the frame is resampled with WIC Fant.
    bool encode(const std::vector<std::uint8_t>& pixels, int width, int height,
                const std::string& path, const std::string& format, int quality,
                int outputWidth, int outputHeight) {
        ensure_wic();
        ComPtr<IWICBitmap> source;
        const UINT bytes = static_cast<UINT>(pixels.size());
        winrt::check_hresult(wic->CreateBitmapFromMemory(
            static_cast<UINT>(width), static_cast<UINT>(height), GUID_WICPixelFormat32bppBGRA,
            static_cast<UINT>(width * 4), bytes,
            const_cast<BYTE*>(reinterpret_cast<const BYTE*>(pixels.data())), &source));

        IWICBitmapSource* image = source.Get();
        ComPtr<IWICBitmapScaler> scaler;
        if (outputWidth != width || outputHeight != height) {
            winrt::check_hresult(wic->CreateBitmapScaler(&scaler));
            winrt::check_hresult(scaler->Initialize(source.Get(), outputWidth, outputHeight,
                                                    WICBitmapInterpolationModeFant));
            image = scaler.Get();
        }

        ComPtr<IWICStream> stream;
        winrt::check_hresult(wic->CreateStream(&stream));
        winrt::check_hresult(stream->InitializeFromFilename(wide(path).c_str(),
                                                             GENERIC_WRITE));
        ComPtr<IWICBitmapEncoder> encoder;
        winrt::check_hresult(wic->CreateEncoder(container_for(format), nullptr, &encoder));
        winrt::check_hresult(encoder->Initialize(stream.Get(), WICBitmapEncoderNoCache));
        ComPtr<IWICBitmapFrameEncode> frame;
        ComPtr<IPropertyBag2> properties;
        winrt::check_hresult(encoder->CreateNewFrame(&frame, &properties));
        if (format != "png" && properties) {
            PROPBAG2 property{};
            property.pstrName = const_cast<LPOLESTR>(L"ImageQuality");
            VARIANT value;
            VariantInit(&value);
            value.vt = VT_R4;
            value.fltVal = std::clamp(quality, 1, 100) / 100.0f;
            properties->Write(1, &property, &value);
            VariantClear(&value);
        }
        winrt::check_hresult(frame->Initialize(properties.Get()));
        winrt::check_hresult(frame->SetSize(static_cast<UINT>(outputWidth),
                                             static_cast<UINT>(outputHeight)));
        GUID pixelFormat = GUID_WICPixelFormat32bppBGRA;
        winrt::check_hresult(frame->SetPixelFormat(&pixelFormat));
        winrt::check_hresult(frame->WriteSource(image, nullptr));
        winrt::check_hresult(frame->Commit());
        winrt::check_hresult(encoder->Commit());
        return true;
    }
};

WindowCapture::WindowCapture() : impl_(new Impl()) {}

WindowCapture::~WindowCapture() { delete impl_; }

void WindowCapture::reset() noexcept {
    if (!impl_) return;
    impl_->minimumFrameTimestamp100ns = 0;
    impl_->reset_wgc();
}

void WindowCapture::mark_input_complete() noexcept {
    impl_->minimumFrameTimestamp100ns = qpc_time_100ns();
}

bool WindowCapture::wgc_available() const noexcept { return impl_ && impl_->wgcReady; }

CaptureResult WindowCapture::capture(void* rawHwnd, const RECT& windowRect,
                                     const std::string& fullPath,
                                     const std::string& reducedPath,
                                     const std::string& format, int quality,
                                     int maxEdge, Context& context) {
    if (!impl_ || !rawHwnd) throw Error("invalid_window", "A window handle is required");
    const HWND hwnd = static_cast<HWND>(rawHwnd);
    const int width = windowRect.right - windowRect.left;
    const int height = windowRect.bottom - windowRect.top;
    if (width <= 0 || height <= 0) throw Error("invalid_window", "Window has no area");

    context.check();
    std::vector<std::uint8_t> pixels;
    int sourceWidth = width;
    int sourceHeight = height;
    std::int64_t frameTimestamp100ns = 0;
    RoScope ro;
    bool usedWgc = ro.ok() && impl_->ensure_wgc(hwnd, width, height);
    bool freshFrame = false;
    bool cachedFrame = false;
    std::int64_t capturedAtUnixMs = 0;
    if (usedWgc) {
        Direct3D11CaptureFrame frame{nullptr};
        const auto minimumTimestamp = impl_->minimumFrameTimestamp100ns.load();
        if (minimumTimestamp != 0) {
            // SendInput returning does not mean the target has processed its
            // release event and painted. Give its UI and compositor two 60 Hz
            // frame intervals, while FrameArrived keeps the newest frame. This
            // is a bounded settling delay, not proof of application success.
            constexpr std::int64_t inputSettleIntervals100ns = 350000;
            while (qpc_time_100ns() < minimumTimestamp + inputSettleIntervals100ns) {
                context.check();
                Sleep(2);
            }
        }
        int waitMilliseconds = 0;
        if (!impl_->hasLastFrame) waitMilliseconds = 1200;
        else if (minimumTimestamp != 0) waitMilliseconds = 100;
        freshFrame = impl_->next_frame(context, frame, frameTimestamp100ns,
                                       waitMilliseconds, minimumTimestamp) &&
                     impl_->copy_frame(frame, pixels, sourceWidth, sourceHeight);
        if (frame) frame.Close();
        if (freshFrame) {
            impl_->lastPixels = pixels;
            impl_->lastWidth = sourceWidth;
            impl_->lastHeight = sourceHeight;
            impl_->lastTimestamp100ns = frameTimestamp100ns;
            constexpr std::int64_t intervalsPerMillisecond = 10000;
            const auto frameAgeMilliseconds = std::max<std::int64_t>(
                0, (qpc_time_100ns() - frameTimestamp100ns) / intervalsPerMillisecond);
            impl_->lastCapturedAtUnixMs = unix_time_milliseconds() - frameAgeMilliseconds;
            impl_->hasLastFrame = true;
            impl_->minimumFrameTimestamp100ns = 0;
        } else if (minimumTimestamp != 0) {
            throw Error("capture_not_ready", "No frame acquired after input; observe again");
        } else if (impl_->hasLastFrame) {
            // Static windows are not required to emit a new WGC frame for
            // every observation. Return the newest retained pixels immediately;
            // input actions separately require a frame newer than their completion.
            pixels = impl_->lastPixels;
            sourceWidth = impl_->lastWidth;
            sourceHeight = impl_->lastHeight;
            frameTimestamp100ns = impl_->lastTimestamp100ns;
            capturedAtUnixMs = impl_->lastCapturedAtUnixMs;
            cachedFrame = true;
        } else {
            usedWgc = false;
        }
    }
    if (!usedWgc) {
        context.check();
        if (!impl_->capture_gdi(hwnd, windowRect, pixels, sourceWidth, sourceHeight)) {
            throw Error("capture_failed", "Windows.Graphics.Capture and desktop capture failed");
        }
        capturedAtUnixMs = unix_time_milliseconds();
        impl_->minimumFrameTimestamp100ns = 0;
    }
    if (freshFrame) capturedAtUnixMs = impl_->lastCapturedAtUnixMs;
    context.check();
    // The reduced image halves frames larger than 1280x720 and keeps smaller
    // ones at 1x; a positive maxEdge further caps only the reduced image.
    double factor = sourceWidth > 1280 || sourceHeight > 720 ? 0.5 : 1.0;
    const int longestEdge = std::max(sourceWidth, sourceHeight);
    if (maxEdge > 0 && longestEdge * factor > maxEdge) {
        factor = static_cast<double>(maxEdge) / static_cast<double>(longestEdge);
    }
    const int reducedWidth = std::max(1, static_cast<int>(std::lround(sourceWidth * factor)));
    const int reducedHeight = std::max(1, static_cast<int>(std::lround(sourceHeight * factor)));
    if (!impl_->encode(pixels, sourceWidth, sourceHeight, fullPath, format, quality,
                       sourceWidth, sourceHeight)) {
        throw Error("encode_failed", "WIC could not encode the screenshot");
    }
    try {
        context.check();
        if (!impl_->encode(pixels, sourceWidth, sourceHeight, reducedPath, format, quality,
                           reducedWidth, reducedHeight)) {
            throw Error("encode_failed", "WIC could not encode the screenshot");
        }
    } catch (...) {
        std::error_code ignored;
        std::filesystem::remove(std::filesystem::path(wide(fullPath)), ignored);
        throw;
    }
    CaptureResult result;
    // WGC can report a frame size that differs from the Win32 outer rect by
    // invisible resize borders. Each image's transform is its pixel size over
    // the window rect, so that ratio is folded in.
    result.full = {fullPath, sourceWidth, sourceHeight,
                   static_cast<double>(sourceWidth) / width,
                   static_cast<double>(sourceHeight) / height};
    result.reduced = {reducedPath, reducedWidth, reducedHeight,
                      static_cast<double>(reducedWidth) / width,
                      static_cast<double>(reducedHeight) / height};
    result.path = reducedPath;
    result.mimeType = format == "png" ? "image/png" : "image/jpeg";
    result.width = reducedWidth;
    result.height = reducedHeight;
    result.scaleX = result.reduced.scaleX;
    result.scaleY = result.reduced.scaleY;
    result.scale = result.scaleX;
    result.backend = usedWgc ? "windows-graphics-capture" : "gdi-visible-desktop-fallback";
    result.frameTimestamp100ns = frameTimestamp100ns;
    result.capturedAtUnixMs = capturedAtUnixMs;
    result.freshFrame = freshFrame;
    result.cachedFrame = cachedFrame;
    result.sourceWidth = sourceWidth;
    result.sourceHeight = sourceHeight;
    return result;
}

} // namespace dcu::windows
