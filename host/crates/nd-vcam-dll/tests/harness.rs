// SPDX-License-Identifier: GPL-3.0-or-later
//! In-process harness for the virtual camera DLL (ADR 0005 decision 8). Loads the *built*
//! `notideck_vcam.dll` with LoadLibrary, drives it like the Frame Server would
//! (class factory -> media source -> presentation descriptor -> Start -> RequestSample) and
//! checks the NV12 samples against the placeholder and against frames written through the
//! shared memory. Runs without admin and without any registration; the section falls back to
//! `Local\` (forced by NOTIDECK_VCAM_LOCAL_ONLY so a real install can never interfere).
//!
//! Build the DLL first:  cargo build -p nd-vcam-dll --target x86_64-pc-windows-gnu
#![cfg(windows)]

use std::ffi::c_void;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use nd_vcam::shm::{nv12_len, CLSID_STR, FPS, MODES};
use nd_vcam::ShmMapping;
use windows::core::{w, Interface, GUID, HRESULT, PCWSTR};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoInitializeEx, IClassFactory, COINIT_MULTITHREADED};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

type GetClassObject = unsafe extern "system" fn(*const GUID, *const GUID, *mut *mut c_void) -> HRESULT;
type CanUnloadNow = unsafe extern "system" fn() -> HRESULT;

static SERIAL: Mutex<()> = Mutex::new(());

struct Loaded {
    get_class_object: GetClassObject,
    can_unload_now: CanUnloadNow,
    _module: HMODULE,
}

fn clsid() -> GUID {
    let hex: String = CLSID_STR.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    GUID::from_u128(u128::from_str_radix(&hex, 16).unwrap())
}

#[allow(clippy::missing_transmute_annotations)]
fn load_dll() -> Loaded {
    std::env::set_var(nd_vcam::win::LOCAL_ONLY_ENV, "1");
    let exe = std::env::current_exe().unwrap();
    let dll = exe.parent().unwrap().parent().unwrap().join("notideck_vcam.dll");
    assert!(
        dll.is_file(),
        "{} not found: run `cargo build -p nd-vcam-dll --target x86_64-pc-windows-gnu` first",
        dll.display()
    );
    // Load a private copy from %TEMP% (a \\wsl$ path can be refused, and it keeps the build output unlocked).
    if let Ok(rd) = std::fs::read_dir(std::env::temp_dir()) {
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().starts_with("notideck_vcam-harness-") {
                let _ = std::fs::remove_file(e.path()); // stale copies of earlier runs; locked ones stay
            }
        }
    }
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let copy = std::env::temp_dir().join(format!("notideck_vcam-harness-{}-{n}.dll", std::process::id()));
    std::fs::copy(&dll, &copy).unwrap();
    let wide: Vec<u16> = copy.to_string_lossy().encode_utf16().chain(Some(0)).collect();
    unsafe {
        let module = LoadLibraryW(PCWSTR(wide.as_ptr())).expect("LoadLibraryW");
        let get = GetProcAddress(module, windows::core::s!("DllGetClassObject")).expect("DllGetClassObject export");
        let can = GetProcAddress(module, windows::core::s!("DllCanUnloadNow")).expect("DllCanUnloadNow export");
        Loaded {
            get_class_object: std::mem::transmute::<_, GetClassObject>(get),
            can_unload_now: std::mem::transmute::<_, CanUnloadNow>(can),
            _module: module,
        }
    }
}

fn start_mf() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        MFStartup(MF_VERSION, MFSTARTUP_FULL).expect("MFStartup");
    }
}

fn factory(dll: &Loaded) -> IClassFactory {
    let mut raw = std::ptr::null_mut();
    unsafe {
        (dll.get_class_object)(&clsid(), &IClassFactory::IID, &mut raw).ok().expect("DllGetClassObject");
        IClassFactory::from_raw(raw)
    }
}

