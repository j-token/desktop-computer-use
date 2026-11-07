//! Frames of the portal's ScreenCast stream through libpipewire-0.3. The
//! stream is opened once per portal session and frames are copied on demand,
//! so repeated observations open no new permission dialog or connection.

use std::ffi::{c_char, c_int, c_void};
use std::os::fd::{IntoRawFd, OwnedFd};
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::time::{Duration, Instant, SystemTime};

use dcu_core::{Context, Error};
use parking_lot::{Condvar, Mutex};

use crate::image::RgbaImage;

const SPA_TYPE_ID: u32 = 3;
const SPA_TYPE_RECTANGLE: u32 = 10;
const SPA_TYPE_FRACTION: u32 = 11;
const SPA_TYPE_OBJECT: u32 = 15;
const SPA_TYPE_CHOICE: u32 = 19;
const SPA_TYPE_OBJECT_FORMAT: u32 = 0x40003;
const SPA_CHOICE_NONE: u32 = 0;
const SPA_CHOICE_RANGE: u32 = 1;
const SPA_CHOICE_ENUM: u32 = 3;
const SPA_PARAM_ENUM_FORMAT: u32 = 3;
const SPA_PARAM_FORMAT: u32 = 4;
const SPA_FORMAT_MEDIA_TYPE: u32 = 1;
const SPA_FORMAT_MEDIA_SUBTYPE: u32 = 2;
const SPA_FORMAT_VIDEO_FORMAT: u32 = 0x20001;
const SPA_FORMAT_VIDEO_SIZE: u32 = 0x20003;
const SPA_FORMAT_VIDEO_FRAMERATE: u32 = 0x20004;
const SPA_MEDIA_TYPE_VIDEO: u32 = 2;
const SPA_MEDIA_SUBTYPE_RAW: u32 = 1;
const SPA_META_HEADER: u32 = 1;

const VIDEO_FORMAT_RGBX: u32 = 7;
const VIDEO_FORMAT_BGRX: u32 = 8;
const VIDEO_FORMAT_XRGB: u32 = 9;
const VIDEO_FORMAT_XBGR: u32 = 10;
const VIDEO_FORMAT_RGBA: u32 = 11;
const VIDEO_FORMAT_BGRA: u32 = 12;
const VIDEO_FORMAT_ARGB: u32 = 13;
const VIDEO_FORMAT_ABGR: u32 = 14;

const PW_DIRECTION_INPUT: c_int = 0;
const PW_STREAM_FLAG_AUTOCONNECT_MAP_BUFFERS: c_int = 1 | 4;
const PW_VERSION_STREAM_EVENTS: u32 = 2;
const FRAME_WAIT: Duration = Duration::from_secs(5);
const MAX_FRAME_AGE_NS: i64 = 5 * 60 * 1_000_000_000;

#[repr(C)]
struct SpaPod {
    size: u32,
    kind: u32,
}

#[repr(C)]
struct SpaHook {
    private: [*mut c_void; 6],
}

type UnusedEvent = Option<unsafe extern "C" fn()>;

#[repr(C)]
struct PwStreamEvents {
    version: u32,
    destroy: UnusedEvent,
    state_changed: Option<unsafe extern "C" fn(*mut c_void, c_int, c_int, *const c_char)>,
    control_info: UnusedEvent,
    io_changed: UnusedEvent,
    param_changed: Option<unsafe extern "C" fn(*mut c_void, u32, *const SpaPod)>,
    add_buffer: UnusedEvent,
    remove_buffer: UnusedEvent,
    process: Option<unsafe extern "C" fn(*mut c_void)>,
    drained: UnusedEvent,
    command: UnusedEvent,
    trigger_done: UnusedEvent,
}

#[repr(C)]
struct PwBuffer {
    buffer: *mut SpaBuffer,
    user_data: *mut c_void,
    size: u64,
}

#[repr(C)]
struct SpaBuffer {
    n_metas: u32,
    n_datas: u32,
    metas: *mut SpaMeta,
    datas: *mut SpaData,
}

#[repr(C)]
struct SpaMeta {
    kind: u32,
    size: u32,
    data: *mut c_void,
}

