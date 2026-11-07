//! Persistent Windows.Graphics.Capture + D3D11 frame-pool capture. One frame
//! pool is reused for successive observations of the same window, and a
//! visible desktop BitBlt is used only when WGC cannot be created (for
//! example on an unsupported pre-1903 host).

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dcu_core::{Context, Error};
use parking_lot::{Condvar, Mutex};
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Foundation::{GENERIC_WRITE, HMODULE, HWND, RECT};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP, D3D_FEATURE_LEVEL,
    D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleBitmap,
    CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC,
    SRCCOPY, SelectObject,
};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, CLSID_WICImagingFactory2, GUID_ContainerFormatJpeg,
    GUID_ContainerFormatPng, GUID_WICPixelFormat32bppBGRA, IWICBitmapFrameEncode, IWICBitmapSource,
    IWICImagingFactory, WICBitmapEncoderNoCache, WICBitmapInterpolationModeFant,
};
use windows::Win32::System::Com::StructuredStorage::{IPropertyBag2, PROPBAG2};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize};
use windows::Win32::UI::WindowsAndMessaging::IsWindow;
use windows::core::{GUID, HSTRING, IInspectable, Interface, w};

const INTERVALS_100NS_PER_SECOND: f64 = 10_000_000.0;
const INTERVALS_100NS_PER_MILLISECOND: i64 = 10_000;
// SendInput returning does not mean the target has processed its release
// event and painted. Give its UI and compositor two 60 Hz frame intervals,
// while FrameArrived keeps the newest frame. This is a bounded settling
// delay, not proof of application success.
const INPUT_SETTLE_100NS: i64 = 350_000;
const FIRST_FRAME_WAIT_MS: u64 = 1200;
const POST_INPUT_FRAME_WAIT_MS: u64 = 100;
const FRAME_WAIT_SLICE: Duration = Duration::from_millis(30);
const FRAME_POOL_BUFFERS: i32 = 2;

/// One encoded image of a captured frame. `scale_x`/`scale_y` map
/// window-local coordinates to this image's pixels: pixel = window * scale.
#[derive(Clone, Debug, Default)]
pub(crate) struct CapturedImage {
    pub(crate) path: String,
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) scale_x: f64,
    pub(crate) scale_y: f64,
}

/// `path`, `width`, `height`, `scale`, `scale_x`, and `scale_y` describe the reduced image.
pub(crate) struct CaptureResult {
    pub(crate) full: CapturedImage,
    pub(crate) reduced: CapturedImage,
    pub(crate) path: String,
    pub(crate) mime_type: &'static str,
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) scale: f64,
    pub(crate) backend: &'static str,
    pub(crate) frame_timestamp_100ns: i64,
    pub(crate) captured_at_unix_ms: i64,
    pub(crate) fresh_frame: bool,
    pub(crate) cached_frame: bool,
    pub(crate) source_width: i32,
    pub(crate) source_height: i32,
    pub(crate) scale_x: f64,
    pub(crate) scale_y: f64,
}

pub(crate) struct CaptureRequest<'a> {
    pub(crate) full_path: &'a str,
    pub(crate) reduced_path: &'a str,
    pub(crate) format: &'a str,
    pub(crate) quality: i32,
    pub(crate) max_edge: i32,
}

fn unix_time_milliseconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

/// WGC SystemRelativeTime uses the same QPC epoch in 100 ns intervals.
fn qpc_time_100ns() -> i64 {
    let mut counter = 0i64;
    let mut frequency = 0i64;
    unsafe {
        let _ = QueryPerformanceCounter(&mut counter);
        let _ = QueryPerformanceFrequency(&mut frequency);
    }
    (counter as f64 * INTERVALS_100NS_PER_SECOND / frequency as f64) as i64
}

struct RuntimeScope {
    initialized: bool,
}

impl RuntimeScope {
    fn enter() -> Self {
        Self {
            initialized: unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.is_ok(),
        }
    }
}

impl Drop for RuntimeScope {
    fn drop(&mut self) {
        if self.initialized {
            unsafe { RoUninitialize() };
        }
    }
}

