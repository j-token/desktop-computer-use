#include "indicator.hpp"

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>

#include <algorithm>
#include <cstdint>
#include <cstring>

namespace dcu::windows {
namespace {

constexpr wchar_t kWindowClass[] = L"DesktopComputerUse.SessionIndicator.1";
constexpr UINT kTimerId = 1;
constexpr UINT kEscapeMessage = WM_APP + 0x51;
constexpr LONG_PTR kCursorWindowId = 100;
constexpr int kEdgeThickness = 24;

constexpr wchar_t kBannerText[] = L"Computer use active  |  Esc ×2 to stop";
constexpr wchar_t kEscapeAgainText[] = L"Press Esc again to stop";

std::atomic<SessionIndicator*> activeIndicator{nullptr};
std::atomic_bool escapeDown{false};
// Hook point for modifier toggles: while set, a physical Escape is still
// counted toward the emergency stop but is not passed on to applications, so
// a toggled Ctrl or Win cannot turn it into an OS shortcut. Set through
// SessionIndicator::set_swallow_user_escape; nothing enables it yet.
std::atomic_bool swallowUserEscape{false};

struct Dib {
    HDC dc = nullptr;
    HBITMAP bitmap = nullptr;
    HBITMAP oldBitmap = nullptr;
    void* bits = nullptr;
    int width = 0;
    int height = 0;

