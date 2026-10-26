// SPDX-License-Identifier: GPL-3.0-or-later
//! End to end for the phone microphone (round 25): real ControlServer + fake phone microphone +
//! the mic manager, with a fake `MicSink` that records what would reach the virtual cable.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nd_agent::mic::{ManagerConfig, MicManager, Timing};
use nd_config::{MicControlReq, MicSettings};
use nd_core::mock::{MockInputInjector, MockLauncher};
use nd_core::{Action, ActionRouter, InputContext, MicActions};
use nd_fakephone::store::ClientIdentity;
use nd_fakephone::{FakeMic, FakePhone, FakePhoneConfig, MicMode};
use nd_platform::{
    AudioEndpoint, DefaultDevices, DeviceFlow, DeviceRole, MicOutputDevice, MicSink, MicStream, PlatformError,
};
use nd_transport::{ControlServer, HostIdentity, MemoryTrustStore, PeerInfo, TrustStore};
use tokio::sync::Notify;

// ------------------------------------------------------------------------------- fake sink

const RATE: usize = 48_000;

#[derive(Default)]
struct FakeSink {
    fail_open: AtomicBool,
    samples: Arc<Mutex<Vec<i16>>>,
    opens: AtomicUsize,
    open_streams: Arc<AtomicUsize>,
    shutdowns: AtomicUsize,
    defaults: Mutex<Option<DefaultDevices>>,
    endpoints: Mutex<Vec<AudioEndpoint>>,
    set_calls: Mutex<Vec<(String, DeviceRole)>>,
}

struct FakeStream {
    samples: Arc<Mutex<Vec<i16>>>,
    open_streams: Arc<AtomicUsize>,
    queue: usize,
    at: Instant,
}

impl FakeStream {
    fn drain(&mut self) {
        let played = (self.at.elapsed().as_secs_f64() * RATE as f64) as usize;
        if played > 0 {
            self.queue = self.queue.saturating_sub(played);
            self.at = Instant::now();
        }
    }
}

impl MicStream for FakeStream {
    fn write(&mut self, s: &[i16]) -> nd_platform::Result<usize> {
        self.drain();
        self.samples.lock().unwrap().extend_from_slice(s);
        self.queue += s.len();
        Ok(s.len())
    }
    fn queued(&self) -> nd_platform::Result<usize> {
        let played = (self.at.elapsed().as_secs_f64() * RATE as f64) as usize;
        Ok(self.queue.saturating_sub(played))
    }
}

impl Drop for FakeStream {
    fn drop(&mut self) {
        self.open_streams.fetch_sub(1, Ordering::SeqCst);
    }
}

impl MicSink for FakeSink {
    fn backend_name(&self) -> &'static str {
        "fake"
    }
    fn devices(&self) -> nd_platform::Result<Vec<MicOutputDevice>> {
        Ok(vec![MicOutputDevice {
            id: "cable".into(),
            name: "CABLE Input (VB-Audio Virtual Cable)".into(),
            virtual_cable: true,
            capture_name: "CABLE Output (VB-Audio Virtual Cable)".into(),
        }])
    }
    fn open(&self, _device: &str, rate: u32) -> nd_platform::Result<Box<dyn MicStream>> {
        assert_eq!(rate as usize, RATE);
        if self.fail_open.load(Ordering::SeqCst) {
            return Err(PlatformError::Other("no virtual cable found".into()));
        }
        self.opens.fetch_add(1, Ordering::SeqCst);
        self.open_streams.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(FakeStream {
            samples: self.samples.clone(),
            open_streams: self.open_streams.clone(),
            queue: 0,
            at: Instant::now(),
        }))
    }
    fn defaults(&self) -> nd_platform::Result<DefaultDevices> {
        self.defaults.lock().unwrap().clone().ok_or(PlatformError::Unsupported("default audio devices"))
    }
    fn endpoints(&self) -> nd_platform::Result<Vec<AudioEndpoint>> {
        Ok(self.endpoints.lock().unwrap().clone())
    }
    fn set_default(&self, id: &str, role: DeviceRole) -> nd_platform::Result<()> {
        self.set_calls.lock().unwrap().push((id.to_string(), role));
        let mut d = self.defaults.lock().unwrap();
        if let Some(d) = d.as_mut() {
            match role {
                DeviceRole::Console => d.render = id.to_string(),
                DeviceRole::Communications => d.render_comm = id.to_string(),
            }
        }
        Ok(())
    }
    fn shutdown(&self) {
        self.shutdowns.fetch_add(1, Ordering::SeqCst);
    }
}