#[repr(C)]
struct SpaData {
    kind: u32,
    flags: u32,
    fd: i64,
    mapoffset: u32,
    maxsize: u32,
    data: *mut c_void,
    chunk: *mut SpaChunk,
}

#[repr(C)]
struct SpaChunk {
    offset: u32,
    size: u32,
    stride: i32,
    flags: i32,
}

#[repr(C)]
struct SpaMetaHeader {
    flags: u32,
    offset: u32,
    pts: i64,
    dts_offset: i64,
    seq: u64,
}

#[link(name = "pipewire-0.3")]
unsafe extern "C" {
    fn pw_init(argc: *mut c_int, argv: *mut *mut *mut c_char);
    fn pw_thread_loop_new(name: *const c_char, props: *const c_void) -> *mut c_void;
    fn pw_thread_loop_get_loop(thread_loop: *mut c_void) -> *mut c_void;
    fn pw_thread_loop_start(thread_loop: *mut c_void) -> c_int;
    fn pw_thread_loop_lock(thread_loop: *mut c_void);
    fn pw_thread_loop_unlock(thread_loop: *mut c_void);
    fn pw_thread_loop_stop(thread_loop: *mut c_void);
    fn pw_thread_loop_destroy(thread_loop: *mut c_void);
    fn pw_context_new(
        main_loop: *mut c_void,
        props: *mut c_void,
        user_data_size: usize,
    ) -> *mut c_void;
    fn pw_context_destroy(context: *mut c_void);
    fn pw_context_connect_fd(
        context: *mut c_void,
        fd: c_int,
        props: *mut c_void,
        user_data_size: usize,
    ) -> *mut c_void;
    fn pw_core_disconnect(core: *mut c_void) -> c_int;
    fn pw_properties_new(key: *const c_char, ...) -> *mut c_void;
    fn pw_stream_new(core: *mut c_void, name: *const c_char, props: *mut c_void) -> *mut c_void;
    fn pw_stream_add_listener(
        stream: *mut c_void,
        listener: *mut SpaHook,
        events: *const PwStreamEvents,
        data: *mut c_void,
    );
    fn pw_stream_connect(
        stream: *mut c_void,
        direction: c_int,
        target_id: u32,
        flags: c_int,
        params: *mut *const SpaPod,
        n_params: u32,
    ) -> c_int;
    fn pw_stream_disconnect(stream: *mut c_void) -> c_int;
    fn pw_stream_destroy(stream: *mut c_void);
    fn pw_stream_dequeue_buffer(stream: *mut c_void) -> *mut PwBuffer;
    fn pw_stream_queue_buffer(stream: *mut c_void, buffer: *mut PwBuffer) -> c_int;
    // The deprecated call, since pw_stream_get_time_n needs PipeWire 0.3.50
    // and Ubuntu 22.04 ships 0.3.48.
    fn pw_stream_get_time(stream: *mut c_void, time: *mut c_void) -> c_int;
}

/// `spa_pod_builder_add_object` output of the capture's EnumFormat: BGRx,
/// RGBx, or RGBA at any size up to 8192x8192 (preferring the window size)
/// and up to 120 frames per second.
pub fn enum_format_pod(preferred_width: u32, preferred_height: u32) -> Vec<u8> {
    let mut properties = Vec::new();
    let mut property = |key: u32, pod: Vec<u8>| {
        properties.extend_from_slice(&key.to_ne_bytes());
        properties.extend_from_slice(&0u32.to_ne_bytes());
        properties.extend_from_slice(&pod);
    };
    property(
        SPA_FORMAT_MEDIA_TYPE,
        pod(SPA_TYPE_ID, &[SPA_MEDIA_TYPE_VIDEO]),
    );
    property(
        SPA_FORMAT_MEDIA_SUBTYPE,
        pod(SPA_TYPE_ID, &[SPA_MEDIA_SUBTYPE_RAW]),
    );
    property(
        SPA_FORMAT_VIDEO_FORMAT,
        choice(
            SPA_CHOICE_ENUM,
            SPA_TYPE_ID,
            4,
            &[VIDEO_FORMAT_BGRX, VIDEO_FORMAT_RGBX, VIDEO_FORMAT_RGBA],
        ),
    );
    property(
        SPA_FORMAT_VIDEO_SIZE,
        choice(
            SPA_CHOICE_RANGE,
            SPA_TYPE_RECTANGLE,
            8,
            &[preferred_width, preferred_height, 1, 1, 8192, 8192],
        ),
    );
    property(
        SPA_FORMAT_VIDEO_FRAMERATE,
        choice(
            SPA_CHOICE_RANGE,
            SPA_TYPE_FRACTION,
            8,
            &[30, 1, 0, 1, 120, 1],
        ),
    );

    let mut body = Vec::new();
    body.extend_from_slice(&SPA_TYPE_OBJECT_FORMAT.to_ne_bytes());
    body.extend_from_slice(&SPA_PARAM_ENUM_FORMAT.to_ne_bytes());
    body.extend_from_slice(&properties);
    let mut object = Vec::new();
    object.extend_from_slice(&(body.len() as u32).to_ne_bytes());
    object.extend_from_slice(&SPA_TYPE_OBJECT.to_ne_bytes());
    object.extend_from_slice(&body);
    object
}

