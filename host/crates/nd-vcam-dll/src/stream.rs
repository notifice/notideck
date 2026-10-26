// SPDX-License-Identifier: GPL-3.0-or-later
//! The single video stream: `IMFMediaStream2` + `IKsControl`, and the shared state behind it.
//!
//! Sample delivery is paced: `RequestSample` only enqueues the token; a worker thread (one per
//! started stream) waits for the next frame slot at the selected fps, fills a sample from the
//! shared memory (or the placeholder) and queues `MEMediaSample`. This keeps the Frame Server's
//! calling thread free and avoids delivering hundreds of samples a second to a pulling consumer.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use nd_vcam::shm::{nv12_len, FPS, MAX_HEIGHT, MAX_WIDTH, MODES};
use nd_vcam::{HasShm, ShmMapping};
use windows::core::{implement, Error, IUnknown, Interface, Result, GUID, HRESULT};
use windows::Win32::Foundation::{E_FAIL, E_INVALIDARG, E_POINTER, S_OK};
use windows::Win32::Media::KernelStreaming::{IKsControl, IKsControl_Impl, KSIDENTIFIER, PINNAME_VIDEO_CAPTURE};
use windows::Win32::Media::MediaFoundation::*;

use crate::attrs::forward_attributes;
use crate::frames::{DeliveryStats, FrameProvider, Source};
use crate::vlog;
use nd_vcam::shm::Peek;

/// Wrapper to move free-threaded MF objects (event queue, allocator, samples) to the worker.
pub struct Sendable<T>(pub T);
unsafe impl<T> Send for Sendable<T> {}
unsafe impl<T> Sync for Sendable<T> {}

pub fn err(hr: HRESULT) -> Error {
    Error::from(hr)
}

const SET_NOT_FOUND: HRESULT = HRESULT(0x8007_0490u32 as i32); // HRESULT_FROM_WIN32(ERROR_SET_NOT_FOUND)

// ---- process-wide shared memory + consumer bookkeeping -----------------------------------------

fn shm_slot() -> &'static Mutex<Option<Arc<ShmMapping>>> {
    static SLOT: OnceLock<Mutex<Option<Arc<ShmMapping>>>> = OnceLock::new();
    SLOT.get_or_init(Default::default)
}

/// The section for this process, created on first use and kept for the process lifetime (the
/// handle keeps the name alive for the app side even across stream restarts).
pub fn global_shm() -> Option<Arc<dyn HasShm>> {
    let mut slot = shm_slot().lock().unwrap_or_else(|e| e.into_inner());
    if slot.is_none() {
        match ShmMapping::create() {
            Ok(m) => {
                vlog!(
                    "shared memory: {:?} section {} ({} bytes)",
                    m.scope(),
                    if m.created() { "created" } else { "opened (already existed)" },
                    nd_vcam::shm::REGION_BYTES
                );
                *slot = Some(Arc::new(m));
            }
            Err(e) => vlog!("shared memory unavailable: {e:#}"),
        }
    }
    slot.clone().map(|a| a as Arc<dyn HasShm>)
}

static ACTIVE_CONSUMERS: AtomicUsize = AtomicUsize::new(0);

fn consumer_started(shm: &Option<Arc<dyn HasShm>>, w: u32, h: u32, fps: u32) {
    ACTIVE_CONSUMERS.fetch_add(1, Ordering::SeqCst);
    if let Some(s) = shm {
        s.shm().set_consumer(true, w, h, fps);
    }
}

fn consumer_stopped(shm: &Option<Arc<dyn HasShm>>, w: u32, h: u32, fps: u32) {
    let mut cur = ACTIVE_CONSUMERS.load(Ordering::SeqCst);
    let prev = loop {
        let next = cur.saturating_sub(1);
        match ACTIVE_CONSUMERS.compare_exchange(cur, next, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(p) => break p,
            Err(actual) => cur = actual,
        }
    };
    if prev <= 1 {
        if let Some(s) = shm {
            s.shm().set_consumer(false, w, h, fps);
        }
    }
}

// ---- media types --------------------------------------------------------------------------------