// ------------------------------------------------------------------------------------- rig

struct Rig {
    manager: Arc<MicManager>,
    sink: Arc<FakeSink>,
    settings: Arc<Mutex<MicSettings>>,
    poke: Arc<Notify>,
    handle: nd_transport::ServerHandle,
    addr: SocketAddr,
    identity: Arc<HostIdentity>,
    trust: Arc<MemoryTrustStore>,
    dir: std::path::PathBuf,
    shutdown: tokio::sync::watch::Sender<bool>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.manager.shutdown();
        let _ = self.shutdown.send(true);
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

fn mic_settings(enabled: bool) -> MicSettings {
    MicSettings { enabled, device: String::new(), voice_processing: true, gain_db: 0, guard_defaults: true }
}

async fn rig(enabled: bool, sink: FakeSink, timing: Timing) -> Rig {
    let dir = std::env::temp_dir().join(format!("nd-agent-mic-test-{}", uuid::Uuid::new_v4()));
    let identity = Arc::new(HostIdentity::load_or_create(&dir).unwrap());
    let trust = Arc::new(MemoryTrustStore::new(identity.fingerprint()));
    let mut server =
        ControlServer::bind_tls("127.0.0.1:0", identity.clone(), trust.clone() as Arc<dyn TrustStore>).await.unwrap();
    let addr = server.local_addr();
    let handle = server.handle();
    let audio_rx = server.take_audio_receiver().unwrap();
    let (_task, shutdown) = server.spawn();

    let sink = Arc::new(sink);
    let settings = Arc::new(Mutex::new(mic_settings(enabled)));
    let poke = Arc::new(Notify::new());
    let (s1, s2) = (settings.clone(), settings.clone());
    let manager = MicManager::start(ManagerConfig {
        server: handle.clone(),
        audio_rx,
        sink: sink.clone(),
        settings: Arc::new(move || s1.lock().unwrap().clone()),
        set_enabled: Arc::new(move |on| s2.lock().unwrap().enabled = on),
        poke: poke.clone(),
        config_dir: dir.clone(),
        timing,
    });
    Rig { manager, sink, settings, poke, handle, addr, identity, trust, dir, shutdown }
}

fn fast() -> Timing {
    Timing { recheck: Duration::from_millis(200), guard: Duration::from_millis(100) }
}

async fn phone(r: &Rig, name: &str, mode: MicMode) -> (Arc<FakePhone>, FakeMic) {
    let cid = Arc::new(ClientIdentity::generate(name).unwrap());
    r.trust.pin(&PeerInfo {
        fp: cid.fp,
        device_id: name.into(),
        device_name: name.into(),
        device_model: "Fake".into(),
        addr: "127.0.0.1:1".parse().unwrap(),
        link: nd_transport::Link::Usb,
    });
    let config = FakePhoneConfig {
        identity: Some(cid),
        pin: Some(r.identity.fingerprint()),
        require_layout: false,
        device_id: name.into(),
        device_name: name.into(),
        ..FakePhoneConfig::default()
    }
    .with_mic();
    let phone = Arc::new(FakePhone::connect_with_config(r.addr, config).await.unwrap());
    let mic = FakeMic::attach(phone.clone(), mode).await.unwrap();
    (phone, mic)
}

async fn wait_for(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let t = Instant::now();
    while !f() {
        assert!(t.elapsed() < Duration::from_secs(secs), "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn state(r: &Rig) -> String {
    r.manager.status_json()["state"].as_str().unwrap_or("").to_string()
}

fn recorded(r: &Rig) -> usize {
    r.sink.samples.lock().unwrap().len()
}

/// Peak of the samples recorded after index `from`.
fn peak_since(r: &Rig, from: usize) -> i32 {
    r.sink.samples.lock().unwrap()[from..].iter().map(|s| i32::from(*s).abs()).max().unwrap_or(0)
}

fn router(m: &Arc<MicManager>) -> ActionRouter {
    ActionRouter::new(Arc::new(MockInputInjector::new()), Arc::new(MockLauncher::new())).with_mic(m.clone())
}

// ------------------------------------------------------------------------------------ tests

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn on_streams_sine_mute_zeros_off_stops() {
    let r = rig(false, FakeSink::default(), fast()).await;
    let (_phone, mic) = phone(&r, "mic1", MicMode::Normal).await;
    let stats = mic.stats();
    let router = router(&r.manager);

    // Off: the phone is left alone.
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(stats.starts.load(Ordering::SeqCst), 0);
    let st = r.manager.status_json();
    assert_eq!(st["state"], "off", "{st}");
    assert_eq!(st["on"], false);
    assert_eq!(st["platform"], if cfg!(windows) { "windows" } else { "linux" });

    // A deck key switches it on (the real path: router -> MicActions -> settings).
    router.execute(&Action::MicToggle, &InputContext::default()).unwrap();
    tokio::time::timeout(Duration::from_secs(1), r.poke.notified()).await.expect("poked");
    wait_for("live", 5, || state(&r) == "live").await;
    assert_eq!(stats.starts.load(Ordering::SeqCst), 1);
    assert!(stats.voice_processing.load(Ordering::SeqCst));
    assert_eq!(stats.last_rate.load(Ordering::SeqCst), 48_000);
    assert_eq!(r.sink.opens.load(Ordering::SeqCst), 1);

    let snap = r.manager.snapshot();
    assert!(snap.on && snap.live && !snap.muted);

    // Samples: the sine, 440 Hz at the phone's amplitude.
    wait_for("1 s of audio", 5, || recorded(&r) > RATE).await;
    {
        let s = r.sink.samples.lock().unwrap();
        let win = &s[RATE / 2..RATE / 2 + RATE / 10]; // 100 ms
        let peak = win.iter().map(|v| i32::from(*v).abs()).max().unwrap();
        assert!((11_000..=12_100).contains(&peak), "peak {peak}");
        let crossings = win.windows(2).filter(|w| w[0] < 0 && w[1] >= 0).count();
        assert!((42..=46).contains(&crossings), "{crossings} rising crossings in 100 ms");
    }

    // Status contents.
    let st = r.manager.status_json();
    assert_eq!(st["phone"]["name"], "mic1");
    assert_eq!(st["phone"]["link"], "usb");
    assert_eq!(st["phone"]["state"], "streaming");
    assert_eq!(st["phone"]["sample_rate"], 48_000);
    assert_eq!(st["output"]["backend"], "fake");
    assert_eq!(st["output"]["device_name"], "CABLE Input (VB-Audio Virtual Cable)");
    assert_eq!(st["output"]["capture_name"], "CABLE Output (VB-Audio Virtual Cable)");
    assert_eq!(st["devices"][0]["virtual_cable"], true);
    assert!(st["level"].as_f64().unwrap() > 0.1, "{st}");
    wait_for("kbps", 4, || r.manager.status_json()["stats"]["kbps"].as_f64().unwrap_or(0.0) > 500.0).await;
    let st = r.manager.status_json();
    assert!(st["stats"]["buffer_ms"].as_u64().unwrap() <= 150);
    assert_eq!(st["stats"]["underruns"], 0, "{st}");
    assert_eq!(st["guard"]["enabled"], false); // fake sink has no default devices to guard

    // Gain: +6 dB doubles the amplitude.
    r.settings.lock().unwrap().gain_db = 6;
    tokio::time::sleep(Duration::from_millis(600)).await;
    let from = recorded(&r) - RATE / 10;
    assert!(peak_since(&r, from) > 20_000, "gain not applied");
    r.settings.lock().unwrap().gain_db = 0;

    // Mute: the phone keeps streaming, the sink gets zeros.
    assert!(router.execute(&Action::MicMute, &InputContext::default()).is_ok());
    assert!(r.manager.snapshot().muted);
    assert_eq!(r.manager.status_json()["muted"], true);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let from = recorded(&r) - RATE / 10;
    assert_eq!(peak_since(&r, from), 0, "muted but samples still reach the cable");
    assert!(stats.streaming.load(Ordering::SeqCst));
    assert_eq!(r.manager.status_json()["level"], 0.0);

    // Unmute is instant.
    r.manager.control(MicControlReq { muted: Some(false), on: None }).unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    let from = recorded(&r) - RATE / 20;
    assert!(peak_since(&r, from) > 8_000);

    // Off: MicStop, the output is released, the state is off.
    r.manager.control(MicControlReq { on: Some(false), muted: None }).unwrap();
    wait_for("off", 3, || state(&r) == "off").await;
    wait_for("MicStop", 3, || stats.stops.load(Ordering::SeqCst) == 1).await;
    wait_for("stream closed", 3, || r.sink.open_streams.load(Ordering::SeqCst) == 0).await;
    assert!(!r.manager.snapshot().on);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn open_failure_is_no_output_and_phone_is_not_started_until_it_works() {
    let sink = FakeSink::default();
    sink.fail_open.store(true, Ordering::SeqCst);
    let r = rig(true, sink, fast()).await;
    let (_phone, mic) = phone(&r, "mic1", MicMode::Normal).await;
    let stats = mic.stats();

    wait_for("no_output", 5, || state(&r) == "no_output").await;
    let st = r.manager.status_json();
    assert!(st["message"].as_str().unwrap().contains("no virtual cable"), "{st}");
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert_eq!(stats.starts.load(Ordering::SeqCst), 0, "phone started without an output");

    // "Installing VB-Cable": the next re-check opens it without a restart.
    r.sink.fail_open.store(false, Ordering::SeqCst);
    wait_for("live", 6, || state(&r) == "live").await;
    assert_eq!(stats.starts.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_phone_waits_and_toggle_updates_snapshot() {
    let r = rig(false, FakeSink::default(), fast()).await;
    let router = router(&r.manager);
    assert_eq!(r.manager.snapshot(), nd_core::MicSnapshot::default());
    assert_eq!(
        router.execute(&Action::MicToggle, &InputContext::default()).unwrap(),
        nd_core::ActionOutcome::SetState(1)
    );
    let snap = r.manager.snapshot();
    assert!(snap.on && !snap.live);
    wait_for("waiting", 3, || state(&r) == "waiting").await;
    assert!(r.manager.status_json()["phone"].is_null());
    assert_eq!(r.sink.opens.load(Ordering::SeqCst), 0);
    assert_eq!(
        router.execute(&Action::MicToggle, &InputContext::default()).unwrap(),
        nd_core::ActionOutcome::SetState(0)
    );
    assert!(!r.manager.snapshot().on);
    assert_eq!(
        router.execute(&Action::MicMute, &InputContext::default()).unwrap(),
        nd_core::ActionOutcome::SetState(1)
    );
    assert!(r.manager.snapshot().muted);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn phone_without_permission_is_reported() {
    let r = rig(true, FakeSink::default(), fast()).await;
    let (_phone, mic) = phone(&r, "mic1", MicMode::NoPermission).await;
    wait_for("no_permission", 5, || state(&r) == "no_permission").await;
    let st = r.manager.status_json();
    assert!(st["message"].as_str().unwrap().contains("microphone access"), "{st}");
    assert_eq!(st["phone"]["state"], "no_permission");
    assert!(mic.stats().starts.load(Ordering::SeqCst) >= 1);
    assert_eq!(mic.stats().packets_sent.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn newest_phone_wins_and_leaving_goes_back_to_waiting() {
    let r = rig(true, FakeSink::default(), fast()).await;
    let (p1, m1) = phone(&r, "old", MicMode::Normal).await;
    wait_for("first phone live", 5, || state(&r) == "live").await;
    let (p2, m2) = phone(&r, "new", MicMode::Normal).await;
    wait_for("second phone started", 5, || m2.stats().starts.load(Ordering::SeqCst) == 1).await;
    wait_for("first phone stopped", 5, || m1.stats().stops.load(Ordering::SeqCst) == 1).await;
    wait_for("second live", 5, || {
        let st = r.manager.status_json();
        st["state"] == "live" && st["phone"]["name"] == "new"
    })
    .await;
    assert_eq!(r.handle.session_count(), 2);

    // The newest phone leaves: the older one takes over.
    drop(m2);
    drop(p2); // closes the connection
    wait_for("old phone again", 8, || {
        let st = r.manager.status_json();
        st["phone"]["name"] == "old" && m1.stats().starts.load(Ordering::SeqCst) >= 2
    })
    .await;
    drop(m1);
    drop(p1);
    wait_for("waiting", 8, || state(&r) == "waiting").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn guard_puts_the_default_device_back_and_remembers_it() {
    let sink = FakeSink::default();
    let ep = |id: &str, name: &str, cable| AudioEndpoint {
        id: id.into(),
        name: name.into(),
        flow: DeviceFlow::Render,
        virtual_cable: cable,
    };
    *sink.endpoints.lock().unwrap() = vec![ep("spk", "Speakers", false), ep("cable", "CABLE Input", true)];
    let d = |r: &str| DefaultDevices { render: r.into(), render_comm: r.into(), ..Default::default() };
    *sink.defaults.lock().unwrap() = Some(d("spk"));
    let r = rig(false, sink, fast()).await;

    // Remembers the speakers.
    wait_for("memory file", 5, || r.dir.join("mic_defaults.json").exists()).await;
    let saved = std::fs::read_to_string(r.dir.join("mic_defaults.json")).unwrap();
    assert!(saved.contains("Speakers"), "{saved}");
    assert_eq!(r.manager.status_json()["guard"]["enabled"], true);
    assert!(r.sink.set_calls.lock().unwrap().is_empty());

    // Windows (or a cable installer) moves the default to the cable: it goes back.
    *r.sink.defaults.lock().unwrap() = Some(d("cable"));
    wait_for("restore", 5, || r.sink.set_calls.lock().unwrap().len() >= 2).await;
    {
        let calls = r.sink.set_calls.lock().unwrap();
        assert!(calls.iter().all(|(id, _)| id == "spk"));
        assert!(calls.iter().any(|(_, role)| *role == DeviceRole::Console));
        assert!(calls.iter().any(|(_, role)| *role == DeviceRole::Communications));
    }
    wait_for("restored log", 5, || {
        r.manager.status_json()["guard"]["restored"].as_array().is_some_and(|a| a.len() == 2)
    })
    .await;
    let st = r.manager.status_json();
    let first = &st["guard"]["restored"][0];
    assert_eq!(first["flow"], "render");
    assert_eq!(first["from"], "CABLE Input");
    assert_eq!(first["to"], "Speakers");
    assert!(first["at"].as_u64().unwrap() > 1_700_000_000);
    assert_eq!(st["guard"]["warning"], "");

    // Guard off: a cable default is left alone.
    r.settings.lock().unwrap().guard_defaults = false;
    tokio::time::sleep(Duration::from_millis(300)).await;
    *r.sink.defaults.lock().unwrap() = Some(d("cable"));
    let before = r.sink.set_calls.lock().unwrap().len();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(r.sink.set_calls.lock().unwrap().len(), before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_stops_the_phone_and_cleans_the_sink() {
    let r = rig(true, FakeSink::default(), fast()).await;
    let (_phone, mic) = phone(&r, "mic1", MicMode::Normal).await;
    wait_for("live", 5, || state(&r) == "live").await;
    r.manager.shutdown();
    wait_for("MicStop", 3, || mic.stats().stops.load(Ordering::SeqCst) == 1).await;
    assert_eq!(r.sink.open_streams.load(Ordering::SeqCst), 0);
    assert!(r.sink.shutdowns.load(Ordering::SeqCst) >= 1);
}