    ~Dib() {
        if (dc && oldBitmap) SelectObject(dc, oldBitmap);
        if (bitmap) DeleteObject(bitmap);
        if (dc) DeleteDC(dc);
    }
};

void put_pixel(std::uint8_t* pixels, int width, int height, int x, int y,
               COLORREF color, std::uint8_t alpha) {
    if (x < 0 || y < 0 || x >= width || y >= height || alpha == 0) return;
    const auto r = static_cast<std::uint8_t>(GetRValue(color));
    const auto g = static_cast<std::uint8_t>(GetGValue(color));
    const auto b = static_cast<std::uint8_t>(GetBValue(color));
    auto* p = pixels + (static_cast<std::size_t>(y) * width + x) * 4;
    // UpdateLayeredWindow requires premultiplied BGRA pixels.
    p[0] = static_cast<std::uint8_t>((static_cast<unsigned>(b) * alpha) / 255U);
    p[1] = static_cast<std::uint8_t>((static_cast<unsigned>(g) * alpha) / 255U);
    p[2] = static_cast<std::uint8_t>((static_cast<unsigned>(r) * alpha) / 255U);
    p[3] = alpha;
}

void fill_rect(std::uint8_t* pixels, int width, int height, RECT rect,
               COLORREF color, std::uint8_t alpha) {
    rect.left = std::max<LONG>(rect.left, 0);
    rect.top = std::max<LONG>(rect.top, 0);
    rect.right = std::min<LONG>(rect.right, width);
    rect.bottom = std::min<LONG>(rect.bottom, height);
    for (LONG y = rect.top; y < rect.bottom; ++y) {
        for (LONG x = rect.left; x < rect.right; ++x) {
            put_pixel(pixels, width, height, static_cast<int>(x), static_cast<int>(y),
                      color, alpha);
        }
    }
}

bool make_dib(int width, int height, Dib& result) {
    HDC screen = GetDC(nullptr);
    if (!screen) return false;
    result.dc = CreateCompatibleDC(screen);

    BITMAPINFO info{};
    info.bmiHeader.biSize = sizeof(info.bmiHeader);
    info.bmiHeader.biWidth = width;
    info.bmiHeader.biHeight = -height; // top-down DIB
    info.bmiHeader.biPlanes = 1;
    info.bmiHeader.biBitCount = 32;
    info.bmiHeader.biCompression = BI_RGB;
    result.bitmap = CreateDIBSection(screen, &info, DIB_RGB_COLORS, &result.bits,
                                     nullptr, 0);
    ReleaseDC(nullptr, screen);
    if (!result.dc || !result.bitmap || !result.bits) return false;
    result.oldBitmap = static_cast<HBITMAP>(SelectObject(result.dc, result.bitmap));
    result.width = width;
    result.height = height;
    std::memset(result.bits, 0, static_cast<std::size_t>(width) * height * 4);
    return true;
}

void make_pixels_opaque(std::uint8_t* pixels, int width, int height) {
    // GDI does not set alpha in a DIB when drawing text and lines. Only the
    // small indicator surface is scanned, so this keeps the layered window
    // correct without touching the whole virtual desktop.
    for (std::size_t i = 0; i < static_cast<std::size_t>(width) * height; ++i) {
        auto* pixel = pixels + i * 4;
        if (pixel[3] == 0 && (pixel[0] != 0 || pixel[1] != 0 || pixel[2] != 0)) {
            pixel[3] = 255;
        }
    }
}

void update_layered_window(HWND hwnd, Dib& dib, POINT destination) {
    POINT source{0, 0};
    SIZE size{dib.width, dib.height};
    BLENDFUNCTION blend{AC_SRC_OVER, 0, 255, AC_SRC_ALPHA};
    HDC screen = GetDC(nullptr);
    if (!screen) return;
    UpdateLayeredWindow(hwnd, screen, &destination, &size, dib.dc, &source, 0,
                        &blend, ULW_ALPHA);
    ReleaseDC(nullptr, screen);
}

BOOL CALLBACK monitor_rect_proc(HMONITOR monitor, HDC, LPRECT, LPARAM data) {
    auto* rectangles = reinterpret_cast<std::vector<RECT>*>(data);
    MONITORINFO info{sizeof(info)};
    if (GetMonitorInfoW(monitor, &info)) rectangles->push_back(info.rcMonitor);
    return TRUE;
}

std::vector<RECT> monitor_rectangles() {
    std::vector<RECT> rectangles;
    EnumDisplayMonitors(nullptr, nullptr, monitor_rect_proc,
                        reinterpret_cast<LPARAM>(&rectangles));
    return rectangles;
}

RECT edge_rect(const RECT& monitor, int side) {
    RECT result = monitor;
    switch (side) {
    case 0:
        result.bottom = std::min(result.bottom, result.top + kEdgeThickness);
        break;
    case 1:
        result.top = std::max(result.top, result.bottom - kEdgeThickness);
        break;
    case 2:
        result.right = std::min(result.right, result.left + kEdgeThickness);
        break;
    case 3:
        result.left = std::max(result.left, result.right - kEdgeThickness);
        break;
    default:
        result = RECT{};
        break;
    }
    return result;
}

void paint_edge(HWND hwnd, const RECT& monitor, int side) {
    const RECT geometry = edge_rect(monitor, side);
    const int width = geometry.right - geometry.left;
    const int height = geometry.bottom - geometry.top;
    if (width <= 0 || height <= 0) return;

    const POINT destination{geometry.left, geometry.top};
    SetWindowPos(hwnd, HWND_TOPMOST, destination.x, destination.y, width, height,
                 SWP_NOACTIVATE | SWP_SHOWWINDOW);

    Dib dib;
    if (!make_dib(width, height, dib)) return;
    auto* pixels = static_cast<std::uint8_t*>(dib.bits);
    for (int distance = 0; distance < kEdgeThickness; ++distance) {
        const auto alpha = static_cast<std::uint8_t>(
            (220 * (kEdgeThickness - distance)) / kEdgeThickness);
        RECT stripe{0, 0, width, height};
        if (side == 0) {
            stripe.top = distance;
            stripe.bottom = distance + 1;
        } else if (side == 1) {
            stripe.top = height - distance - 1;
            stripe.bottom = stripe.top + 1;
        } else if (side == 2) {
            stripe.left = distance;
            stripe.right = distance + 1;
        } else {
            stripe.left = width - distance - 1;
            stripe.right = stripe.left + 1;
        }
        fill_rect(pixels, width, height, stripe, RGB(42, 132, 255), alpha);
    }
    update_layered_window(hwnd, dib, destination);
}

LRESULT CALLBACK low_level_keyboard_proc(int code, WPARAM message, LPARAM data) {
    if (code == HC_ACTION && data) {
        const auto* keyboard = reinterpret_cast<const KBDLLHOOKSTRUCT*>(data);
        const bool injected =
            (keyboard->flags & (LLKHF_INJECTED | LLKHF_LOWER_IL_INJECTED)) != 0;
        if (keyboard->vkCode == VK_ESCAPE && !injected) {
            if (message == WM_KEYDOWN || message == WM_SYSKEYDOWN) {
                if (!escapeDown.exchange(true, std::memory_order_acq_rel)) {
                    if (auto* indicator = activeIndicator.load(std::memory_order_acquire)) {
                        indicator->post_escape_message(keyboard->time);
                    }
                }
            } else if (message == WM_KEYUP || message == WM_SYSKEYUP) {
                escapeDown.store(false, std::memory_order_release);
            }
            if (swallowUserEscape.load(std::memory_order_acquire)) return 1;
        }
    }
    return CallNextHookEx(nullptr, code, message, data);
}

void paint_banner(HWND hwnd, const std::wstring& label) {
    constexpr int width = 420;
    constexpr int height = 72;
    RECT virtualRect{
        GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN),
        GetSystemMetrics(SM_XVIRTUALSCREEN) + GetSystemMetrics(SM_CXVIRTUALSCREEN),
        GetSystemMetrics(SM_YVIRTUALSCREEN) + GetSystemMetrics(SM_CYVIRTUALSCREEN)};
    if (virtualRect.right <= virtualRect.left || virtualRect.bottom <= virtualRect.top) return;