/// Wait for an event of type `want` on a generator, ignoring (but logging) others.
fn wait_event(gen: &IMFMediaEventGenerator, want: MF_EVENT_TYPE, what: &str) -> IMFMediaEvent {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match unsafe { gen.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
            Ok(ev) => {
                let t = unsafe { ev.GetType().unwrap() };
                if t == want.0 as u32 {
                    let st = unsafe { ev.GetStatus().unwrap() };
                    assert!(st.is_ok(), "{what}: event status {st:?}");
                    return ev;
                }
                eprintln!("(skipping event type {t} while waiting for {what})");
            }
            Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => {
                assert!(Instant::now() < deadline, "timeout waiting for {what}");
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => panic!("GetEvent while waiting for {what}: {e}"),
        }
    }
}

fn frame_size(t: &IMFMediaType) -> (u32, u32) {
    let v = unsafe { t.GetUINT64(&MF_MT_FRAME_SIZE).unwrap() };
    ((v >> 32) as u32, v as u32)
}

fn request_and_read(stream: &IMFMediaStream, events: &IMFMediaEventGenerator) -> Vec<u8> {
    unsafe { stream.RequestSample(None).expect("RequestSample") };
    let ev = wait_event(events, MEMediaSample, "MEMediaSample");
    let value = unsafe { ev.GetValue().unwrap() };
    let unk = windows::core::IUnknown::try_from(&value).expect("sample IUnknown");
    let sample: IMFSample = unk.cast().unwrap();
    unsafe {
        assert!(sample.GetSampleTime().unwrap() > 0);
        assert_eq!(sample.GetSampleDuration().unwrap(), 10_000_000 / FPS as i64);
        assert_eq!(sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap(), 1);
        let buf = sample.ConvertToContiguousBuffer().unwrap();
        let (mut p, mut max, mut cur) = (std::ptr::null_mut(), 0u32, 0u32);
        buf.Lock(&mut p, Some(&mut max), Some(&mut cur)).unwrap();
        let out = std::slice::from_raw_parts(p, cur as usize).to_vec();
        buf.Unlock().unwrap();
        out
    }
}

fn pattern(w: u32, h: u32) -> Vec<u8> {
    (0..nv12_len(w, h)).map(|i| ((i * 7 + i / 640 + 3) % 256) as u8).collect()
}