#[derive(Default)]
struct FrameState {
    latest: Option<Direct3D11CaptureFrame>,
    signalled: bool,
}

#[derive(Default)]
struct FrameSlot {
    state: Mutex<FrameState>,
    arrived: Condvar,
}

impl FrameSlot {
    fn clear(&self) {
        let mut state = self.state.lock();
        if let Some(frame) = state.latest.take() {
            let _ = frame.Close();
        }
        state.signalled = false;
    }
}

struct Staging {
    texture: ID3D11Texture2D,
    width: u32,
    height: u32,
}

/// A started capture session of one window at one size.
struct WgcSession {
    device: ID3D11Device,
    device_context: ID3D11DeviceContext,
    // Staging textures belong to their creating D3D device; a restarted
    // capture creates a new device even when the window size is unchanged.
    staging: Option<Staging>,
    // Kept alive for as long as the session captures it.
    _item: GraphicsCaptureItem,
    frame_pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    frame_token: i64,
    target: HWND,
    width: i32,
    height: i32,
    frames: Arc<FrameSlot>,
}

impl Drop for WgcSession {
    fn drop(&mut self) {
        let _ = self.frame_pool.RemoveFrameArrived(self.frame_token);
        let _ = self.session.Close();
        let _ = self.frame_pool.Close();
        self.frames.clear();
    }
}

struct RetainedFrame {
    pixels: Vec<u8>,
    width: i32,
    height: i32,
    timestamp_100ns: i64,
    captured_at_unix_ms: i64,
}

struct Frame {
    pixels: Vec<u8>,
    width: i32,
    height: i32,
}

pub(crate) struct WindowCapture {
    wic: Option<IWICImagingFactory>,
    wgc: Option<WgcSession>,
    last_frame: Option<RetainedFrame>,
    /// Following observations must use a frame acquired after this QPC time; 0 when none is required.
    minimum_frame_timestamp_100ns: i64,
}

fn create_device(driver_type: D3D_DRIVER_TYPE) -> Option<(ID3D11Device, ID3D11DeviceContext)> {
    let feature_levels = [
        D3D_FEATURE_LEVEL_11_1,
        D3D_FEATURE_LEVEL_11_0,
        D3D_FEATURE_LEVEL_10_1,
    ];
    let mut device = None;
    let mut device_context = None;
    let mut selected = D3D_FEATURE_LEVEL::default();
    unsafe {
        D3D11CreateDevice(
            None,
            driver_type,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&feature_levels),
            D3D11_SDK_VERSION,
            Some(&mut device),
            Some(&mut selected),
            Some(&mut device_context),
        )
    }
    .ok()?;
    Some((device?, device_context?))
}

fn start_session(
    hwnd: HWND,
    width: i32,
    height: i32,
    device: ID3D11Device,
    device_context: ID3D11DeviceContext,
) -> windows::core::Result<Option<WgcSession>> {
    let dxgi_device: IDXGIDevice = device.cast()?;
    let direct_device: IDirect3DDevice =
        unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device) }?.cast()?;
    let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
    let item: GraphicsCaptureItem = unsafe { interop.CreateForWindow(hwnd) }?;
    let size = item.Size()?;
    if size.Width <= 0 || size.Height <= 0 {
        return Ok(None);
    }
    let frame_pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
        &direct_device,
        DirectXPixelFormat::B8G8R8A8UIntNormalized,
        FRAME_POOL_BUFFERS,
        size,
    )?;
    let session = frame_pool.CreateCaptureSession(&item)?;
    session.SetIsCursorCaptureEnabled(false)?;
    let frames = Arc::new(FrameSlot::default());
    let slot = frames.clone();
    let frame_token = frame_pool.FrameArrived(&TypedEventHandler::<
        Direct3D11CaptureFramePool,
        IInspectable,
    >::new(move |sender, _| {
        let Some(pool) = sender.as_ref() else {
            return Ok(());
        };
        {
            let mut state = slot.state.lock();
            // Drain continuously, including while a drag is executing.
            // Keeping only the newest frame avoids an early-action backlog.
            while let Ok(frame) = pool.TryGetNextFrame() {
                if let Some(previous) = state.latest.replace(frame) {
                    let _ = previous.Close();
                }
                state.signalled = true;
            }
        }
        slot.arrived.notify_one();
        Ok(())
    }))?;
    let started = WgcSession {
        device,
        device_context,
        staging: None,
        _item: item,
        frame_pool,
        session,
        frame_token,
        target: hwnd,
        width,
        height,
        frames,
    };
    started.session.StartCapture()?;
    Ok(Some(started))
}