    const POINT destination{virtualRect.left + 24, virtualRect.top + 20};
    SetWindowPos(hwnd, HWND_TOPMOST, destination.x, destination.y, width, height,
                 SWP_NOACTIVATE | SWP_SHOWWINDOW);

    Dib dib;
    if (!make_dib(width, height, dib)) return;
    auto* pixels = static_cast<std::uint8_t*>(dib.bits);
    fill_rect(pixels, width, height, RECT{0, 0, width, height}, RGB(18, 24, 38), 232);
    fill_rect(pixels, width, height, RECT{0, 0, 4, height}, RGB(63, 164, 255), 255);

    SetBkMode(dib.dc, TRANSPARENT);
    SetTextColor(dib.dc, RGB(245, 248, 255));
    HFONT font = CreateFontW(-18, 0, 0, 0, FW_SEMIBOLD, FALSE, FALSE, FALSE,
                             DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS,
                             CLEARTYPE_QUALITY, DEFAULT_PITCH | FF_DONTCARE,
                             L"Segoe UI");
    if (font) {
        HGDIOBJ oldFont = SelectObject(dib.dc, font);
        TextOutW(dib.dc, 18, 24, label.c_str(), static_cast<int>(label.size()));
        SelectObject(dib.dc, oldFont);
        DeleteObject(font);
    }

