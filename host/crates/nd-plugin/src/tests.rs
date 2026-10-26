// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use crate::discovery::Persisted;
use crate::host::HostError;
use futures_util::{SinkExt, StreamExt};
use nd_core::model::ControlKind;
use nd_core::plugin::{PluginControl, PluginEvent, PluginEventKind, PluginInstance, PluginSettings};
use serde_json::{json, Value};
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

const MANIFEST: &str = r#"{
  "id": "dev.test.one", "name": "One", "version": "1.0", "protocol": 1,
  "entry": "run.sh",
  "actions": [{"id": "a", "name": "A", "settings": [{"key": "k", "label": "K", "type": "text"}]}]
}"#;

fn manifest_with(edit: impl FnOnce(&mut Value)) -> String {
    let mut v: Value = serde_json::from_str(MANIFEST).unwrap();
    edit(&mut v);
    v.to_string()
}

fn invalid(json: &str) -> String {
    match Manifest::parse(json) {
        Err(ManifestError::Invalid(m)) => m,
        other => panic!("expected Invalid, got {other:?}"),
    }
}

// ---- manifest ---------------------------------------------------------------------------------

#[test]
fn manifest_good() {
    let m = Manifest::parse(MANIFEST).unwrap();
    assert_eq!(m.id, "dev.test.one");
    assert_eq!(m.actions[0].controls, vec![ControlKind::Key]);
    assert_eq!(m.action("a").unwrap().settings[0].key, "k");
}

#[test]
fn manifest_bad_rules() {
    assert!(matches!(Manifest::parse("{nope"), Err(ManifestError::Json(_))));
    for id in ["ab", "1abc", "Abc", "a b c", "a/b"] {
        invalid(&manifest_with(|v| v["id"] = json!(id)));
    }
    invalid(&manifest_with(|v| v["id"] = json!("a".repeat(65))));
    invalid(&manifest_with(|v| v["protocol"] = json!(2)));
    invalid(&manifest_with(|v| v["name"] = json!("")));
    invalid(&manifest_with(|v| v["actions"] = json!([])));
    let many: Vec<Value> = (0..65).map(|i| json!({"id": format!("a{i}"), "name": "x"})).collect();
    invalid(&manifest_with(|v| v["actions"] = json!(many)));
    invalid(&manifest_with(|v| v["actions"] = json!([{"id": "a", "name": "A"}, {"id": "a", "name": "B"}])));
    invalid(&manifest_with(|v| v["actions"][0]["id"] = json!("Bad Id")));
    invalid(&manifest_with(|v| {
        v["actions"][0]["settings"] =
            json!([{"key": "k", "label": "K", "type": "text"}, {"key": "k", "label": "L", "type": "toggle"}])
    }));
    invalid(&manifest_with(|v| v["actions"][0]["settings"] = json!([{"key": "k", "label": "K", "type": "select"}])));
    invalid(&manifest_with(|v| v["actions"][0]["controls"] = json!([])));
    invalid(&manifest_with(|v| v["actions"][0]["triggers"] = json!([])));
    for bad in ["../x.svg", "/abs/x.svg", "a/../../x", "C:\\x.svg", "\\\\srv\\x"] {
        invalid(&manifest_with(|v| v["icon"] = json!(bad)));
        invalid(&manifest_with(|v| v["actions"][0]["icon"] = json!(bad)));
        invalid(&manifest_with(|v| v["entry"] = json!(bad)));
        invalid(&manifest_with(|v| v["entry"] = json!({"linux": [bad]})));
    }
    invalid(&manifest_with(|v| v["entry"] = json!([])));
    // A bare program name for PATH is fine.
    Manifest::parse(&manifest_with(|v| v["entry"] = json!(["node", "main.js"]))).unwrap();
}

#[test]
fn manifest_face() {
    use nd_core::model::ControlFace;
    assert_eq!(Manifest::parse(MANIFEST).unwrap().actions[0].face, ControlFace::Default);
    let m = Manifest::parse(&manifest_with(|v| v["actions"][0]["face"] = json!("stream"))).unwrap();
    assert_eq!(m.actions[0].face, ControlFace::Stream);
    let m = Manifest::parse(&manifest_with(|v| v["actions"][0]["face"] = json!("media"))).unwrap();
    assert_eq!(m.actions[0].face, ControlFace::Media);
    assert!(Manifest::parse(&manifest_with(|v| v["actions"][0]["face"] = json!("sparkly"))).is_err());
    // The default face is left out of the serialised manifest (the UI reads `face` only when set).
    let out = serde_json::to_value(Manifest::parse(MANIFEST).unwrap()).unwrap();
    assert!(out["actions"][0].get("face").is_none());
    let out =
        serde_json::to_value(Manifest::parse(&manifest_with(|v| v["actions"][0]["face"] = json!("stream"))).unwrap())
            .unwrap();
    assert_eq!(out["actions"][0]["face"], "stream");
}