impl WgcSession {
    /// The newest frame at or after `minimum_timestamp_100ns`, waiting up to
    /// `wait` for one to arrive.
    fn next_frame(
        &self,
        context: &Context,
        wait: Duration,
        minimum_timestamp_100ns: i64,
    ) -> Result<Option<(Direct3D11CaptureFrame, i64)>, Error> {
        let deadline = Instant::now() + wait;
        loop {
            context.check()?;
            let mut state = self.frames.state.lock();
            if let Some(frame) = state.latest.take() {
                let Ok(time) = frame.SystemRelativeTime() else {
                    return Ok(None);
                };
                state.signalled = false;
                if time.Duration >= minimum_timestamp_100ns {
                    return Ok(Some((frame, time.Duration)));
                }
                let _ = frame.Close();
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            let slice = FRAME_WAIT_SLICE.min(deadline - now);
            self.frames
                .arrived
                .wait_while_for(&mut state, |state| !state.signalled, slice);
        }
    }

    fn copy_frame(&mut self, frame: &Direct3D11CaptureFrame) -> Option<Frame> {
        let access: IDirect3DDxgiInterfaceAccess = frame.Surface().ok()?.cast().ok()?;
        let source: ID3D11Texture2D = unsafe { access.GetInterface() }.ok()?;
        let mut description = D3D11_TEXTURE2D_DESC::default();
        unsafe { source.GetDesc(&mut description) };
        let (width, height) = (description.Width, description.Height);
        if width == 0 || height == 0 || width > i32::MAX as u32 || height > i32::MAX as u32 {
            return None;
        }

        if !self
            .staging
            .as_ref()
            .is_some_and(|staging| staging.width == width && staging.height == height)
        {
            self.staging = None;
            let staging_description = D3D11_TEXTURE2D_DESC {
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
                ..description
            };
            let mut texture = None;
            unsafe {
                self.device
                    .CreateTexture2D(&staging_description, None, Some(&mut texture))
            }
            .ok()?;
            self.staging = Some(Staging {
                texture: texture?,
                width,
                height,
            });
        }
        let staging = &self.staging.as_ref()?.texture;
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            self.device_context.CopyResource(staging, &source);
            self.device_context
                .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .ok()?;
        }
        let row_bytes = width as usize * 4;
        let mut pixels = vec![0u8; row_bytes * height as usize];
        for (row, destination) in pixels.chunks_exact_mut(row_bytes).enumerate() {
            // SAFETY: the mapped subresource holds `height` rows of RowPitch
            // bytes, each starting with `row_bytes` bytes of pixels.
            let source_row = unsafe {
                std::slice::from_raw_parts(
                    mapped
                        .pData
                        .cast::<u8>()
                        .add(row * mapped.RowPitch as usize),
                    row_bytes,
                )
            };
            destination.copy_from_slice(source_row);
        }
        unsafe { self.device_context.Unmap(staging, 0) };
        Some(Frame {
            pixels,
            width: width as i32,
            height: height as i32,
        })
    }
}

fn capture_gdi(rect: &RECT) -> Option<Frame> {
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    if width <= 0 || height <= 0 {
        return None;
    }
    unsafe {
        let screen = GetDC(None);
        if screen.is_invalid() {
            return None;
        }
        let memory = CreateCompatibleDC(Some(screen));
        let bitmap = CreateCompatibleBitmap(screen, width, height);
        let mut frame = None;
        if !memory.is_invalid() && !bitmap.is_invalid() {
            let old = SelectObject(memory, bitmap.into());
            let copied = BitBlt(
                memory,
                0,
                0,
                width,
                height,
                Some(screen),
                rect.left,
                rect.top,
                SRCCOPY | CAPTUREBLT,
            )
            .is_ok();
            SelectObject(memory, old);
            if copied {
                let mut info = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: width,
                        biHeight: -height,
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: BI_RGB.0,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                let mut pixels = vec![0u8; width as usize * height as usize * 4];
                GetDIBits(
                    memory,
                    bitmap,
                    0,
                    height as u32,
                    Some(pixels.as_mut_ptr().cast()),
                    &mut info,
                    DIB_RGB_COLORS,
                );
                frame = Some(Frame {
                    pixels,
                    width,
                    height,
                });
            }
        }
        if !bitmap.is_invalid() {
            let _ = DeleteObject(bitmap.into());
        }
        if !memory.is_invalid() {
            let _ = DeleteDC(memory);
        }
        ReleaseDC(None, screen);
        frame
    }
}