fn pack(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

fn nv12_type(w: u32, h: u32) -> Result<IMFMediaType> {
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        t.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, pack(w, h))?;
        t.SetUINT32(&MF_MT_DEFAULT_STRIDE, w)?;
        t.SetUINT64(&MF_MT_FRAME_RATE, pack(FPS, 1))?;
        t.SetUINT64(&MF_MT_FRAME_RATE_RANGE_MAX, pack(FPS, 1))?;
        t.SetUINT64(&MF_MT_FRAME_RATE_RANGE_MIN, pack(FPS, 1))?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        t.SetUINT32(&MF_MT_SAMPLE_SIZE, nv12_len(w, h) as u32)?;
        t.SetUINT32(&MF_MT_AVG_BITRATE, (nv12_len(w, h) as u64 * 8 * FPS as u64).min(u32::MAX as u64) as u32)?;
        Ok(t)
    }
}

/// (width, height, fps) of a media type, if it is a plausible NV12 type.
fn type_params(t: &IMFMediaType) -> Option<(u32, u32, u32)> {
    unsafe {
        let size = t.GetUINT64(&MF_MT_FRAME_SIZE).ok()?;
        let (w, h) = ((size >> 32) as u32, size as u32);
        let rate = t.GetUINT64(&MF_MT_FRAME_RATE).unwrap_or(pack(FPS, 1));
        let (n, d) = ((rate >> 32) as u32, (rate as u32).max(1));
        let fps = (n / d).clamp(1, 120);
        if w == 0 || h == 0 || w > MAX_WIDTH || h > MAX_HEIGHT || w % 2 != 0 || h % 2 != 0 {
            return None;
        }
        if let Ok(sub) = t.GetGUID(&MF_MT_SUBTYPE) {
            if sub != MFVideoFormat_NV12 {
                return None;
            }
        }
        Some((w, h, fps))
    }
}

// ---- shared stream state ---------------------------------------------------------------------

enum Msg {
    Request(Sendable<Option<IUnknown>>),
    Quit,
}

struct Worker {
    tx: Sender<Msg>,
    handle: JoinHandle<()>,
}

struct Inner {
    queue: Option<IMFMediaEventQueue>,
    descriptor: Option<IMFStreamDescriptor>,
    source: Option<IMFMediaSource>,
    allocator: Option<IMFVideoSampleAllocatorEx>,
    allocator_ready: bool,
    state: MF_STREAM_STATE,
    size: (u32, u32),
    fps: u32,
    provider: FrameProvider,
    shm: Option<Arc<dyn HasShm>>,
    worker: Option<Worker>,
    last_source: Option<Source>,
    delivery: DeliveryStats,
    stats_since: Instant,
}

pub struct StreamShared {
    id: u32,
    pub attrs: IMFAttributes,
    inner: Mutex<Inner>,
}
unsafe impl Send for StreamShared {}
unsafe impl Sync for StreamShared {}

impl StreamShared {
    pub fn new(id: u32) -> Result<Arc<Self>> {
        unsafe {
            let attrs = {
                let mut a = None;
                MFCreateAttributes(&mut a, 8)?;
                a.ok_or_else(|| err(E_FAIL))?
            };
            attrs.SetGUID(&MF_DEVICESTREAM_STREAM_CATEGORY, &PINNAME_VIDEO_CAPTURE)?;
            attrs.SetUINT32(&MF_DEVICESTREAM_STREAM_ID, id)?;
            attrs.SetUINT32(&MF_DEVICESTREAM_FRAMESERVER_SHARED, 1)?;
            attrs.SetUINT32(&MF_DEVICESTREAM_ATTRIBUTE_FRAMESOURCE_TYPES, MFFrameSourceTypes_Color.0 as u32)?;

            let types: Vec<Option<IMFMediaType>> =
                MODES.iter().map(|&(w, h)| nv12_type(w, h).map(Some)).collect::<Result<_>>()?;
            let descriptor = MFCreateStreamDescriptor(id, &types)?;
            let handler = descriptor.GetMediaTypeHandler()?;
            if let Some(Some(first)) = types.first() {
                handler.SetCurrentMediaType(first)?;
            }
            let queue = MFCreateEventQueue()?;
            Ok(Arc::new(StreamShared {
                id,
                attrs,
                inner: Mutex::new(Inner {
                    queue: Some(queue),
                    descriptor: Some(descriptor),
                    source: None,
                    allocator: None,
                    allocator_ready: false,
                    state: MF_STREAM_STATE_STOPPED,
                    size: MODES[0],
                    fps: FPS,
                    provider: FrameProvider::new(None),
                    shm: None,
                    worker: None,
                    last_source: None,
                    delivery: DeliveryStats::default(),
                    stats_since: Instant::now(),
                }),
            }))
        }
    }