/// A pod with its body padded to 8 bytes, as the SPA builder writes it.
fn pod(kind: u32, values: &[u32]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&((values.len() * 4) as u32).to_ne_bytes());
    bytes.extend_from_slice(&kind.to_ne_bytes());
    for value in values {
        bytes.extend_from_slice(&value.to_ne_bytes());
    }
    bytes.resize(bytes.len().next_multiple_of(8), 0);
    bytes
}

fn choice(choice_type: u32, child_type: u32, child_size: u32, values: &[u32]) -> Vec<u8> {
    let mut body = vec![choice_type, 0, child_size, child_type];
    body.extend_from_slice(values);
    pod(SPA_TYPE_CHOICE, &body)
}

#[derive(Debug, PartialEq, Eq)]
pub struct VideoFormat {
    pub format: u32,
    pub width: u32,
    pub height: u32,
}

/// `spa_format_video_raw_parse`: a Format object with a video format, size,
/// and framerate, each either plain or a None choice.
pub fn parse_video_format(pod: &[u8]) -> Option<VideoFormat> {
    let read = |offset: usize| {
        pod.get(offset..offset + 4)
            .map(|bytes| u32::from_ne_bytes(bytes.try_into().unwrap()))
    };
    let object_size = read(0)? as usize;
    if read(4)? != SPA_TYPE_OBJECT || object_size < 8 || pod.len() < 8 + object_size {
        return None;
    }
    if read(8)? != SPA_TYPE_OBJECT_FORMAT {
        return None;
    }
    let object_end = 8 + object_size;
    let property = |wanted_key: u32, wanted_type: u32, minimum_size: usize| -> Option<&[u8]> {
        let mut offset = 16;
        while offset + 16 <= object_end {
            let key = read(offset)?;
            let size = read(offset + 8)? as usize;
            let kind = read(offset + 12)?;
            let body = pod.get(offset + 16..offset + 16 + size)?;
            if key == wanted_key {
                return collect_value(body, kind, wanted_type, minimum_size);
            }
            offset += 16 + size.next_multiple_of(8);
        }
        None
    };
    let format = property(SPA_FORMAT_VIDEO_FORMAT, SPA_TYPE_ID, 4)?;
    let size = property(SPA_FORMAT_VIDEO_SIZE, SPA_TYPE_RECTANGLE, 8)?;
    property(SPA_FORMAT_VIDEO_FRAMERATE, SPA_TYPE_FRACTION, 8)?;
    let word = |bytes: &[u8], index: usize| {
        u32::from_ne_bytes(bytes[index * 4..index * 4 + 4].try_into().unwrap())
    };
    Some(VideoFormat {
        format: word(format, 0),
        width: word(size, 0),
        height: word(size, 1),
    })
}

fn collect_value(body: &[u8], kind: u32, wanted_type: u32, minimum_size: usize) -> Option<&[u8]> {
    if kind != SPA_TYPE_CHOICE {
        return (kind == wanted_type && body.len() >= minimum_size).then_some(body);
    }
    let word = |index: usize| {
        body.get(index * 4..index * 4 + 4)
            .map(|bytes| u32::from_ne_bytes(bytes.try_into().unwrap()))
    };
    if word(0)? != SPA_CHOICE_NONE {
        return None;
    }
    let child_size = word(2)? as usize;
    let child_type = word(3)?;
    let child = body.get(16..16 + child_size)?;
    (child_type == wanted_type && child.len() >= minimum_size).then_some(child)
}