fn container_format(format: &str) -> GUID {
    if format == "png" {
        GUID_ContainerFormatPng
    } else {
        GUID_ContainerFormatJpeg
    }
}

fn encode_failed() -> Error {
    Error::new("encode_failed", "WIC could not encode the screenshot")
}

fn remove_file(path: &str) {
    let _ = std::fs::remove_file(path);
}

impl WindowCapture {
    pub(crate) fn new() -> Self {
        Self {
            wic: None,
            wgc: None,
            last_frame: None,
            minimum_frame_timestamp_100ns: 0,
        }
    }

    /// Stops the persistent frame pool and releases its capture resources, so
    /// a stopped session neither keeps receiving frames nor retains a window.
    pub(crate) fn reset(&mut self) {
        self.minimum_frame_timestamp_100ns = 0;
        self.reset_wgc();
    }

    pub(crate) fn mark_input_complete(&mut self) {
        self.minimum_frame_timestamp_100ns = qpc_time_100ns();
    }

    fn reset_wgc(&mut self) {
        self.wgc = None;
        self.last_frame = None;
    }

    fn ensure_wgc(&mut self, hwnd: HWND, width: i32, height: i32) -> bool {
        if !unsafe { IsWindow(Some(hwnd)) }.as_bool() || width <= 0 || height <= 0 {
            return false;
        }
        if self
            .wgc
            .as_ref()
            .is_some_and(|wgc| wgc.target == hwnd && wgc.width == width && wgc.height == height)
        {
            return true;
        }
        self.reset_wgc();

        let Some((device, device_context)) =
            create_device(D3D_DRIVER_TYPE_HARDWARE).or_else(|| create_device(D3D_DRIVER_TYPE_WARP))
        else {
            return false;
        };
        match start_session(hwnd, width, height, device, device_context) {
            Ok(Some(session)) => {
                self.wgc = Some(session);
                true
            }
            Ok(None) | Err(_) => false,
        }
    }

    fn wic(&mut self) -> windows::core::Result<IWICImagingFactory> {
        if let Some(wic) = &self.wic {
            return Ok(wic.clone());
        }
        let wic =
            unsafe { CoCreateInstance(&CLSID_WICImagingFactory2, None, CLSCTX_INPROC_SERVER) }
                .or_else(|_| unsafe {
                    CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
                })?;
        self.wic = Some(wic);
        Ok(self.wic.clone().expect("factory stored above"))
    }