    make_pixels_opaque(pixels, width, height);
    update_layered_window(hwnd, dib, destination);
}

void paint_cursor(HWND hwnd) {
    constexpr int size = 56;
    POINT cursor{};
    if (!GetCursorPos(&cursor)) return;
    const POINT destination{cursor.x - size / 2, cursor.y - size / 2};
    SetWindowPos(hwnd, HWND_TOPMOST, destination.x, destination.y, size, size,
                 SWP_NOACTIVATE | SWP_SHOWWINDOW);

    Dib dib;
    if (!make_dib(size, size, dib)) return;
    auto* pixels = static_cast<std::uint8_t*>(dib.bits);
    const int center = size / 2;
    HPEN ringPen = CreatePen(PS_SOLID, 3, RGB(54, 205, 255));
    if (ringPen) {
        HGDIOBJ oldPen = SelectObject(dib.dc, ringPen);
        HGDIOBJ oldBrush = SelectObject(dib.dc, GetStockObject(NULL_BRUSH));
        Ellipse(dib.dc, center - 14, center - 14, center + 15, center + 15);
        SelectObject(dib.dc, oldBrush);
        SelectObject(dib.dc, oldPen);
        DeleteObject(ringPen);
    }
    POINT arrow[]{{center - 1, center - 4}, {center - 1, center + 14},
                  {center + 4, center + 9}, {center + 10, center + 16},
                  {center + 14, center + 12}, {center + 8, center + 5},
                  {center + 15, center + 5}};
    HBRUSH arrowBrush = CreateSolidBrush(RGB(255, 255, 255));
    if (arrowBrush) {
        HGDIOBJ oldBrush = SelectObject(dib.dc, arrowBrush);
        Polygon(dib.dc, arrow, static_cast<int>(std::size(arrow)));
        SelectObject(dib.dc, oldBrush);
        DeleteObject(arrowBrush);
    }

    make_pixels_opaque(pixels, size, size);
    update_layered_window(hwnd, dib, destination);
}

LRESULT CALLBACK indicator_wnd_proc(HWND hwnd, UINT message, WPARAM wParam,
                                    LPARAM lParam) {
    auto* indicator = reinterpret_cast<SessionIndicator*>(
        GetWindowLongPtrW(hwnd, GWLP_USERDATA));
    switch (message) {
    case WM_NCCREATE: {
        auto* create = reinterpret_cast<CREATESTRUCTW*>(lParam);
        indicator = static_cast<SessionIndicator*>(create->lpCreateParams);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA,
                          reinterpret_cast<LONG_PTR>(indicator));
        return TRUE;
    }
    case WM_NCHITTEST:
        return HTTRANSPARENT;
    case WM_MOUSEACTIVATE:
        return MA_NOACTIVATE;
    case kEscapeMessage:
        if (indicator) indicator->handle_escape(static_cast<std::uint32_t>(wParam));
        return 0;
    case WM_TIMER:
        if (wParam == kTimerId && GetWindowLongPtrW(hwnd, GWLP_ID) == 0) {
            if (indicator) indicator->repaint();
            return 0;
        }
        break;
    case WM_DISPLAYCHANGE:
        // The system broadcasts this message to each overlay window. Let the
        // banner perform the rebuild so it cannot destroy the edge window
        // whose callback is currently running.
        if (indicator && GetWindowLongPtrW(hwnd, GWLP_ID) == 0) indicator->repaint();
        return 0;
    case WM_CLOSE:
        DestroyWindow(hwnd);
        return 0;
    case WM_DESTROY:
        KillTimer(hwnd, kTimerId);
        // Only the banner owns the message loop. Cursor, edge, and monitor
        // rebuild windows can be destroyed independently without ending it.
        if (GetWindowLongPtrW(hwnd, GWLP_ID) == 0) PostQuitMessage(0);
        return 0;
    default:
        break;
    }
    return DefWindowProcW(hwnd, message, wParam, lParam);
}

} // namespace

SessionIndicator::~SessionIndicator() { stop(); }

bool SessionIndicator::start(std::function<void()> onStop, std::string& error) {
    stop();
    {
        std::lock_guard lock(stateMutex_);
        onStop_ = std::move(onStop);
        threadExited_ = false;
        startOk_ = false;
        startError_.clear();
    }
    thread_ = std::thread([this] {
        thread_main(onStop_);
    });

    std::unique_lock lock(stateMutex_);
    stateCv_.wait(lock, [this] { return ready_.load() || threadExited_; });
    if (!startOk_) {
        error = startError_.empty() ? "Unable to create session indicator" : startError_;
        lock.unlock();
        stop();
        return false;
    }
    return true;
}

void SessionIndicator::stop() noexcept {
    HWND hwnd = nullptr;
    {
        std::lock_guard lock(stateMutex_);
        hwnd = static_cast<HWND>(window_);
    }
    if (hwnd) PostMessageW(hwnd, WM_CLOSE, 0, 0);
    if (thread_.joinable() && thread_.get_id() != std::this_thread::get_id()) {
        thread_.join();
    }
    running_.store(false, std::memory_order_release);
    ready_.store(false, std::memory_order_release);
}