#[test]
fn media_source_end_to_end() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dll = load_dll();
    start_mf();

    // Only the right CLSID is served.
    let mut raw = std::ptr::null_mut();
    let bogus = GUID::from_u128(1);
    let hr = unsafe { (dll.get_class_object)(&bogus, &IClassFactory::IID, &mut raw) };
    assert_eq!(hr, HRESULT(0x80040111u32 as i32)); // CLASS_E_CLASSNOTAVAILABLE

    let factory = factory(&dll);
    let source: IMFMediaSource = unsafe { factory.CreateInstance(None) }.expect("CreateInstance(IMFMediaSource)");
    assert_eq!(unsafe { (dll.can_unload_now)() }, HRESULT(1), "objects alive -> S_FALSE");
    let ex: IMFMediaSourceEx = source.cast().expect("IMFMediaSourceEx");
    unsafe {
        source.cast::<windows::Win32::Media::KernelStreaming::IKsControl>().expect("IKsControl");
        source.cast::<IMFGetService>().expect("IMFGetService");
        source.cast::<IMFSampleAllocatorControl>().expect("IMFSampleAllocatorControl");
        source.cast::<IMFActivate>().expect("IMFActivate");
        assert_eq!(source.GetCharacteristics().unwrap(), MFMEDIASOURCE_IS_LIVE.0 as u32);
        // Source attributes: associated sources + CLSID.
        let sa = ex.GetSourceAttributes().unwrap();
        assert_eq!(sa.GetUINT32(&MF_VIRTUALCAMERA_PROVIDE_ASSOCIATED_CAMERA_SOURCES).unwrap(), 1);
        assert_eq!(sa.GetGUID(&MFT_TRANSFORM_CLSID_Attribute).unwrap(), clsid());
        // Stream attributes.
        let st = ex.GetStreamAttributes(0).unwrap();
        assert_eq!(
            st.GetGUID(&MF_DEVICESTREAM_STREAM_CATEGORY).unwrap(),
            windows::Win32::Media::KernelStreaming::PINNAME_VIDEO_CAPTURE
        );
        assert_eq!(st.GetUINT32(&MF_DEVICESTREAM_FRAMESERVER_SHARED).unwrap(), 1);
        assert_eq!(st.GetUINT32(&MF_DEVICESTREAM_STREAM_ID).unwrap(), 0);
    }

    // Presentation descriptor: one stream, three NV12 types.
    let pd = unsafe { source.CreatePresentationDescriptor() }.unwrap();
    unsafe {
        assert_eq!(pd.GetStreamDescriptorCount().unwrap(), 1);
        let (mut selected, mut desc) = (windows::Win32::Foundation::BOOL(0), None);
        pd.GetStreamDescriptorByIndex(0, &mut selected, &mut desc).unwrap();
        let desc = desc.unwrap();
        let handler = desc.GetMediaTypeHandler().unwrap();
        assert_eq!(handler.GetMajorType().unwrap(), MFMediaType_Video);
        assert_eq!(handler.GetMediaTypeCount().unwrap(), 3);
        let mut chosen = None;
        for (i, &(w, h)) in MODES.iter().enumerate() {
            let t = handler.GetMediaTypeByIndex(i as u32).unwrap();
            assert_eq!(t.GetGUID(&MF_MT_SUBTYPE).unwrap(), MFVideoFormat_NV12);
            assert_eq!(frame_size(&t), (w, h));
            assert_eq!(t.GetUINT64(&MF_MT_FRAME_RATE).unwrap(), ((FPS as u64) << 32) | 1);
            assert_eq!(t.GetUINT32(&MF_MT_INTERLACE_MODE).unwrap(), MFVideoInterlace_Progressive.0 as u32);
            assert_eq!(t.GetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT).unwrap(), 1);
            assert_eq!(t.GetUINT32(&MF_MT_DEFAULT_STRIDE).unwrap(), w);
            assert_eq!(t.GetUINT64(&MF_MT_PIXEL_ASPECT_RATIO).unwrap(), (1u64 << 32) | 1);
            if (w, h) == (640, 360) {
                chosen = Some(t);
            }
        }
        handler.SetCurrentMediaType(&chosen.unwrap()).unwrap();
        pd.SelectStream(0).unwrap();
    }

    // Start -> MENewStream + MESourceStarted.
    let events: IMFMediaEventGenerator = source.cast().unwrap();
    unsafe { source.Start(&pd, std::ptr::null(), &windows::core::PROPVARIANT::default()).expect("Start") };
    let ev = wait_event(&events, MENewStream, "MENewStream");
    let stream: IMFMediaStream =
        windows::core::IUnknown::try_from(&unsafe { ev.GetValue().unwrap() }).unwrap().cast().unwrap();
    wait_event(&events, MESourceStarted, "MESourceStarted");
    let stream_events: IMFMediaEventGenerator = stream.cast().unwrap();
    wait_event(&stream_events, MEStreamStarted, "MEStreamStarted");
    unsafe {
        assert_eq!(stream.GetStreamDescriptor().unwrap().GetStreamIdentifier().unwrap(), 0);
        assert!(stream.GetMediaSource().is_ok());
        stream.cast::<IMFMediaStream2>().unwrap();
    }

    // 1) No producer: placeholder.
    let placeholder = notideck_vcam::placeholder::render(640, 360);
    let first = request_and_read(&stream, &stream_events);
    assert_eq!(first.len(), nv12_len(640, 360));
    assert_eq!(first[0], notideck_vcam::placeholder::BG_Y, "top-left pixel is graphite");
    assert_eq!(first, placeholder);

    // The app side opens the (Local\) section the DLL created and sees the consumer.
    let map = ShmMapping::open().expect("app side can open the section");
    let shm = map.shm();
    assert_eq!(shm.wanted(), Some((640, 360, FPS)));

    // 2) A frame written by the app comes through unchanged.
    let frame = pattern(640, 360);
    assert!(shm.write_frame(640, 360, 42, &frame));
    assert_eq!(request_and_read(&stream, &stream_events), frame);
    // Unchanged -> the same frame again.
    assert_eq!(request_and_read(&stream, &stream_events), frame);
    // A newer frame replaces it.
    let frame2: Vec<u8> = frame.iter().map(|b| b.wrapping_add(1)).collect();
    assert!(shm.write_frame(640, 360, 43, &frame2));
    assert_eq!(request_and_read(&stream, &stream_events), frame2);

    // 3) Wrong-size frame -> placeholder.
    assert!(shm.write_frame(1280, 720, 44, &pattern(1280, 720)));
    assert_eq!(request_and_read(&stream, &stream_events), placeholder);

    // Pacing: 6 requests at 30 fps take roughly 5 intervals, never a burst.
    let t0 = Instant::now();
    for _ in 0..6 {
        unsafe { stream.RequestSample(None).unwrap() };
    }
    for _ in 0..6 {
        wait_event(&stream_events, MEMediaSample, "paced sample");
    }
    let el = t0.elapsed();
    assert!(el >= Duration::from_millis(120), "samples were not paced: {el:?}");

    // Stop -> consumer inactive, events delivered.
    unsafe { source.Stop().unwrap() };
    wait_event(&events, MESourceStopped, "MESourceStopped");
    wait_event(&stream_events, MEStreamStopped, "MEStreamStopped");
    assert_eq!(shm.wanted(), None, "consumer_active cleared on Stop");

    // Restart at another size, then Shutdown clears the consumer too.
    unsafe {
        let pd2 = source.CreatePresentationDescriptor().unwrap();
        let (mut sel, mut d) = (windows::Win32::Foundation::BOOL(0), None);
        pd2.GetStreamDescriptorByIndex(0, &mut sel, &mut d).unwrap();
        let h = d.unwrap().GetMediaTypeHandler().unwrap();
        h.SetCurrentMediaType(&h.GetMediaTypeByIndex(1).unwrap()).unwrap();
        pd2.SelectStream(0).unwrap();
        source.Start(&pd2, std::ptr::null(), &windows::core::PROPVARIANT::default()).unwrap();
    }
    wait_event(&events, MENewStream, "MENewStream (restart)");
    wait_event(&events, MESourceStarted, "MESourceStarted (restart)");
    assert_eq!(shm.wanted(), Some((1280, 720, FPS)));
    unsafe { source.Shutdown().unwrap() };
    assert_eq!(shm.wanted(), None, "consumer_active cleared on Shutdown");
    assert!(unsafe { source.CreatePresentationDescriptor() }.is_err(), "after Shutdown");

    drop((stream, stream_events, events, ex, source, factory));
}