    /// Encodes the BGRA frame at `output_width` x `output_height`; a size
    /// different from the frame is resampled with WIC Fant.
    fn encode(
        &mut self,
        frame: &Frame,
        path: &str,
        format: &str,
        quality: i32,
        output_width: i32,
        output_height: i32,
    ) -> windows::core::Result<()> {
        let wic = self.wic()?;
        unsafe {
            let bitmap = wic.CreateBitmapFromMemory(
                frame.width as u32,
                frame.height as u32,
                &GUID_WICPixelFormat32bppBGRA,
                frame.width as u32 * 4,
                &frame.pixels,
            )?;
            let mut image: IWICBitmapSource = bitmap.cast()?;
            if output_width != frame.width || output_height != frame.height {
                let scaler = wic.CreateBitmapScaler()?;
                scaler.Initialize(
                    &bitmap,
                    output_width as u32,
                    output_height as u32,
                    WICBitmapInterpolationModeFant,
                )?;
                image = scaler.cast()?;
            }

            let stream = wic.CreateStream()?;
            stream.InitializeFromFilename(&HSTRING::from(path), GENERIC_WRITE.0)?;
            let encoder = wic.CreateEncoder(&container_format(format), std::ptr::null())?;
            encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
            let mut frame_encode: Option<IWICBitmapFrameEncode> = None;
            let mut properties: Option<IPropertyBag2> = None;
            encoder.CreateNewFrame(&mut frame_encode, &mut properties)?;
            let frame_encode = frame_encode.ok_or_else(windows::core::Error::empty)?;
            if format != "png"
                && let Some(properties) = &properties
            {
                let property = PROPBAG2 {
                    pstrName: windows::core::PWSTR(w!("ImageQuality").as_ptr().cast_mut()),
                    ..Default::default()
                };
                let value = VARIANT::from(quality.clamp(1, 100) as f32 / 100.0);
                let _ = properties.Write(1, &property, &value);
            }
            frame_encode.Initialize(properties.as_ref())?;
            frame_encode.SetSize(output_width as u32, output_height as u32)?;
            let mut pixel_format = GUID_WICPixelFormat32bppBGRA;
            frame_encode.SetPixelFormat(&mut pixel_format)?;
            frame_encode.WriteSource(&image, std::ptr::null())?;
            frame_encode.Commit()?;
            encoder.Commit()?;
        }
        Ok(())
    }