    pub fn id(&self) -> u32 {
        self.id
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set_source(&self, source: Option<IMFMediaSource>) {
        self.lock().source = source;
    }

    pub fn descriptor(&self) -> Result<IMFStreamDescriptor> {
        self.lock().descriptor.clone().ok_or_else(|| err(MF_E_SHUTDOWN))
    }

    fn queue(&self) -> Result<IMFMediaEventQueue> {
        self.lock().queue.clone().ok_or_else(|| err(MF_E_SHUTDOWN))
    }

    pub fn state(&self) -> MF_STREAM_STATE {
        self.lock().state
    }

    pub fn set_allocator(&self, allocator: &IUnknown) -> Result<()> {
        let a: IMFVideoSampleAllocatorEx = allocator.cast()?;
        let mut g = self.lock();
        g.allocator = Some(a);
        g.allocator_ready = false;
        Ok(())
    }

    /// Start (or restart with a new type) delivering samples. `ty` is the type chosen by the
    /// consumer; `None` keeps the current/default one.
    pub fn start(self: &Arc<Self>, ty: Option<&IMFMediaType>) -> Result<()> {
        // A running worker (restart with a new type) is stopped first, outside the lock.
        self.stop_worker();
        let shm = global_shm();
        let mut g = self.lock();
        let queue = g.queue.clone().ok_or_else(|| err(MF_E_SHUTDOWN))?;

        let (w, h, fps) = match ty.and_then(type_params) {
            Some(p) => p,
            None => (g.size.0, g.size.1, g.fps),
        };
        let alloc_type = match ty {
            Some(t) => t.clone(),
            None => nv12_type(w, h)?,
        };

        if g.allocator.is_none() {
            let mut raw = std::ptr::null_mut();
            unsafe { MFCreateVideoSampleAllocatorEx(&IMFVideoSampleAllocatorEx::IID, &mut raw)? };
            g.allocator = Some(unsafe { IMFVideoSampleAllocatorEx::from_raw(raw) });
        }
        let allocator = g.allocator.clone().ok_or_else(|| err(E_FAIL))?;
        if g.allocator_ready {
            unsafe { allocator.UninitializeSampleAllocator().ok() };
        }
        match unsafe { allocator.InitializeSampleAllocator(10, &alloc_type) } {
            Ok(()) => g.allocator_ready = true,
            Err(e) => {
                // Not fatal: samples are then built from memory buffers.
                vlog!("InitializeSampleAllocator failed ({e}); using plain memory samples");
                g.allocator_ready = false;
            }
        }

        let was_running = g.state == MF_STREAM_STATE_RUNNING;
        if was_running {
            consumer_stopped(&g.shm, g.size.0, g.size.1, g.fps);
        }
        g.size = (w, h);
        g.fps = fps;
        g.shm = shm.clone();
        g.provider.set_shm(shm.clone());
        g.provider.reset();
        g.delivery = DeliveryStats::default();
        g.stats_since = Instant::now();
        consumer_started(&shm, w, h, fps);

        let (tx, rx) = channel();
        let me = Sendable(self.clone());
        let handle = std::thread::Builder::new()
            .name("vcam-stream".into())
            .spawn(move || worker_loop(me.0, rx))
            .map_err(|_| err(E_FAIL))?;
        g.worker = Some(Worker { tx, handle });
        g.state = MF_STREAM_STATE_RUNNING;
        drop(g);
        vlog!("stream started {w}x{h}@{fps}");
        unsafe { queue.QueueEventParamVar(MEStreamStarted.0 as u32, &GUID::zeroed(), S_OK, std::ptr::null())? };
        Ok(())
    }

    pub fn stop(&self) -> Result<()> {
        self.stop_worker();
        let mut g = self.lock();
        let queue = g.queue.clone().ok_or_else(|| err(MF_E_SHUTDOWN))?;
        if g.state == MF_STREAM_STATE_RUNNING {
            consumer_stopped(&g.shm, g.size.0, g.size.1, g.fps);
        }
        if g.allocator_ready {
            if let Some(a) = &g.allocator {
                unsafe { a.UninitializeSampleAllocator().ok() };
            }
            g.allocator_ready = false;
        }
        let was = g.state;
        g.state = MF_STREAM_STATE_STOPPED;
        drop(g);
        if was != MF_STREAM_STATE_STOPPED {
            vlog!("stream stopped");
            unsafe { queue.QueueEventParamVar(MEStreamStopped.0 as u32, &GUID::zeroed(), S_OK, std::ptr::null())? };
        }
        Ok(())
    }

    pub fn shutdown(&self) {
        let _ = self.stop();
        let mut g = self.lock();
        if let Some(q) = g.queue.take() {
            unsafe { q.Shutdown().ok() };
        }
        g.descriptor = None;
        g.source = None;
        g.allocator = None;
        g.shm = None;
        g.provider.set_shm(None);
    }

    pub fn pause(&self) -> Result<()> {
        let mut g = self.lock();
        if g.state != MF_STREAM_STATE_RUNNING {
            return Err(err(MF_E_INVALID_STATE_TRANSITION));
        }
        g.state = MF_STREAM_STATE_PAUSED;
        Ok(())
    }

    fn stop_worker(&self) {
        let w = self.lock().worker.take();
        if let Some(w) = w {
            let _ = w.tx.send(Msg::Quit);
            let _ = w.handle.join();
        }
    }

    pub fn request_sample(&self, token: Option<&IUnknown>) -> Result<()> {
        let g = self.lock();
        if g.queue.is_none() {
            return Err(err(MF_E_SHUTDOWN));
        }
        if g.state != MF_STREAM_STATE_RUNNING {
            return Err(err(MF_E_INVALIDREQUEST));
        }
        if let Some(w) = &g.worker {
            let _ = w.tx.send(Msg::Request(Sendable(token.cloned())));
        }
        Ok(())
    }

    /// Build and queue one sample (called on the worker thread).
    fn deliver(&self, token: Option<IUnknown>) {
        let mut g = self.lock();
        let (Some(queue), true) = (g.queue.clone(), g.state == MF_STREAM_STATE_RUNNING) else { return };
        let (w, h) = g.size;
        let fps = g.fps.max(1);
        let result = (|| -> Result<IMFSample> {
            let sample = make_sample(&g, w, h)?;
            let buffer = unsafe { sample.GetBufferByIndex(0)? };
            let g = &mut *g;
            let src = fill_buffer(&buffer, w, h, &mut g.provider)?;
            let age = g.provider.last_age_us();
            g.delivery.record(src, age);
            if g.stats_since.elapsed() >= STATS_INTERVAL {
                let secs = g.stats_since.elapsed().as_secs_f64();
                g.stats_since = Instant::now();
                vlog!("{}", g.delivery.take_line(secs));
            }
            if g.last_source != Some(src) {
                vlog!("frame source: {src:?}");
                g.last_source = Some(src);
            }
            unsafe {
                sample.SetSampleTime(MFGetSystemTime())?;
                sample.SetSampleDuration(10_000_000 / fps as i64)?;
                sample.SetUINT32(&MFSampleExtension_CleanPoint, 1)?;
                if let Some(t) = &token {
                    sample.SetUnknown(&MFSampleExtension_Token, t)?;
                }
            }
            Ok(sample)
        })();
        drop(g);
        match result {
            Ok(sample) => {
                if let Err(e) =
                    unsafe { queue.QueueEventParamUnk(MEMediaSample.0 as u32, &GUID::zeroed(), S_OK, &sample) }
                {
                    vlog!("queue MEMediaSample failed: {e}");
                }
            }
            Err(e) => {
                vlog!("sample failed: {e}");
                let _ =
                    unsafe { queue.QueueEventParamVar(MEError.0 as u32, &GUID::zeroed(), e.code(), std::ptr::null()) };
            }
        }
    }
}

fn make_sample(g: &Inner, w: u32, h: u32) -> Result<IMFSample> {
    if g.allocator_ready {
        if let Some(a) = &g.allocator {
            match unsafe { a.AllocateSample() } {
                Ok(s) => return Ok(s),
                Err(e) => vlog!("AllocateSample failed ({e}); using a memory buffer"),
            }
        }
    }
    unsafe {
        let s = MFCreateSample()?;
        let b = MFCreateMemoryBuffer(nv12_len(w, h) as u32)?;
        s.AddBuffer(&b)?;
        Ok(s)
    }
}

/// Write the next frame into `buffer`, honouring a 2-D buffer's pitch.
fn fill_buffer(buffer: &IMFMediaBuffer, w: u32, h: u32, provider: &mut FrameProvider) -> Result<Source> {
    let len = nv12_len(w, h);
    let (wu, hu) = (w as usize, h as usize);
    unsafe {
        if let Ok(b2d) = buffer.cast::<IMF2DBuffer>() {
            let mut base: *mut u8 = std::ptr::null_mut();
            let mut pitch: i32 = 0;
            b2d.Lock2D(&mut base, &mut pitch)?;
            let result = if base.is_null() || pitch < w as i32 {
                Err(err(E_POINTER))
            } else if pitch as usize == wu {
                Ok(provider.fill(w, h, std::slice::from_raw_parts_mut(base, len)))
            } else {
                let mut tmp = vec![0u8; len];
                let src = provider.fill(w, h, &mut tmp);
                let p = pitch as usize;
                for row in 0..hu {
                    std::ptr::copy_nonoverlapping(tmp.as_ptr().add(row * wu), base.add(row * p), wu);
                }
                let uv = base.add(p * hu);
                for row in 0..hu / 2 {
                    std::ptr::copy_nonoverlapping(tmp.as_ptr().add(wu * hu + row * wu), uv.add(row * p), wu);
                }
                Ok(src)
            };
            b2d.Unlock2D()?;
            let src = result?;
            let _ = buffer.SetCurrentLength(len as u32);
            return Ok(src);
        }
        let mut ptr: *mut u8 = std::ptr::null_mut();
        let mut max = 0u32;
        buffer.Lock(&mut ptr, Some(&mut max), None)?;
        let result = if ptr.is_null() || (max as usize) < len {
            Err(err(E_INVALIDARG))
        } else {
            Ok(provider.fill(w, h, std::slice::from_raw_parts_mut(ptr, len)))
        };
        buffer.Unlock()?;
        let src = result?;
        buffer.SetCurrentLength(len as u32)?;
        Ok(src)
    }
}

const STATS_INTERVAL: Duration = Duration::from_secs(5);

/// When the next slot is due but the producer has not published since the last sample, wait up
/// to this fraction of an interval for the frame that is about to arrive instead of repeating
/// the old one (which would then be a whole interval late).
const FRESH_WAIT_FRACTION: u32 = 2;

fn worker_loop(shared: Arc<StreamShared>, rx: Receiver<Msg>) {
    let mut deadline = Instant::now();
    while let Ok(msg) = rx.recv() {
        let token = match msg {
            Msg::Quit => break,
            Msg::Request(t) => t.0,
        };
        let interval = Duration::from_micros(1_000_000 / shared.lock().fps.max(1) as u64);
        let now = Instant::now();
        if now > deadline + interval * 2 {
            deadline = now; // fell behind (idle): do not burst
        }
        if deadline > now {
            std::thread::sleep(deadline - now);
        }
        // Fixed schedule + producer phase: a fresh frame can arrive just after the slot. Wait a
        // little for it, and when it comes re-phase the schedule to it so later frames are
        // picked up on arrival rather than up to a full interval late.
        let mut rephase = false;
        if shared.lock().provider.peek() == Peek::Unchanged {
            let give_up = Instant::now() + interval / FRESH_WAIT_FRACTION;
            loop {
                std::thread::sleep(Duration::from_millis(1));
                match shared.lock().provider.peek() {
                    Peek::Unchanged if Instant::now() < give_up => {}
                    Peek::New => {
                        rephase = true;
                        break;
                    }
                    _ => break,
                }
            }
        }
        if rephase {
            deadline = Instant::now();
        }
        deadline += interval;
        let delivered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| shared.deliver(token)));
        if delivered.is_err() {
            vlog!("panic while delivering a sample (caught)");
        }
    }
}