struct FrameState {
    failed: bool,
    has_frame: bool,
    input_requires_fresh_frame: bool,
    frame_width: i32,
    frame_height: i32,
    frame_stride: i32,
    frame_format: u32,
    frame: Vec<u8>,
    frame_sequence: u64,
    source_frame_sequence: u64,
    frame_timestamp_ns: i64,
    frame_generation: u64,
    input_watermark_sequence: u64,
    frame_captured_at: SystemTime,
}

/// What the PipeWire thread shares with the capture caller. It is boxed so
/// its address, which the stream listener keeps, never moves.
struct StreamShared {
    state: Mutex<FrameState>,
    condition: Condvar,
    stream: AtomicPtr<c_void>,
}

impl StreamShared {
    fn fail(&self) {
        self.state.lock().failed = true;
        self.condition.notify_all();
    }
}

struct Connection {
    thread_loop: *mut c_void,
    context: *mut c_void,
    core: *mut c_void,
    stream: *mut c_void,
    loop_started: bool,
    // libpipewire keeps pointers to both for the stream's lifetime.
    listener: Box<SpaHook>,
    events: Box<PwStreamEvents>,
}

pub struct PipeWireCapture {
    shared: Box<StreamShared>,
    connection: Option<Connection>,
}

// SAFETY: the raw PipeWire objects are only touched by the owner under the
// backend's lock and by PipeWire's own thread through `StreamShared`.
unsafe impl Send for PipeWireCapture {}

impl PipeWireCapture {
    pub fn new() -> Self {
        Self {
            shared: Box::new(StreamShared {
                state: Mutex::new(FrameState {
                    failed: false,
                    has_frame: false,
                    input_requires_fresh_frame: false,
                    frame_width: 0,
                    frame_height: 0,
                    frame_stride: 0,
                    frame_format: 0,
                    frame: Vec::new(),
                    frame_sequence: 0,
                    source_frame_sequence: 0,
                    frame_timestamp_ns: 0,
                    frame_generation: 0,
                    input_watermark_sequence: 0,
                    frame_captured_at: SystemTime::UNIX_EPOCH,
                }),
                condition: Condvar::new(),
                stream: AtomicPtr::new(ptr::null_mut()),
            }),
            connection: None,
        }
    }

    pub fn started(&self) -> bool {
        self.connection.is_some()
    }

    pub fn mark_input_complete(&self) {
        let mut state = self.shared.state.lock();
        state.input_watermark_sequence = state.frame_sequence;
        state.input_requires_fresh_frame = true;
    }

    pub fn coordinate_generation(&self) -> u64 {
        self.shared.state.lock().frame_generation
    }

    /// Connects to the stream `node_id` of the PipeWire remote `remote`.
    pub fn start(
        &mut self,
        remote: OwnedFd,
        node_id: u32,
        serial: u64,
        width: i32,
        height: i32,
    ) -> Result<(), Error> {
        {
            let mut state = self.shared.state.lock();
            state.frame_width = width.max(1);
            state.frame_height = height.max(1);
            state.failed = false;
            state.has_frame = false;
        }
        let result = self.connect(
            remote,
            node_id,
            serial,
            width.max(1) as u32,
            height.max(1) as u32,
        );
        if result.is_err() {
            self.destroy();
        }
        result
    }