#[test]
fn activator_path_and_1080p() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dll = load_dll();
    start_mf();
    let factory = factory(&dll);
    // The Frame Server asks for IMFActivate and then activates the real source.
    let activate: IMFActivate = unsafe { factory.CreateInstance(None) }.expect("CreateInstance(IMFActivate)");
    let source: IMFMediaSourceEx = unsafe { activate.ActivateObject() }.expect("ActivateObject");
    unsafe {
        // Default (current) type is the first mode: 1080p.
        let pd = source.CreatePresentationDescriptor().unwrap();
        let (mut sel, mut d) = (windows::Win32::Foundation::BOOL(0), None);
        pd.GetStreamDescriptorByIndex(0, &mut sel, &mut d).unwrap();
        let cur = d.unwrap().GetMediaTypeHandler().unwrap().GetCurrentMediaType().unwrap();
        assert_eq!(frame_size(&cur), MODES[0]);
        pd.SelectStream(0).unwrap();
        let events: IMFMediaEventGenerator = source.cast().unwrap();
        source.Start(&pd, std::ptr::null(), &windows::core::PROPVARIANT::default()).unwrap();
        let ev = wait_event(&events, MENewStream, "MENewStream");
        let stream: IMFMediaStream =
            windows::core::IUnknown::try_from(&ev.GetValue().unwrap()).unwrap().cast().unwrap();
        let se: IMFMediaEventGenerator = stream.cast().unwrap();
        let s = request_and_read(&stream, &se);
        assert_eq!(s.len(), nv12_len(1920, 1080));
        assert_eq!(s[0], notideck_vcam::placeholder::BG_Y);
        source.Shutdown().unwrap();
    }
    let _ = w!("");
}