// ---- COM object ----------------------------------------------------------------------------------

#[implement(IMFMediaStream2, IKsControl, IMFAttributes)]
pub struct MediaStream {
    pub attrs: IMFAttributes,
    pub shared: Arc<StreamShared>,
}

impl MediaStream {
    pub fn create(shared: Arc<StreamShared>) -> IMFMediaStream2 {
        crate::exports::object_created();
        MediaStream { attrs: shared.attrs.clone(), shared }.into()
    }
}

impl Drop for MediaStream {
    fn drop(&mut self) {
        crate::exports::object_dropped();
    }
}

forward_attributes!(MediaStream_Impl);

impl IMFMediaEventGenerator_Impl for MediaStream_Impl {
    fn GetEvent(&self, flags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS) -> Result<IMFMediaEvent> {
        let q = self.shared.queue()?; // lock released before a possibly blocking GetEvent
        unsafe { q.GetEvent(flags.0) }
    }
    fn BeginGetEvent(&self, cb: Option<&IMFAsyncCallback>, state: Option<&IUnknown>) -> Result<()> {
        let q = self.shared.queue()?;
        unsafe { q.BeginGetEvent(cb, state) }
    }
    fn EndGetEvent(&self, result: Option<&IMFAsyncResult>) -> Result<IMFMediaEvent> {
        let q = self.shared.queue()?;
        unsafe { q.EndGetEvent(result) }
    }
    fn QueueEvent(
        &self,
        met: u32,
        ext: *const GUID,
        hr: HRESULT,
        value: *const windows::core::PROPVARIANT,
    ) -> Result<()> {
        let q = self.shared.queue()?;
        unsafe { q.QueueEventParamVar(met, ext, hr, value) }
    }
}