#[test]
fn command_for_os_selection() {
    let m = Manifest::parse(&manifest_with(|v| v["entry"] = json!("run.sh"))).unwrap();
    assert_eq!(m.command_for_os(), Some(vec!["run.sh".to_string()]));
    let m = Manifest::parse(&manifest_with(|v| {
        v["entry"] = json!({"windows": "w.exe", "linux": ["node", "l.js"], "default": "d"})
    }))
    .unwrap();
    let want = if cfg!(windows) { vec!["w.exe".to_string()] } else { vec!["node".to_string(), "l.js".to_string()] };
    assert_eq!(m.command_for_os(), Some(want));
    let m = Manifest::parse(&manifest_with(|v| v["entry"] = json!({"default": "d"}))).unwrap();
    assert_eq!(m.command_for_os(), Some(vec!["d".to_string()]));
    let other = if cfg!(windows) { "linux" } else { "windows" };
    let m = Manifest::parse(&manifest_with(|v| v["entry"] = json!({ other: "x" }))).unwrap();
    assert_eq!(m.command_for_os(), None);
}

// ---- host fixture -----------------------------------------------------------------------------

struct Fx {
    host: Arc<PluginHost>,
    dir: tempfile::TempDir,
    events: tokio::sync::mpsc::UnboundedReceiver<HostEvent>,
    pokes: Arc<AtomicUsize>,
    imports: Arc<AtomicUsize>,
}

fn write_plugin(root: &std::path::Path, folder: &str, manifest: &str) {
    let d = root.join(folder);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("manifest.json"), manifest).unwrap();
}

fn config(
    dir: &tempfile::TempDir,
) -> (HostConfig, tokio::sync::mpsc::UnboundedReceiver<HostEvent>, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let pokes = Arc::new(AtomicUsize::new(0));
    let imports = Arc::new(AtomicUsize::new(0));
    let (p, i) = (pokes.clone(), imports.clone());
    let cfg = HostConfig {
        bundled_dirs: vec![dir.path().join("bundled")],
        user_dir: dir.path().join("user"),
        state_file: dir.path().join("plugins.json"),
        icons: Arc::new(move |png: &[u8]| {
            i.fetch_add(1, Ordering::SeqCst);
            Some(format!("icon{}", png.len()))
        }),
        poke: Arc::new(move || {
            p.fetch_add(1, Ordering::SeqCst);
        }),
        events: tx,
        agent_version: "9.9".into(),
    };
    (cfg, rx, pokes, imports)
}

/// A host with one bundled plugin `dev.test.one`, no processes spawned.
fn fixture() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    write_plugin(&dir.path().join("bundled"), "dev.test.one", MANIFEST);
    let (cfg, events, pokes, imports) = config(&dir);
    let host = PluginHost::start_inner(cfg, None).unwrap();
    Fx { host, dir, events, pokes, imports }
}

fn inst(ctx: &str, k: &str) -> PluginInstance {
    let mut settings = PluginSettings::new();
    settings.insert("k".into(), json!(k));
    PluginInstance {
        context: ctx.into(),
        plugin: "dev.test.one".into(),
        action: "a".into(),
        settings,
        control: ControlKind::Key,
        state: 1,
        value: 0.5,
    }
}

type Client = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(port: u16) -> Client {
    tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/")).await.unwrap().0
}

async fn send(c: &mut Client, v: Value) {
    c.send(Message::Text(v.to_string())).await.unwrap();
}

async fn recv(c: &mut Client) -> Value {
    loop {
        let m =
            tokio::time::timeout(Duration::from_secs(3), c.next()).await.expect("timeout").expect("closed").unwrap();
        if let Message::Text(t) = m {
            return serde_json::from_str(&t).unwrap();
        }
    }
}

async fn closed(c: &mut Client) -> bool {
    matches!(
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                match c.next().await {
                    None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return true,
                    _ => {}
                }
            }
        })
        .await,
        Ok(true)
    )
}

async fn register(fx: &Fx) -> Client {
    let mut c = connect(fx.host.port()).await;
    let token = fx.host.token("dev.test.one").unwrap();
    send(&mut c, json!({"jsonrpc": "2.0", "id": 1, "method": "register", "params": {"pluginId": "dev.test.one", "token": token, "protocol": 1}})).await;
    let r = recv(&mut c).await;
    assert_eq!(r["id"], 1);
    assert_eq!(r["result"]["protocol"], 1);
    assert_eq!(r["result"]["agent"]["version"], "9.9");
    c
}