void SessionIndicator::post_escape_message(std::uint32_t eventTimeMs) noexcept {
    HWND hwnd = nullptr;
    {
        std::lock_guard lock(stateMutex_);
        hwnd = static_cast<HWND>(window_);
    }
    if (hwnd) PostMessageW(hwnd, kEscapeMessage, static_cast<WPARAM>(eventTimeMs), 0);
}

void SessionIndicator::show_transient_text(std::wstring text,
                                           std::chrono::milliseconds duration) {
    std::lock_guard lock(stateMutex_);
    transientText_ = std::move(text);
    transientUntil_ = std::chrono::steady_clock::now() + duration;
}

void SessionIndicator::set_swallow_user_escape(bool enabled) noexcept {
    swallowUserEscape.store(enabled, std::memory_order_release);
}

std::wstring SessionIndicator::banner_text() {
    std::lock_guard lock(stateMutex_);
    if (!transientText_.empty() && std::chrono::steady_clock::now() < transientUntil_) {
        return transientText_;
    }
    transientText_.clear();
    return kBannerText;
}

void SessionIndicator::request_stop_async() noexcept {
    HWND hwnd = nullptr;
    {
        std::lock_guard lock(stateMutex_);
        hwnd = static_cast<HWND>(window_);
    }
    if (hwnd) PostMessageW(hwnd, WM_CLOSE, 0, 0);
}

bool SessionIndicator::rebuild_edges() noexcept {
    const auto monitors = monitor_rectangles();
    if (monitors.empty()) return false;

    std::vector<HWND> previous;
    {
        std::lock_guard lock(stateMutex_);
        previous.reserve(edgeWindows_.size());
        for (void* value : edgeWindows_) previous.push_back(reinterpret_cast<HWND>(value));
        edgeWindows_.clear();
    }
    for (HWND edge : previous) {
        if (edge && IsWindow(edge)) DestroyWindow(edge);
    }

    const DWORD overlayStyle = WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE |
                               WS_EX_TOOLWINDOW | WS_EX_TOPMOST;
    const HINSTANCE instance = GetModuleHandleW(nullptr);
    std::vector<HWND> created;
    created.reserve(monitors.size() * 4);
    for (const RECT& monitor : monitors) {
        for (int side = 0; side < 4; ++side) {
            const RECT geometry = edge_rect(monitor, side);
            const int width = geometry.right - geometry.left;
            const int height = geometry.bottom - geometry.top;
            if (width <= 0 || height <= 0) {
                for (HWND window : created) DestroyWindow(window);
                return false;
            }
            HWND edge = CreateWindowExW(
                overlayStyle, kWindowClass, L"Desktop computer use edge", WS_POPUP,
                geometry.left, geometry.top, width, height, nullptr, nullptr, instance, this);
            if (!edge) {
                for (HWND window : created) DestroyWindow(window);
                return false;
            }
            SetWindowLongPtrW(edge, GWLP_ID,
                              static_cast<LONG_PTR>(created.size() + 1));
            created.push_back(edge);
        }
    }

    {
        std::lock_guard lock(stateMutex_);
        edgeWindows_.reserve(created.size());
        for (HWND edge : created) edgeWindows_.push_back(reinterpret_cast<void*>(edge));
    }
    return true;
}