    fn connect(
        &mut self,
        remote: OwnedFd,
        node_id: u32,
        serial: u64,
        width: u32,
        height: u32,
    ) -> Result<(), Error> {
        let unavailable = |message: &str| Error::new("capture_unavailable", message);
        let events = Box::new(PwStreamEvents {
            version: PW_VERSION_STREAM_EVENTS,
            destroy: None,
            state_changed: Some(state_changed),
            control_info: None,
            io_changed: None,
            param_changed: Some(param_changed),
            add_buffer: None,
            remove_buffer: None,
            process: Some(process),
            drained: None,
            command: None,
            trigger_done: None,
        });
        let listener = Box::new(SpaHook {
            private: [ptr::null_mut(); 6],
        });
        // SAFETY: plain libpipewire calls; every pointer passed is either one
        // it returned above or owned by this object until destroy().
        unsafe {
            pw_init(ptr::null_mut(), ptr::null_mut());
            let thread_loop = pw_thread_loop_new(c"dcu-capture".as_ptr(), ptr::null());
            if thread_loop.is_null() {
                return Err(unavailable("Could not create PipeWire loop"));
            }
            self.connection = Some(Connection {
                thread_loop,
                context: ptr::null_mut(),
                core: ptr::null_mut(),
                stream: ptr::null_mut(),
                loop_started: false,
                listener,
                events,
            });
            let connection = self.connection.as_mut().unwrap();
            connection.context =
                pw_context_new(pw_thread_loop_get_loop(thread_loop), ptr::null_mut(), 0);
            if connection.context.is_null() {
                return Err(unavailable("Could not create PipeWire context"));
            }
            // pw_context_connect_fd owns the descriptor from here on, also on
            // failure; PipeWire closes it on disconnect.
            connection.core =
                pw_context_connect_fd(connection.context, remote.into_raw_fd(), ptr::null_mut(), 0);
            if connection.core.is_null() {
                return Err(unavailable("Could not connect to PipeWire remote"));
            }
            let target_serial = std::ffi::CString::new(serial.to_string()).unwrap();
            let properties = if serial == 0 {
                ptr::null_mut()
            } else {
                pw_properties_new(
                    c"target.object".as_ptr(),
                    target_serial.as_ptr(),
                    ptr::null::<c_char>(),
                )
            };
            connection.stream = pw_stream_new(
                connection.core,
                c"desktop-computer-use-capture".as_ptr(),
                properties,
            );
            if connection.stream.is_null() {
                return Err(unavailable("Could not create PipeWire stream"));
            }
            self.shared
                .stream
                .store(connection.stream, Ordering::Release);
            let shared = (&*self.shared as *const StreamShared)
                .cast_mut()
                .cast::<c_void>();
            pw_stream_add_listener(
                connection.stream,
                &mut *connection.listener,
                &*connection.events,
                shared,
            );

            let format = enum_format_pod(width, height);
            let mut params = [format.as_ptr().cast::<SpaPod>()];
            if pw_thread_loop_start(thread_loop) != 0 {
                return Err(unavailable("Could not start PipeWire loop"));
            }
            connection.loop_started = true;
            pw_thread_loop_lock(thread_loop);
            let connected = pw_stream_connect(
                connection.stream,
                PW_DIRECTION_INPUT,
                node_id,
                PW_STREAM_FLAG_AUTOCONNECT_MAP_BUFFERS,
                params.as_mut_ptr(),
                1,
            );
            pw_thread_loop_unlock(thread_loop);
            if connected != 0 {
                return Err(unavailable("Could not connect PipeWire ScreenCast stream"));
            }
        }
        Ok(())
    }

    fn destroy(&mut self) {
        let Some(connection) = self.connection.take() else {
            return;
        };
        // SAFETY: tears down the objects created in connect(), in
        // libpipewire's required order, each at most once.
        unsafe {
            if connection.loop_started {
                pw_thread_loop_lock(connection.thread_loop);
                if !connection.stream.is_null() {
                    pw_stream_disconnect(connection.stream);
                }
                pw_thread_loop_unlock(connection.thread_loop);
                pw_thread_loop_stop(connection.thread_loop);
            }
            self.shared.stream.store(ptr::null_mut(), Ordering::Release);
            if !connection.stream.is_null() {
                pw_stream_destroy(connection.stream);
            }
            if !connection.core.is_null() {
                pw_core_disconnect(connection.core);
            }
            if !connection.context.is_null() {
                pw_context_destroy(connection.context);
            }
            pw_thread_loop_destroy(connection.thread_loop);
        }
    }