async fn call(c: &mut Client, id: u64, method: &str, params: Value) -> Value {
    send(c, json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})).await;
    recv(c).await
}

fn status(fx: &Fx) -> PluginStatus {
    fx.host.list().into_iter().find(|p| p.id == "dev.test.one").unwrap().status
}

// ---- discovery / state ------------------------------------------------------------------------

#[tokio::test]
async fn discovery_override_invalid_and_state() {
    let dir = tempfile::tempdir().unwrap();
    let bundled = dir.path().join("bundled");
    let user = dir.path().join("user");
    write_plugin(&bundled, "dev.test.one", MANIFEST);
    write_plugin(
        &bundled,
        "dev.test.two",
        &MANIFEST.replace("dev.test.one", "dev.test.two").replace("\"One\"", "\"Two\""),
    );
    // User plugin with the same id as a bundled one wins; it asks for a permission.
    write_plugin(
        &user,
        "dev.test.one",
        &manifest_with(|v| {
            v["name"] = json!("One (user)");
            v["permissions"] = json!(["network"]);
        }),
    );
    write_plugin(&user, "broken", "{ not json");
    write_plugin(&user, "badrule", &manifest_with(|v| v["protocol"] = json!(7)));
    std::fs::create_dir_all(user.join("empty")).unwrap();
    let (cfg, _rx, _p, _i) = config(&dir);
    let host = PluginHost::start_inner(cfg, None).unwrap();
    let list = host.list();
    let get = |id: &str| list.iter().find(|p| p.id == id).unwrap_or_else(|| panic!("no {id}: {list:?}")).clone();
    assert_eq!(list.len(), 4);
    let one = get("dev.test.one");
    assert_eq!(one.name, "One (user)");
    assert!(!one.bundled);
    assert_eq!(one.status, PluginStatus::NeedsApproval);
    assert_eq!(get("dev.test.two").status, PluginStatus::Starting);
    assert!(get("dev.test.two").bundled);
    let broken = get("broken");
    assert_eq!(broken.status, PluginStatus::Invalid);
    assert_eq!(broken.name, "broken");
    assert!(broken.error.is_some());
    assert_eq!(get("badrule").status, PluginStatus::Invalid);
    assert!(host.token("dev.test.one").is_none());

    host.command("dev.test.one", PluginCommand::Approve).unwrap();
    assert_eq!(host.list().iter().find(|p| p.id == "dev.test.one").unwrap().status, PluginStatus::Starting);
    let saved: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("plugins.json")).unwrap()).unwrap();
    assert_eq!(saved["approved"]["dev.test.one"], json!(["network"]));
    assert!(!dir.path().join("plugins.json.tmp").exists());

    host.command("dev.test.two", PluginCommand::Disable).unwrap();
    let saved: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("plugins.json")).unwrap()).unwrap();
    assert_eq!(saved["disabled"], json!(["dev.test.two"]));
    assert_eq!(host.list().iter().find(|p| p.id == "dev.test.two").unwrap().status, PluginStatus::Disabled);
    host.command("dev.test.two", PluginCommand::Enable).unwrap();
    assert_eq!(host.list().iter().find(|p| p.id == "dev.test.two").unwrap().status, PluginStatus::Starting);
    assert_eq!(host.command("nope", PluginCommand::Restart), Err(HostError::NotFound("nope".into())));
    assert!(matches!(host.command("broken", PluginCommand::Approve), Err(HostError::Invalid(_))));
}

#[test]
fn approval_logic() {
    let mut p = Persisted::default();
    assert!(!p.needs_approval("x", true, &[Permission::Exec]));
    assert!(p.needs_approval("x", false, &[]));
    p.approved.insert("x".into(), vec![]);
    assert!(!p.needs_approval("x", false, &[]));
    assert!(p.needs_approval("x", false, &[Permission::Network]));
    p.approved.insert("x".into(), vec![Permission::Network, Permission::Exec]);
    assert!(!p.needs_approval("x", false, &[Permission::Network]));
    assert!(p.needs_approval("x", false, &[Permission::Network, Permission::Filesystem]));
}

#[tokio::test]
async fn uninstall_user_only() {
    let dir = tempfile::tempdir().unwrap();
    write_plugin(&dir.path().join("bundled"), "dev.test.one", MANIFEST);
    write_plugin(&dir.path().join("user"), "dev.test.three", &MANIFEST.replace("dev.test.one", "dev.test.three"));
    let (cfg, _rx, _p, _i) = config(&dir);
    let host = PluginHost::start_inner(cfg, None).unwrap();
    assert!(matches!(host.command("dev.test.one", PluginCommand::Uninstall), Err(HostError::Invalid(_))));
    host.command("dev.test.three", PluginCommand::Uninstall).unwrap();
    assert!(!dir.path().join("user/dev.test.three").exists());
    assert!(host.list().iter().all(|p| p.id != "dev.test.three"));
}