void SessionIndicator::repaint() noexcept {
    HWND banner = nullptr;
    HWND cursor = nullptr;
    std::vector<HWND> edges;
    {
        std::lock_guard lock(stateMutex_);
        banner = static_cast<HWND>(window_);
        cursor = static_cast<HWND>(cursorWindow_);
        edges.reserve(edgeWindows_.size());
        for (void* value : edgeWindows_) edges.push_back(reinterpret_cast<HWND>(value));
    }
    if (!banner || !cursor) return;
    const auto monitors = monitor_rectangles();
    if (monitors.empty()) return;
    if (edges.size() != monitors.size() * 4) {
        if (!rebuild_edges()) return;
        std::lock_guard lock(stateMutex_);
        edges.clear();
        edges.reserve(edgeWindows_.size());
        for (void* value : edgeWindows_) edges.push_back(reinterpret_cast<HWND>(value));
    }
    try {
        paint_banner(banner, banner_text());
        paint_cursor(cursor);
        for (std::size_t monitorIndex = 0; monitorIndex < monitors.size(); ++monitorIndex) {
            for (int side = 0; side < 4; ++side) {
                const std::size_t edgeIndex = monitorIndex * 4 + side;
                if (edgeIndex < edges.size()) paint_edge(edges[edgeIndex], monitors[monitorIndex], side);
            }
        }
    } catch (...) {
        // Keep the indicator thread alive if a driver wraps a GDI failure in
        // an exception. The next timer tick retries the small surfaces.
    }
}

void SessionIndicator::invoke_stop_callback() noexcept {
    if (onStop_) {
        try {
            onStop_();
        } catch (...) {
            // Escape is an asynchronous emergency stop. Never allow an
            // exception from the callback to tear down the message loop.
        }
    }
}

void SessionIndicator::handle_escape(std::uint32_t eventTimeMs) noexcept {
    // Unsigned subtraction stays correct across the 49.7-day tick wrap.
    if (escapeArmed_ && eventTimeMs - firstEscapeTimeMs_ <= kDoubleEscapeWindowMs) {
        escapeArmed_ = false;
        invoke_stop_callback();
        request_stop_async();
        return;
    }
    // A single press, or one after the window expired, only arms the stop.
    escapeArmed_ = true;
    firstEscapeTimeMs_ = eventTimeMs;
    try {
        show_transient_text(kEscapeAgainText,
                            std::chrono::milliseconds(kDoubleEscapeWindowMs));
    } catch (...) {
        // The hint is cosmetic; the double-press window still applies.
    }
    repaint();
}