    /// Copies the window's pixels from the latest frame. After input, only a
    /// frame produced after that input counts. `origin` is the screen
    /// position of the frame's top-left pixel.
    pub fn capture(
        &self,
        window: [i32; 4],
        origin: (i32, i32),
        context: &Context,
    ) -> Result<RgbaImage, Error> {
        let stream_failed = || {
            Error::new(
                "capture_unavailable",
                "PipeWire reported a capture stream failure",
            )
        };
        let (require_fresh_frame, watermark, wait_for_frame) = {
            let state = self.shared.state.lock();
            if state.failed {
                return Err(stream_failed());
            }
            let wait = !state.has_frame
                || (state.input_requires_fresh_frame
                    && state.frame_sequence <= state.input_watermark_sequence);
            (
                state.input_requires_fresh_frame,
                state.input_watermark_sequence,
                wait,
            )
        };
        if wait_for_frame {
            let deadline = Instant::now() + FRAME_WAIT;
            while Instant::now() < deadline {
                context.check()?;
                let mut state = self.shared.state.lock();
                if state.failed
                    || (state.has_frame
                        && (!require_fresh_frame || state.frame_sequence > watermark))
                {
                    break;
                }
                self.shared
                    .condition
                    .wait_for(&mut state, Duration::from_millis(50));
            }
        }
        context.check()?;

        let mut state = self.shared.state.lock();
        if state.failed {
            return Err(stream_failed());
        }
        let frame_bytes = state.frame_stride as usize * state.frame_height.max(0) as usize;
        if !state.has_frame
            || state.frame_width <= 0
            || state.frame_height <= 0
            || state.frame_stride < state.frame_width * 4
            || state.frame.len() < frame_bytes
        {
            return Err(Error::new(
                "capture_unavailable",
                "No frame arrived from the PipeWire ScreenCast stream",
            ));
        }
        let fresh_frame = wait_for_frame && state.frame_sequence > watermark;
        if require_fresh_frame && !fresh_frame {
            return Err(Error::new(
                "capture_not_ready",
                "No new PipeWire frame arrived after the input operation",
            ));
        }
        if fresh_frame {
            state.input_requires_fresh_frame = false;
        }

        let [x, y, width, height] = window;
        let mut rgba = vec![0u8; width.max(0) as usize * height.max(0) as usize * 4];
        let format = state.frame_format;
        let channels: [usize; 3] = match format {
            VIDEO_FORMAT_BGRX | VIDEO_FORMAT_BGRA => [2, 1, 0],
            VIDEO_FORMAT_XBGR | VIDEO_FORMAT_ABGR => [3, 2, 1],
            VIDEO_FORMAT_XRGB | VIDEO_FORMAT_ARGB => [1, 2, 3],
            _ => [0, 1, 2],
        };
        for row in 0..height {
            let source_y = (y - origin.1 + row).clamp(0, state.frame_height - 1);
            for column in 0..width {
                let source_x = (x - origin.0 + column).clamp(0, state.frame_width - 1);
                let source =
                    source_y as usize * state.frame_stride as usize + source_x as usize * 4;
                let target = (row as usize * width as usize + column as usize) * 4;
                for (offset, channel) in channels.iter().enumerate() {
                    rgba[target + offset] = state.frame[source + channel];
                }
                rgba[target + 3] = 255;
            }
        }
        let mut image = RgbaImage::new(width, height, rgba);
        image.captured_at = state.frame_captured_at;
        image.frame_sequence = state.frame_sequence;
        image.source_frame_sequence = state.source_frame_sequence;
        image.frame_timestamp_ns = state.frame_timestamp_ns;
        image.frame_generation = state.frame_generation;
        image.fresh_frame = fresh_frame;
        image.cached_frame = !fresh_frame;
        Ok(image)
    }
}

impl Drop for PipeWireCapture {
    fn drop(&mut self) {
        self.destroy();
    }
}

unsafe extern "C" fn state_changed(
    data: *mut c_void,
    _old: c_int,
    _state: c_int,
    error: *const c_char,
) {
    // SAFETY: data is the StreamShared registered with the listener.
    let shared = unsafe { &*data.cast::<StreamShared>() };
    if !error.is_null() {
        shared.fail();
    }
}