    /// Encodes one frame twice: the full-resolution original to `full_path`
    /// and a reduced copy (0.5x above 1280x720, otherwise 1x, then capped by
    /// a positive `max_edge`) to `reduced_path`.
    pub(crate) fn capture(
        &mut self,
        hwnd: HWND,
        window_rect: &RECT,
        request: &CaptureRequest<'_>,
        context: &Context,
    ) -> Result<CaptureResult, Error> {
        if hwnd.is_invalid() {
            return Err(Error::new("invalid_window", "A window handle is required"));
        }
        let width = window_rect.right - window_rect.left;
        let height = window_rect.bottom - window_rect.top;
        if width <= 0 || height <= 0 {
            return Err(Error::new("invalid_window", "Window has no area"));
        }

        context.check()?;
        let runtime = RuntimeScope::enter();
        let mut used_wgc = runtime.initialized && self.ensure_wgc(hwnd, width, height);
        let mut frame = None;
        let mut frame_timestamp_100ns = 0;
        let mut fresh_frame = false;
        let mut cached_frame = false;
        let mut captured_at_unix_ms = 0;
        if used_wgc {
            let minimum_timestamp = self.minimum_frame_timestamp_100ns;
            if minimum_timestamp != 0 {
                while qpc_time_100ns() < minimum_timestamp + INPUT_SETTLE_100NS {
                    context.check()?;
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
            let wait_ms = if self.last_frame.is_none() {
                FIRST_FRAME_WAIT_MS
            } else if minimum_timestamp != 0 {
                POST_INPUT_FRAME_WAIT_MS
            } else {
                0
            };
            let mut acquired = match &self.wgc {
                Some(wgc) => {
                    wgc.next_frame(context, Duration::from_millis(wait_ms), minimum_timestamp)?
                }
                None => None,
            };
            if acquired.is_none() && minimum_timestamp != 0 {
                // WGC emits frames only when content changes, so an input that
                // left the window unchanged never produces one. A new capture
                // session always delivers an initial frame of the current
                // content, captured after the input, so restarting keeps the
                // post-input guarantee.
                self.reset_wgc();
                if self.ensure_wgc(hwnd, width, height)
                    && let Some(wgc) = &self.wgc
                {
                    acquired = wgc.next_frame(
                        context,
                        Duration::from_millis(FIRST_FRAME_WAIT_MS),
                        minimum_timestamp,
                    )?;
                }
            }
            if let Some((captured, timestamp)) = acquired {
                frame = self.wgc.as_mut().and_then(|wgc| wgc.copy_frame(&captured));
                let _ = captured.Close();
                frame_timestamp_100ns = timestamp;
            }
            fresh_frame = frame.is_some();
            if let Some(frame) = &frame {
                let frame_age_ms = ((qpc_time_100ns() - frame_timestamp_100ns)
                    / INTERVALS_100NS_PER_MILLISECOND)
                    .max(0);
                captured_at_unix_ms = unix_time_milliseconds() - frame_age_ms;
                self.last_frame = Some(RetainedFrame {
                    pixels: frame.pixels.clone(),
                    width: frame.width,
                    height: frame.height,
                    timestamp_100ns: frame_timestamp_100ns,
                    captured_at_unix_ms,
                });
                self.minimum_frame_timestamp_100ns = 0;
            } else if minimum_timestamp != 0 {
                return Err(Error::new(
                    "capture_not_ready",
                    "No frame acquired after input; observe again",
                ));
            } else if let Some(last) = &self.last_frame {
                // Static windows are not required to emit a new WGC frame for
                // every observation. Return the newest retained pixels at
                // once; input actions separately require a frame newer than
                // their completion.
                frame = Some(Frame {
                    pixels: last.pixels.clone(),
                    width: last.width,
                    height: last.height,
                });
                frame_timestamp_100ns = last.timestamp_100ns;
                captured_at_unix_ms = last.captured_at_unix_ms;
                cached_frame = true;
            } else {
                used_wgc = false;
            }
        }
        let frame = match frame {
            Some(frame) if used_wgc => frame,
            _ => {
                context.check()?;
                let frame = capture_gdi(window_rect).ok_or_else(|| {
                    Error::new(
                        "capture_failed",
                        "Windows.Graphics.Capture and desktop capture failed",
                    )
                })?;
                captured_at_unix_ms = unix_time_milliseconds();
                self.minimum_frame_timestamp_100ns = 0;
                frame
            }
        };
        context.check()?;

        // The reduced image halves frames larger than 1280x720 and keeps
        // smaller ones at 1x; a positive max_edge further caps only the
        // reduced image.
        let mut factor = if frame.width > 1280 || frame.height > 720 {
            0.5
        } else {
            1.0
        };
        let longest_edge = frame.width.max(frame.height);
        if request.max_edge > 0 && f64::from(longest_edge) * factor > f64::from(request.max_edge) {
            factor = f64::from(request.max_edge) / f64::from(longest_edge);
        }
        let reduced_width = ((f64::from(frame.width) * factor).round() as i32).max(1);
        let reduced_height = ((f64::from(frame.height) * factor).round() as i32).max(1);
        self.encode(
            &frame,
            request.full_path,
            request.format,
            request.quality,
            frame.width,
            frame.height,
        )
        .map_err(|_| encode_failed())?;
        let reduced = context.check().and_then(|()| {
            self.encode(
                &frame,
                request.reduced_path,
                request.format,
                request.quality,
                reduced_width,
                reduced_height,
            )
            .map_err(|_| encode_failed())
        });
        if let Err(error) = reduced {
            remove_file(request.full_path);
            return Err(error);
        }

        // WGC can report a frame size that differs from the Win32 outer rect
        // by invisible resize borders. Each image's transform is its pixel
        // size over the window rect, so that ratio is folded in.
        let image = |path: &str, image_width: i32, image_height: i32| CapturedImage {
            path: path.to_owned(),
            width: image_width,
            height: image_height,
            scale_x: f64::from(image_width) / f64::from(width),
            scale_y: f64::from(image_height) / f64::from(height),
        };
        let full = image(request.full_path, frame.width, frame.height);
        let reduced = image(request.reduced_path, reduced_width, reduced_height);
        Ok(CaptureResult {
            path: reduced.path.clone(),
            mime_type: if request.format == "png" {
                "image/png"
            } else {
                "image/jpeg"
            },
            width: reduced_width,
            height: reduced_height,
            scale: reduced.scale_x,
            scale_x: reduced.scale_x,
            scale_y: reduced.scale_y,
            backend: if used_wgc {
                "windows-graphics-capture"
            } else {
                "gdi-visible-desktop-fallback"
            },
            frame_timestamp_100ns,
            captured_at_unix_ms,
            fresh_frame,
            cached_frame,
            source_width: frame.width,
            source_height: frame.height,
            full,
            reduced,
        })
    }
}