impl IMFMediaStream_Impl for MediaStream_Impl {
    fn GetMediaSource(&self) -> Result<IMFMediaSource> {
        self.shared.lock().source.clone().ok_or_else(|| err(MF_E_SHUTDOWN))
    }
    fn GetStreamDescriptor(&self) -> Result<IMFStreamDescriptor> {
        self.shared.descriptor()
    }
    fn RequestSample(&self, token: Option<&IUnknown>) -> Result<()> {
        guard(|| self.shared.request_sample(token))
    }
}

impl IMFMediaStream2_Impl for MediaStream_Impl {
    fn SetStreamState(&self, value: MF_STREAM_STATE) -> Result<()> {
        guard(|| {
            let cur = self.shared.state();
            if cur == value {
                return Ok(());
            }
            match value {
                MF_STREAM_STATE_PAUSED => self.shared.pause(),
                MF_STREAM_STATE_RUNNING => self.shared.start(None),
                MF_STREAM_STATE_STOPPED => self.shared.stop(),
                _ => Err(err(MF_E_INVALID_STATE_TRANSITION)),
            }
        })
    }
    fn GetStreamState(&self) -> Result<MF_STREAM_STATE> {
        Ok(self.shared.state())
    }
}

impl IKsControl_Impl for MediaStream_Impl {
    fn KsProperty(
        &self,
        _p: *const KSIDENTIFIER,
        _l: u32,
        _d: *mut core::ffi::c_void,
        _dl: u32,
        ret: *mut u32,
    ) -> Result<()> {
        if ret.is_null() {
            return Err(err(E_POINTER));
        }
        Err(err(SET_NOT_FOUND))
    }
    fn KsMethod(
        &self,
        _p: *const KSIDENTIFIER,
        _l: u32,
        _d: *mut core::ffi::c_void,
        _dl: u32,
        ret: *mut u32,
    ) -> Result<()> {
        if ret.is_null() {
            return Err(err(E_POINTER));
        }
        Err(err(SET_NOT_FOUND))
    }
    fn KsEvent(
        &self,
        _p: *const KSIDENTIFIER,
        _l: u32,
        _d: *mut core::ffi::c_void,
        _dl: u32,
        ret: *mut u32,
    ) -> Result<()> {
        if ret.is_null() {
            return Err(err(E_POINTER));
        }
        Err(err(SET_NOT_FOUND))
    }
}

/// Never let a panic cross the COM boundary.
pub fn guard<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(_) => {
            vlog!("panic caught at COM boundary");
            Err(err(windows::Win32::Foundation::E_UNEXPECTED))
        }
    }
}

pub const KS_NOT_FOUND: HRESULT = SET_NOT_FOUND;