unsafe extern "C" fn param_changed(data: *mut c_void, id: u32, param: *const SpaPod) {
    // SAFETY: data is the StreamShared registered with the listener.
    let shared = unsafe { &*data.cast::<StreamShared>() };
    if param.is_null() || id != SPA_PARAM_FORMAT {
        return;
    }
    // SAFETY: a pod is its 8-byte header followed by `size` body bytes.
    let pod = unsafe { std::slice::from_raw_parts(param.cast::<u8>(), 8 + (*param).size as usize) };
    let Some(format) =
        parse_video_format(pod).filter(|format| format.width != 0 && format.height != 0)
    else {
        shared.fail();
        return;
    };
    let mut state = shared.state.lock();
    let format_changed = state.frame_width != format.width as i32
        || state.frame_height != format.height as i32
        || state.frame_format != format.format;
    state.frame_width = format.width as i32;
    state.frame_height = format.height as i32;
    state.frame_format = format.format;
    if format_changed {
        state.frame_generation += 1;
        state.has_frame = false;
    }
    drop(state);
    shared.condition.notify_all();
}

unsafe extern "C" fn process(data: *mut c_void) {
    // SAFETY: data is the StreamShared registered with the listener.
    let shared = unsafe { &*data.cast::<StreamShared>() };
    let stream = shared.stream.load(Ordering::Acquire);
    if stream.is_null() {
        return;
    }
    // SAFETY: called on the PipeWire thread with the stream that owns this
    // listener; the dequeued buffer stays valid until it is queued again.
    unsafe {
        let buffer = pw_stream_dequeue_buffer(stream);
        if buffer.is_null() {
            return;
        }
        copy_frame(shared, stream, (*buffer).buffer);
        pw_stream_queue_buffer(stream, buffer);
    }
}

/// # Safety
/// `buffer` must be a mapped buffer dequeued from `stream`.
unsafe fn copy_frame(shared: &StreamShared, stream: *mut c_void, buffer: *const SpaBuffer) {
    // SAFETY: the caller guarantees a valid dequeued buffer.
    let buffer = unsafe { buffer.as_ref() };
    let Some(buffer) = buffer.filter(|buffer| buffer.n_datas > 0) else {
        return;
    };
    let data = unsafe { &*buffer.datas };
    if data.data.is_null() || data.chunk.is_null() {
        return;
    }
    let chunk = unsafe { &*data.chunk };
    let (width, height, format) = {
        let state = shared.state.lock();
        (state.frame_width, state.frame_height, state.frame_format)
    };
    let stride = chunk.stride;
    let row_bytes = width.max(0) as usize * 4;
    let source_stride = stride.unsigned_abs() as usize;
    let required = if height > 0 && source_stride > 0 {
        source_stride * height as usize
    } else {
        0
    };
    let available = chunk.size as usize;
    let supported_format = matches!(
        format,
        VIDEO_FORMAT_BGRX
            | VIDEO_FORMAT_RGBX
            | VIDEO_FORMAT_RGBA
            | VIDEO_FORMAT_BGRA
            | VIDEO_FORMAT_XRGB
            | VIDEO_FORMAT_XBGR
            | VIDEO_FORMAT_ARGB
            | VIDEO_FORMAT_ABGR
    );
    let usable = width > 0
        && height > 0
        && stride > 0
        && source_stride >= row_bytes
        && required <= available
        && chunk.offset <= data.maxsize
        && available <= (data.maxsize - chunk.offset) as usize
        && supported_format;
    if !usable {
        shared.fail();
        return;
    }
    // SAFETY: the bounds above keep the copied range inside the mapped chunk.
    let pixels = unsafe {
        std::slice::from_raw_parts(
            data.data.cast::<u8>().add(chunk.offset as usize),
            source_stride * height as usize,
        )
    };
    let mut captured_at = SystemTime::now();
    let mut source_sequence = 0;
    let mut timestamp_ns = 0;
    if let Some(header) = unsafe { meta_header(buffer) } {
        source_sequence = header.seq;
        timestamp_ns = header.pts;
        // SPA timestamps and the stream clock are monotonic nanoseconds.
        // Translate a sane timestamp delta to wall time so capturedAt
        // identifies frame production rather than callback scheduling time.
        if header.pts > 0 {
            // pw_time grew in later PipeWire releases, which write the larger
            // struct even through the deprecated call; `now` stays first.
            let mut stream_time = [0i64; 32];
            let result = unsafe { pw_stream_get_time(stream, stream_time.as_mut_ptr().cast()) };
            let now = stream_time[0];
            if result == 0 && now >= header.pts {
                let age = now - header.pts;
                if age <= MAX_FRAME_AGE_NS {
                    captured_at -= Duration::from_nanos(age as u64);
                }
            }
        }
    }
    let mut state = shared.state.lock();
    state.frame_stride = stride;
    state.frame.clear();
    state.frame.extend_from_slice(pixels);
    state.frame_captured_at = captured_at;
    state.frame_sequence += 1;
    state.source_frame_sequence = source_sequence;
    state.frame_timestamp_ns = timestamp_ns;
    state.has_frame = true;
    drop(state);
    shared.condition.notify_all();
}