void SessionIndicator::thread_main(std::function<void()> onStop) {
    onStop_ = std::move(onStop);
    escapeArmed_ = false;
    {
        std::lock_guard lock(stateMutex_);
        transientText_.clear();
    }

    SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    WNDCLASSEXW klass{};
    klass.cbSize = sizeof(klass);
    klass.hInstance = GetModuleHandleW(nullptr);
    klass.lpfnWndProc = indicator_wnd_proc;
    klass.lpszClassName = kWindowClass;
    klass.hCursor = LoadCursorW(nullptr, IDC_ARROW);
    const ATOM registeredClass = RegisterClassExW(&klass);
    const DWORD registrationError = registeredClass ? ERROR_SUCCESS : GetLastError();
    if (!registeredClass && registrationError != ERROR_CLASS_ALREADY_EXISTS) {
        std::lock_guard lock(stateMutex_);
        startError_ = "RegisterClassExW failed: " + std::to_string(registrationError);
        threadExited_ = true;
        stateCv_.notify_all();
        return;
    }

    const DWORD overlayStyle = WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE |
                               WS_EX_TOOLWINDOW | WS_EX_TOPMOST;
    auto destroy_edges = [this]() noexcept {
        std::vector<HWND> edges;
        {
            std::lock_guard lock(stateMutex_);
            edges.reserve(edgeWindows_.size());
            for (void* value : edgeWindows_) edges.push_back(reinterpret_cast<HWND>(value));
            edgeWindows_.clear();
        }
        for (HWND edge : edges) {
            if (edge && IsWindow(edge)) DestroyWindow(edge);
        }
    };
    auto fail_start = [&](std::string message, HWND banner, HWND cursor,
                          HHOOK keyboardHook) noexcept {
        SessionIndicator* expected = this;
        activeIndicator.compare_exchange_strong(expected, nullptr,
                                                std::memory_order_acq_rel);
        if (keyboardHook) UnhookWindowsHookEx(keyboardHook);
        destroy_edges();
        if (cursor && IsWindow(cursor)) DestroyWindow(cursor);
        if (banner && IsWindow(banner)) DestroyWindow(banner);
        {
            std::lock_guard lock(stateMutex_);
            window_ = nullptr;
            cursorWindow_ = nullptr;
            keyboardHook_ = nullptr;
            startOk_ = false;
            startError_ = std::move(message);
            threadExited_ = true;
        }
        running_.store(false, std::memory_order_release);
        ready_.store(false, std::memory_order_release);
        stateCv_.notify_all();
        UnregisterClassW(kWindowClass, klass.hInstance);
    };

    HWND hwnd = CreateWindowExW(
        overlayStyle, kWindowClass, L"Desktop computer use", WS_POPUP, 0, 0, 420, 72, nullptr,
        nullptr, klass.hInstance, this);
    if (!hwnd) {
        fail_start("CreateWindowExW failed: " + std::to_string(GetLastError()), nullptr,
                   nullptr, nullptr);
        return;
    }
    HWND cursor = CreateWindowExW(
        overlayStyle, kWindowClass, L"Desktop computer use cursor", WS_POPUP, 0, 0, 56, 56,
        nullptr, nullptr, klass.hInstance, this);
    if (!cursor) {
        const auto code = GetLastError();
        fail_start("CreateWindowExW for cursor failed: " + std::to_string(code), hwnd,
                   nullptr, nullptr);
        return;
    }
    SetWindowLongPtrW(cursor, GWLP_ID, kCursorWindowId);
    if (!rebuild_edges()) {
        fail_start("Unable to create a click-through edge on every monitor", hwnd, cursor,
                   nullptr);
        return;
    }

    HHOOK keyboardHook = SetWindowsHookExW(
        WH_KEYBOARD_LL, low_level_keyboard_proc, klass.hInstance, 0);
    if (!keyboardHook) {
        const auto code = GetLastError();
        fail_start("SetWindowsHookExW(WH_KEYBOARD_LL) failed: " + std::to_string(code),
                   hwnd, cursor, nullptr);
        return;
    }
    if (!SetTimer(hwnd, kTimerId, 33, nullptr)) {
        const auto code = GetLastError();
        fail_start("SetTimer failed: " + std::to_string(code), hwnd, cursor, keyboardHook);
        return;
    }

    {
        std::lock_guard lock(stateMutex_);
        window_ = hwnd;
        cursorWindow_ = cursor;
        keyboardHook_ = keyboardHook;
        startOk_ = true;
        ready_.store(true, std::memory_order_release);
        running_.store(true, std::memory_order_release);
    }
    escapeDown.store(false, std::memory_order_release);
    activeIndicator.store(this, std::memory_order_release);
    stateCv_.notify_all();
    repaint();

    MSG message{};
    while (GetMessageW(&message, nullptr, 0, 0) > 0) {
        TranslateMessage(&message);
        DispatchMessageW(&message);
    }

    SessionIndicator* expected = this;
    activeIndicator.compare_exchange_strong(expected, nullptr,
                                            std::memory_order_acq_rel);
    HHOOK installedHook = nullptr;
    {
        std::lock_guard lock(stateMutex_);
        installedHook = static_cast<HHOOK>(keyboardHook_);
        keyboardHook_ = nullptr;
    }
    if (installedHook) UnhookWindowsHookEx(installedHook);
    destroy_edges();
    if (cursor) DestroyWindow(cursor);
    if (hwnd && IsWindow(hwnd)) DestroyWindow(hwnd);

    {
        std::lock_guard lock(stateMutex_);
        window_ = nullptr;
        cursorWindow_ = nullptr;
        threadExited_ = true;
    }
    running_.store(false, std::memory_order_release);
    ready_.store(false, std::memory_order_release);
    stateCv_.notify_all();
    UnregisterClassW(kWindowClass, klass.hInstance);
}

} // namespace dcu::windows