// ---- zip --------------------------------------------------------------------------------------

fn make_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default();
    for (name, data) in entries {
        w.start_file(*name, opts).unwrap();
        w.write_all(data).unwrap();
    }
    w.finish().unwrap().into_inner()
}

fn user_host() -> (Arc<PluginHost>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let (cfg, _rx, _p, _i) = config(&dir);
    (PluginHost::start_inner(cfg, None).unwrap(), dir)
}

#[tokio::test]
async fn zip_install_root_and_nested() {
    let (host, dir) = user_host();
    let zip = make_zip(&[("manifest.json", MANIFEST.as_bytes()), ("run.sh", b"#!/bin/sh"), ("icons/a.svg", b"<svg/>")]);
    let info = host.install_zip(&zip).unwrap();
    assert_eq!(info.id, "dev.test.one");
    assert_eq!(info.status, PluginStatus::NeedsApproval);
    assert!(dir.path().join("user/dev.test.one/icons/a.svg").is_file());
    assert!(host.file("dev.test.one", "icons/a.svg").is_some());
    assert!(host.file("dev.test.one", "../../plugins.json").is_none());
    assert!(host.file("dev.test.one", "nothing").is_none());

    // Nested top folder, replacing the old copy.
    let m2 = MANIFEST.replace("\"1.0\"", "\"2.0\"");
    let zip = make_zip(&[("some-top/manifest.json", m2.as_bytes()), ("some-top/run.sh", b"#!/bin/sh")]);
    let info = host.install_zip(&zip).unwrap();
    assert_eq!(info.version, "2.0");
    assert!(dir.path().join("user/dev.test.one/run.sh").is_file());
    assert!(!dir.path().join("user/dev.test.one/icons").exists());
    assert!(std::fs::read_dir(dir.path().join("user")).unwrap().all(|e| !e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with('.')));
}