/// `spa_buffer_find_meta_data(buffer, SPA_META_Header, sizeof header)`.
///
/// # Safety
/// `buffer` must describe valid metadata entries.
unsafe fn meta_header(buffer: &SpaBuffer) -> Option<&SpaMetaHeader> {
    (0..buffer.n_metas as usize).find_map(|index| {
        // SAFETY: index is below n_metas.
        let meta = unsafe { &*buffer.metas.add(index) };
        let fits = meta.kind == SPA_META_HEADER && meta.size as usize >= size_of::<SpaMetaHeader>();
        // SAFETY: a header entry of at least the header's size holds one.
        (fits && !meta.data.is_null()).then(|| unsafe { &*meta.data.cast::<SpaMetaHeader>() })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // spa_pod_builder_add_object output from libspa 0.3.48 (Ubuntu 22.04).
    const LIBSPA_ENUM_FORMAT_640X480: [u8; 224] = [
        0xd8, 0x00, 0x00, 0x00, 0x0f, 0x00, 0x00, 0x00, 0x03, 0x00, 0x04, 0x00, 0x03, 0x00, 0x00,
        0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x03, 0x00,
        0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1c, 0x00, 0x00,
        0x00, 0x13, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00,
        0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0x0b,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x28, 0x00, 0x00, 0x00, 0x13, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x08, 0x00, 0x00, 0x00, 0x0a, 0x00, 0x00, 0x00, 0x80, 0x02, 0x00, 0x00, 0xe0, 0x01,
        0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x20, 0x00, 0x00, 0x00,
        0x20, 0x00, 0x00, 0x04, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x28, 0x00, 0x00, 0x00,
        0x13, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00,
        0x00, 0x0b, 0x00, 0x00, 0x00, 0x1e, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x78, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
    ];

    // spa_format_video_raw_build output for BGRx 1280x720 at 0/1 fps.
    const LIBSPA_BGRX_1280X720_FORMAT: [u8; 136] = [
        0x80, 0x00, 0x00, 0x00, 0x0f, 0x00, 0x00, 0x00, 0x03, 0x00, 0x04, 0x00, 0x04, 0x00, 0x00,
        0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x03, 0x00,
        0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00,
        0x00, 0x03, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00,
        0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x0a, 0x00, 0x00, 0x00, 0x00,
        0x05, 0x00, 0x00, 0xd0, 0x02, 0x00, 0x00, 0x04, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x08, 0x00, 0x00, 0x00, 0x0b, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00,
        0x00,
    ];

    #[test]
    fn enum_format_matches_libspa_builder_bytes() {
        assert_eq!(enum_format_pod(640, 480), LIBSPA_ENUM_FORMAT_640X480);
    }

    #[test]
    fn fixed_format_parses_format_and_size() {
        let format = parse_video_format(&LIBSPA_BGRX_1280X720_FORMAT);

        assert_eq!(
            format,
            Some(VideoFormat {
                format: VIDEO_FORMAT_BGRX,
                width: 1280,
                height: 720
            })
        );
    }

    #[test]
    fn format_with_an_open_choice_is_rejected() {
        assert_eq!(parse_video_format(&LIBSPA_ENUM_FORMAT_640X480), None);
    }

    #[test]
    fn truncated_format_is_rejected() {
        assert_eq!(
            parse_video_format(&LIBSPA_BGRX_1280X720_FORMAT[..100]),
            None
        );
    }
}