#[tokio::test]
async fn zip_install_requires_entry_for_this_os() {
    let (host, dir) = user_host();
    let m = |entry: Value| manifest_with(|v| v["entry"] = entry);
    let this = if cfg!(windows) { "windows" } else { "linux" };
    let other = if cfg!(windows) { "linux" } else { "windows" };
    // Built for the other OS only.
    let mf = m(json!({ other: "demo.bin" }));
    let zip = make_zip(&[("manifest.json", mf.as_bytes()), ("demo.bin", b"x")]);
    match host.install_zip(&zip) {
        Err(HostError::Invalid(s)) => {
            assert!(s.starts_with("This plugin package has no "), "{s}");
            assert!(s.contains("demo.bin"), "{s}");
        }
        r => panic!("{r:?}"),
    }
    // Entry for this OS, but the file is not in the zip (only the other OS's file is).
    let mf = m(json!({ this: "demo", other: "demo.exe" }));
    let zip = make_zip(&[("manifest.json", mf.as_bytes()), ("demo.exe", b"x")]);
    match host.install_zip(&zip) {
        Err(HostError::Invalid(s)) => assert!(s.contains("no ") && s.contains("demo.exe"), "{s}"),
        r => panic!("{r:?}"),
    }
    // Entry file missing and nothing else to blame.
    let mf = m(json!(format!("gone/run-{}", this)));
    let zip = make_zip(&[("manifest.json", mf.as_bytes())]);
    match host.install_zip(&zip) {
        Err(HostError::Invalid(s)) => assert!(s.contains("missing its program"), "{s}"),
        r => panic!("{r:?}"),
    }
    assert!(!dir.path().join("user/dev.test.one").exists());
    // And the good case.
    let mf = m(json!({ this: "demo" }));
    let zip = make_zip(&[("manifest.json", mf.as_bytes()), ("demo", b"x")]);
    host.install_zip(&zip).unwrap();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_entry_file_gives_up_with_one_warning() {
    let dir = tempfile::tempdir().unwrap();
    let plugin = dir.path().join("bundled/dev.test.one");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::write(plugin.join("manifest.json"), manifest_with(|v| v["entry"] = json!("nothere.sh"))).unwrap();
    let (cfg, _rx, _p, _i) = config(&dir);
    let host = PluginHost::start(cfg).unwrap();
    // The old behaviour retried after 1 s, 2 s, ...
    tokio::time::sleep(Duration::from_millis(3500)).await;
    let log = host.log("dev.test.one").unwrap();
    assert_eq!(log.iter().filter(|l| l.contains("cannot start plugin")).count(), 1, "{log:?}");
    assert!(log.iter().all(|l| !l.contains("restarting in")), "{log:?}");
    assert_eq!(host.list().iter().find(|p| p.id == "dev.test.one").unwrap().status, PluginStatus::Crashed);
    let h = host.clone();
    tokio::task::spawn_blocking(move || h.shutdown()).await.unwrap();
}

#[tokio::test]
async fn zip_rejects_bad_archives() {
    let (host, dir) = user_host();
    let m = MANIFEST.as_bytes();
    for bad in [
        make_zip(&[("manifest.json", m), ("../evil.txt", b"x")]),
        make_zip(&[("manifest.json", m), ("/abs/evil.txt", b"x")]),
        make_zip(&[("top/manifest.json", m), ("top/../../evil.txt", b"x")]),
        make_zip(&[("a/manifest.json", m), ("b/other.txt", b"x")]),
        make_zip(&[("readme.txt", b"x")]),
        make_zip(&[("manifest.json", b"{ nope")]),
        b"not a zip".to_vec(),
    ] {
        assert!(matches!(host.install_zip(&bad), Err(HostError::Invalid(_))));
    }
    let big = vec![0u8; 51 * 1024 * 1024];
    let zip = make_zip(&[("manifest.json", m), ("big.bin", &big)]);
    assert!(matches!(host.install_zip(&zip), Err(HostError::Invalid(_))));
    assert!(!dir.path().join("evil.txt").exists());
    let user = dir.path().join("user");
    assert!(std::fs::read_dir(&user).unwrap().next().is_none(), "temp dirs must be cleaned up");
    assert!(host.list().is_empty());
}

// ---- JSON-RPC session -------------------------------------------------------------------------

#[tokio::test]
async fn bad_token_and_wrong_first_message_close() {
    let fx = fixture();
    let mut c = connect(fx.host.port()).await;
    send(&mut c, json!({"jsonrpc": "2.0", "id": 1, "method": "register", "params": {"pluginId": "dev.test.one", "token": "wrong", "protocol": 1}})).await;
    let r = recv(&mut c).await;
    assert!(r["error"].is_object());
    assert!(closed(&mut c).await);
    assert_eq!(status(&fx), PluginStatus::Starting);

    let mut c = connect(fx.host.port()).await;
    send(&mut c, json!({"jsonrpc": "2.0", "method": "setTitle", "params": {}})).await;
    assert!(closed(&mut c).await);

    let mut c = connect(fx.host.port()).await;
    send(&mut c, json!({"jsonrpc": "2.0", "id": 1, "method": "register", "params": {"pluginId": "nope", "token": "x", "protocol": 1}})).await;
    let _ = recv(&mut c).await;
    assert!(closed(&mut c).await);
}

#[tokio::test]
async fn register_then_will_appear_and_looks() {
    let mut fx = fixture();
    fx.host.set_visible(vec![inst("p:1", "hello")]);
    assert_eq!(fx.host.sample("p:1").unwrap().dimmed, Some(true));
    assert!(fx.host.sample("p:unknown").is_none());

    let mut c = register(&fx).await;
    assert_eq!(status(&fx), PluginStatus::Running);
    let w = recv(&mut c).await;
    assert_eq!(w["method"], "action.willAppear");
    assert!(w.get("id").is_none());
    assert_eq!(w["params"]["context"], "p:1");
    assert_eq!(w["params"]["action"], "a");
    assert_eq!(w["params"]["controller"], "key");
    assert_eq!(w["params"]["state"], 1);
    assert_eq!(w["params"]["value"], 0.5);
    assert_eq!(w["params"]["settings"]["k"], "hello");

    let s = fx.host.sample("p:1").unwrap();
    assert_eq!(s.dimmed, Some(false));
    assert_eq!(s.title, None);

    let before = fx.pokes.load(Ordering::SeqCst);
    assert_eq!(call(&mut c, 2, "setTitle", json!({"context": "p:1", "title": "T"})).await["result"], json!({}));
    call(&mut c, 3, "setSubtitle", json!({"context": "p:1", "subtitle": "S"})).await;
    call(&mut c, 4, "setState", json!({"context": "p:1", "state": 2})).await;
    call(&mut c, 5, "setValue", json!({"context": "p:1", "value": 7.5})).await;
    assert!(fx.pokes.load(Ordering::SeqCst) >= before + 4);
    let s = fx.host.sample("p:1").unwrap();
    assert_eq!(
        (s.title.as_deref(), s.subtitle.as_deref(), s.state_index, s.value),
        (Some("T"), Some("S"), Some(2), Some(1.0))
    );
    assert_eq!(s.icon_hash, None);

    // Notifications (no id) get no reply: the next reply is for the request after them.
    send(&mut c, json!({"jsonrpc": "2.0", "method": "setTitle", "params": {"context": "p:1", "title": "N"}})).await;
    let r = call(&mut c, 6, "setSubtitle", json!({"context": "p:1", "subtitle": "S2"})).await;
    assert_eq!(r["id"], 6);
    assert_eq!(fx.host.sample("p:1").unwrap().title.as_deref(), Some("N"));

    // showOk / showAlert / openUrl / log / setSettings
    call(&mut c, 7, "showOk", json!({"context": "p:1"})).await;
    assert_eq!(fx.events.recv().await.unwrap(), HostEvent::Feedback { context: "p:1".into(), ok: true });
    call(&mut c, 8, "showAlert", json!({"context": "p:1"})).await;
    assert_eq!(fx.events.recv().await.unwrap(), HostEvent::Feedback { context: "p:1".into(), ok: false });
    call(&mut c, 9, "openUrl", json!({"url": "https://example.com/x"})).await;
    assert_eq!(fx.events.recv().await.unwrap(), HostEvent::OpenUrl { url: "https://example.com/x".into() });
    let r = call(&mut c, 10, "openUrl", json!({"url": "file:///etc/passwd"})).await;
    assert_eq!(r["error"]["code"], -32002);
    call(&mut c, 11, "log", json!({"level": "warn", "message": "careful"})).await;
    assert!(fx.host.log("dev.test.one").unwrap().iter().any(|l| l.contains("[warn] careful")));
    assert_eq!(call(&mut c, 12, "log", json!({"level": "loud", "message": "x"})).await["error"]["code"], -32602);

    call(&mut c, 13, "setSettings", json!({"context": "p:1", "settings": {"k": "new"}})).await;
    match fx.events.recv().await.unwrap() {
        HostEvent::SetSettings { context, plugin, settings } => {
            assert_eq!((context.as_str(), plugin.as_str()), ("p:1", "dev.test.one"));
            assert_eq!(settings["k"], "new");
        }
        other => panic!("{other:?}"),
    }
    // The agent later reports the saved settings: no echo.
    fx.host.set_visible(vec![inst("p:1", "hello")]);
    fx.host.set_visible(vec![inst("p:1", "new")]);

    // Errors.
    assert_eq!(call(&mut c, 20, "setTitle", json!({"context": "nope:1", "title": "x"})).await["error"]["code"], -32001);
    assert_eq!(call(&mut c, 21, "setTitle", json!({"context": "p:1"})).await["error"]["code"], -32602);
    assert_eq!(call(&mut c, 22, "frobnicate", json!({})).await["error"]["code"], -32601);
    assert_eq!(call(&mut c, 23, "showOk", json!({"context": "nope:1"})).await["error"]["code"], -32001);
    assert!(fx.events.try_recv().is_err());

    // The connection stays usable and nothing was echoed to it as settings.didChange.
    fx.host.event(PluginEvent {
        context: "p:1".into(),
        plugin: "dev.test.one".into(),
        action: "a".into(),
        settings: PluginSettings::new(),
        kind: PluginEventKind::KeyDown,
    });
    let m = recv(&mut c).await;
    assert_eq!(m["method"], "key.down");
}

#[tokio::test]
async fn visible_diff_and_events() {
    let fx = fixture();
    let mut c = register(&fx).await;
    fx.host.set_visible(vec![inst("p:1", "a"), inst("p:2", "b")]);
    let mut seen = [recv(&mut c).await, recv(&mut c).await];
    seen.sort_by_key(|m| m["params"]["context"].as_str().unwrap().to_string());
    assert!(seen.iter().all(|m| m["method"] == "action.willAppear"));

    // Unchanged: nothing sent. Changed settings: didChange. Dropped: willDisappear.
    fx.host.set_visible(vec![inst("p:1", "a2")]);
    let m1 = recv(&mut c).await;
    let m2 = recv(&mut c).await;
    let mut ms = [m1, m2];
    ms.sort_by_key(|m| m["method"].as_str().unwrap().to_string());
    assert_eq!(ms[0]["method"], "action.willDisappear");
    assert_eq!(ms[0]["params"]["context"], "p:2");
    assert_eq!(ms[1]["method"], "settings.didChange");
    assert_eq!(ms[1]["params"]["settings"]["k"], "a2");
    fx.host.set_visible(vec![inst("p:1", "a2")]);

    // A look survives disappear/appear; the old context is still accepted this connection.
    call(&mut c, 1, "setTitle", json!({"context": "p:1", "title": "kept"})).await;
    fx.host.set_visible(vec![]);
    assert_eq!(recv(&mut c).await["method"], "action.willDisappear");
    assert_eq!(call(&mut c, 2, "setTitle", json!({"context": "p:1", "title": "kept2"})).await["result"], json!({}));
    assert_eq!(fx.host.sample("p:1").unwrap().title.as_deref(), Some("kept2"));
    fx.host.set_visible(vec![inst("p:1", "a2")]);
    assert_eq!(recv(&mut c).await["method"], "action.willAppear");

    let ev = |kind| PluginEvent {
        context: "p:1".into(),
        plugin: "dev.test.one".into(),
        action: "a".into(),
        settings: inst("p:1", "a2").settings,
        kind,
    };
    fx.host.event(ev(PluginEventKind::DialRotate { ticks: -3 }));
    let m = recv(&mut c).await;
    assert_eq!(m["method"], "dial.rotate");
    assert_eq!(m["params"]["ticks"], -3);
    assert_eq!(m["params"]["context"], "p:1");
    assert_eq!(m["params"]["settings"]["k"], "a2");
    fx.host.event(ev(PluginEventKind::SliderChange { value: 0.25 }));
    let m = recv(&mut c).await;
    assert_eq!(m["method"], "slider.change");
    assert_eq!(m["params"]["value"], 0.25);
    for (kind, name) in [
        (PluginEventKind::KeyDown, "key.down"),
        (PluginEventKind::KeyUp, "key.up"),
        (PluginEventKind::LongPress, "key.longPress"),
        (PluginEventKind::DoubleTap, "key.doubleTap"),
        (PluginEventKind::DialPress, "dial.press"),
        (PluginEventKind::DialRelease, "dial.release"),
        (PluginEventKind::TouchStart, "touch.start"),
        (PluginEventKind::TouchEnd, "touch.end"),
    ] {
        fx.host.event(ev(kind));
        let m = recv(&mut c).await;
        assert_eq!(m["method"], name);
        assert!(m["params"].get("ticks").is_none());
    }
    fx.host.focus_changed("Code.exe", "main.rs");
    let m = recv(&mut c).await;
    assert_eq!(m["method"], "system.focusChanged");
    assert_eq!(m["params"]["app"], "Code.exe");
    assert_eq!(m["params"]["title"], "main.rs");
}

#[tokio::test]
async fn second_registration_replaces_first_and_disconnect_dims() {
    let fx = fixture();
    fx.host.set_visible(vec![inst("p:1", "a")]);
    let mut first = register(&fx).await;
    assert_eq!(recv(&mut first).await["method"], "action.willAppear");
    let mut second = register(&fx).await;
    assert_eq!(recv(&mut second).await["method"], "action.willAppear");
    assert!(closed(&mut first).await);
    assert_eq!(status(&fx), PluginStatus::Running);
    assert_eq!(fx.host.sample("p:1").unwrap().dimmed, Some(false));
    drop(second);
    for _ in 0..50 {
        if status(&fx) != PluginStatus::Running {
            break;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    assert_ne!(status(&fx), PluginStatus::Running);
    assert_eq!(fx.host.sample("p:1").unwrap().dimmed, Some(true));
}

#[tokio::test]
async fn set_image_data_url_path_and_cache() {
    use base64::Engine;
    let fx = fixture();
    std::fs::create_dir_all(fx.dir.path().join("bundled/dev.test.one/icons")).unwrap();
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><rect width="10" height="10" fill="#e3e6ea"/></svg>"##;
    std::fs::write(fx.dir.path().join("bundled/dev.test.one/icons/x.svg"), svg).unwrap();
    fx.host.set_visible(vec![inst("p:1", "a")]);
    let mut c = register(&fx).await;
    let _ = recv(&mut c).await;

    let url = format!("data:image/svg+xml;base64,{}", base64::engine::general_purpose::STANDARD.encode(svg));
    assert_eq!(call(&mut c, 1, "setImage", json!({"context": "p:1", "image": url})).await["result"], json!({}));
    let h1 = fx.host.sample("p:1").unwrap().icon_hash.unwrap();
    assert!(h1.starts_with("icon"));
    // Same image again, by path: same bytes, so the importer isn't called again.
    assert_eq!(
        call(&mut c, 2, "setImage", json!({"context": "p:1", "image": "icons/x.svg"})).await["result"],
        json!({})
    );
    assert_eq!(fx.imports.load(Ordering::SeqCst), 1);
    assert_eq!(fx.host.sample("p:1").unwrap().icon_hash.as_deref(), Some(h1.as_str()));
    // Escapes and bad data.
    assert_eq!(
        call(&mut c, 3, "setImage", json!({"context": "p:1", "image": "../../plugins.json"})).await["error"]["code"],
        -32002
    );
    assert_eq!(
        call(&mut c, 4, "setImage", json!({"context": "p:1", "image": "data:image/png;base64,AAAA"})).await["error"]
            ["code"],
        -32602
    );
    assert_eq!(
        call(&mut c, 5, "setImage", json!({"context": "p:1", "image": "icons/missing.png"})).await["error"]["code"],
        -32602
    );
    // Clear.
    call(&mut c, 6, "setImage", json!({"context": "p:1", "image": ""})).await;
    assert_eq!(fx.host.sample("p:1").unwrap().icon_hash.as_deref(), Some(""));
}

#[tokio::test]
async fn shutdown_closes_connections() {
    let fx = fixture();
    let mut c = register(&fx).await;
    fx.host.shutdown();
    assert!(closed(&mut c).await);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_spawn_passes_args_and_logs_output() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let plugin = dir.path().join("bundled/dev.test.one");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::write(plugin.join("manifest.json"), manifest_with(|v| v["entry"] = json!("run.sh"))).unwrap();
    let script = plugin.join("run.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\necho \"args: $@ env: $NOTIDECK_PLUGIN_ID cwd: $(basename \"$PWD\")\"\nsleep 30\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let (cfg, _rx, _p, _i) = config(&dir);
    let host = PluginHost::start(cfg).unwrap();
    let mut log = Vec::new();
    for _ in 0..100 {
        log = host.log("dev.test.one").unwrap();
        if log.iter().any(|l| l.contains("args:")) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let line = log.iter().find(|l| l.contains("args:")).expect("no output captured").clone();
    assert!(line.contains("--port") && line.contains("--token") && line.contains("--plugin-id dev.test.one"), "{line}");
    assert!(line.contains("env: dev.test.one") && line.contains("cwd: dev.test.one"), "{line}");
    let h = host.clone();
    tokio::task::spawn_blocking(move || h.shutdown()).await.unwrap();
}

#[tokio::test]
async fn set_detail_and_phone_play_stop() {
    let mut fx = fixture();
    fx.host.set_visible(vec![inst("p:1", "x")]);
    let mut c = register(&fx).await;
    let _ = recv(&mut c).await; // willAppear
    assert_eq!(fx.host.sample("p:1").unwrap().detail, None);

    assert_eq!(call(&mut c, 2, "setDetail", json!({"context": "p:1", "detail": "12.4K"})).await["result"], json!({}));
    assert_eq!(fx.host.sample("p:1").unwrap().detail.as_deref(), Some("12.4K"));
    assert_eq!(
        call(&mut c, 3, "setDetail", json!({"context": "nope:1", "detail": "x"})).await["error"]["code"],
        -32001
    );
    assert_eq!(call(&mut c, 4, "setDetail", json!({"context": "p:1"})).await["error"]["code"], -32602);

    // phone.play: without a context, with a context and maxHeight.
    call(&mut c, 5, "phone.play", json!({"url": "https://stream.example/a.m3u8"})).await;
    assert_eq!(
        fx.events.recv().await.unwrap(),
        HostEvent::PlayOnPhone {
            plugin: "dev.test.one".into(),
            context: None,
            url: "https://stream.example/a.m3u8".into(),
            title: String::new(),
            subtitle: String::new(),
            max_height: 0,
            muted: false,
            sync_delay_ms: 0,
            session: String::new(),
        }
    );
    let r = call(&mut c, 6, "phone.play", json!({"context": "p:1", "url": "HTTPS://stream.example/b.m3u8", "title": "T", "subtitle": "S", "maxHeight": 720, "muted": true, "syncDelayMs": 8000, "session": "s7"})).await;
    assert_eq!(r["result"], json!({}));
    assert_eq!(
        fx.events.recv().await.unwrap(),
        HostEvent::PlayOnPhone {
            context: Some("p:1".into()),
            url: "HTTPS://stream.example/b.m3u8".into(),
            title: "T".into(),
            subtitle: "S".into(),
            max_height: 720,
            plugin: "dev.test.one".into(),
            muted: true,
            sync_delay_ms: 8000,
            session: "s7".into(),
        }
    );

    // Rejections send no event.
    assert_eq!(
        call(&mut c, 7, "phone.play", json!({"url": "http://stream.example/a.m3u8"})).await["error"]["code"],
        -32002
    );
    assert_eq!(call(&mut c, 8, "phone.play", json!({"url": "file:///etc/passwd"})).await["error"]["code"], -32002);
    assert_eq!(
        call(&mut c, 9, "phone.play", json!({"context": "nope:1", "url": "https://x/a.m3u8"})).await["error"]["code"],
        -32001
    );
    assert_eq!(call(&mut c, 10, "phone.play", json!({"title": "no url"})).await["error"]["code"], -32602);
    assert!(fx.events.try_recv().is_err());

    call(&mut c, 11, "phone.stop", json!({})).await;
    assert_eq!(fx.events.recv().await.unwrap(), HostEvent::StopOnPhone);
}
