// SPDX-License-Identifier: GPL-3.0-or-later

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use nd_fakephone::store::describe_goodbye;
use nd_fakephone::{
    connect_stored, default_identity_dir, forget_if_invalidated, plaintext_v1_probe, ConnectRequest, FakePhoneConfig,
    FakePhoneError, PendingHook,
};
use nd_proto::v1::GoodbyeReason;
use nd_transport::{Fingerprint, PairingUri};

const HELP: &str = "\
nd-fakephone - headless Notideck phone for e2e runs (TLS 1.3 + pairing, protocol v2)

USAGE:
    fakephone [OPTIONS]

CONNECTION:
    --addr <ip:port>          Host control address. Default: the stored host's address, or
                              127.0.0.1:47800 on first contact. Overrides pair-URI addresses.
    --identity <dir>          Directory with the phone's persistent state: client-cert.der,
                              client-key.der and hosts.json (pinned hosts: host_id, name, fp,
                              addrs, port). Created on demand.
                              Default: $XDG_STATE_HOME/notideck/fakephone, else
                              ~/.local/state/notideck/fakephone, else <tmp>/notideck-fakephone.
    --host <host_id>          Pick a stored host when several are pinned.
    --expect-host-fp <hex>    Require this host certificate fingerprint (64 hex chars), overriding
                              the stored pin.

HOST VERIFICATION:
    A stored host is pinned by its fingerprint. With --pair-uri the URI's `f` is pinned. Without
    any pin the first contact is trust-on-first-use (like the phone over USB): the fingerprint and
    Welcome.host_id are stored.

PAIRING:
    --pair-uri <notideck://pair?...>
                              Pair over \"LAN\" with a QR link: connect to its addresses/port, send
                              Hello + PairRequest, print the PairResult, then run the session.
    --device-name <name>      Name shown in the PC's device list (default: Fake Phone).
    --approval-timeout <s>    How long to wait for Allow on the PC when pending (default 130).
                              An unknown phone over USB shows 'waiting for approval' until then.

SESSION:
    --tap <control-id>        Send a tap on this control, then exit.
    --tap-rail <control-id>   Tap a pinned side-rail key (page '__rail'), then exit.
    --nav <action>            Send a navigation-rail request (home, home-pause, back, resume,
                              page:<N> for page dot N), print the NavState that follows, then exit.
    --exit-after <seconds>    Leave the session after this long (exit 0).
    --layout-wait <seconds>   How long to wait for the first layout (default 3).

CAMERA:
    --camera                  Act as a phone camera: advertise the camera, answer CameraStart with
                              an H.264 test pattern (moving colour bars + frame counter) over the
                              media connection; honours CameraControl (keyframe, lens, torch).
                              (Needs the default `camera` cargo feature.)

MICROPHONE:
    --mic                     Act as a phone microphone: advertise CAPABILITY_MIC, answer MicStart by
                              streaming a 440 Hz sine (48 kHz mono s16le) over an audio media connection.
    --mic-denied              Like --mic, but report NO_PERMISSION and never stream.

TESTING:
    --plaintext-v1            Send a v1 plaintext Hello and print the host's refusal Goodbye.
    -h, --help                This text.

EXIT CODES:
    0 ok / clean exit; 1 connection, pairing or protocol failure;
    2 usage error; 3 host revoked/rejected this phone (stored host forgotten).
";

struct Opts {
    addr: Option<SocketAddr>,
    identity: PathBuf,
    host: Option<String>,
    expect_fp: Option<Fingerprint>,
    pair_uri: Option<PairingUri>,
    device_name: Option<String>,
    approval_timeout: u64,
    tap: Option<String>,
    nav: Option<(nd_proto::v1::NavAction, u32)>,
    tap_rail: Option<String>,
    exit_after: Option<u64>,
    layout_wait: u64,
    plaintext_v1: bool,
    camera: bool,
    mic: Option<nd_fakephone::MicMode>,
}

fn parse() -> Result<Option<Opts>, String> {
    let mut o = Opts {
        addr: None,
        identity: default_identity_dir(),
        host: None,
        expect_fp: None,
        pair_uri: None,
        device_name: None,
        approval_timeout: 130,
        tap: None,
        nav: None,
        tap_rail: None,
        exit_after: None,
        layout_wait: 3,
        plaintext_v1: false,
        camera: false,
        mic: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut val = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match a.as_str() {
            "--addr" => o.addr = Some(val("--addr")?.parse().map_err(|e| format!("--addr: {e}"))?),
            "--identity" => o.identity = PathBuf::from(val("--identity")?),
            "--host" => o.host = Some(val("--host")?),
            "--expect-host-fp" => {
                o.expect_fp = Some(
                    Fingerprint::from_hex(val("--expect-host-fp")?.trim())
                        .ok_or("--expect-host-fp must be 64 hex characters")?,
                )
            }
            "--pair-uri" => {
                o.pair_uri = Some(PairingUri::parse(val("--pair-uri")?.trim()).map_err(|e| format!("--pair-uri: {e}"))?)
            }
            "--device-name" => o.device_name = Some(val("--device-name")?),
            "--approval-timeout" => {
                o.approval_timeout =
                    val("--approval-timeout")?.parse().map_err(|e| format!("--approval-timeout: {e}"))?
            }
            "--tap" => o.tap = Some(val("--tap")?),
            "--tap-rail" => o.tap_rail = Some(val("--tap-rail")?),
            "--nav" => {
                use nd_proto::v1::NavAction as N;
                let v = val("--nav")?;
                o.nav = Some(match v.as_str() {
                    "home" => (N::Home, 0),
                    "home-pause" => (N::HomePause, 0),
                    "back" => (N::Back, 0),
                    "resume" => (N::Resume, 0),
                    other => match other.strip_prefix("page:").and_then(|n| n.parse().ok()) {
                        Some(n) => (N::GoToPage, n),
                        None => {
                            return Err(format!(
                                "--nav: unknown action `{other}` (home, home-pause, back, resume, page:<N>)"
                            ))
                        }
                    },
                })
            }
            "--exit-after" => {
                o.exit_after = Some(val("--exit-after")?.parse().map_err(|e| format!("--exit-after: {e}"))?)
            }
            "--layout-wait" => {
                o.layout_wait = val("--layout-wait")?.parse().map_err(|e| format!("--layout-wait: {e}"))?
            }
            "--plaintext-v1" => o.plaintext_v1 = true,
            "--camera" => {
                if cfg!(feature = "camera") {
                    o.camera = true
                } else {
                    return Err("--camera needs the `camera` cargo feature".into());
                }
            }
            "--mic" => o.mic = Some(nd_fakephone::MicMode::Normal),
            "--mic-denied" => o.mic = Some(nd_fakephone::MicMode::NoPermission),
            "--help" | "-h" => return Ok(None),
            other => return Err(format!("unknown argument `{other}` (see --help)")),
        }
    }
    Ok(Some(o))
}

#[tokio::main]
async fn main() -> ExitCode {
    let opts = match parse() {
        Ok(Some(o)) => o,
        Ok(None) => {
            print!("{HELP}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    match run(opts).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            match e {
                FakePhoneError::Goodbye { reason, message } => {
                    eprintln!("{}", describe_goodbye(reason, &message));
                    if matches!(reason, GoodbyeReason::Revoked | GoodbyeReason::Unauthorized) {
                        return ExitCode::from(3);
                    }
                }
                FakePhoneError::PairingRejected(_) => return ExitCode::from(3),
                _ => {}
            }
            ExitCode::FAILURE
        }
    }
}

async fn run(opts: Opts) -> Result<ExitCode, FakePhoneError> {
    let mut config = FakePhoneConfig {
        approval_timeout: Duration::from_secs(opts.approval_timeout),
        handshake_timeout: Duration::from_secs(5),
        require_layout: false,
        ..FakePhoneConfig::default()
    };
    config.on_pending = Some(PendingHook(std::sync::Arc::new(|host: &str| {
        println!("Pairing pending: waiting for approval on {host} (press Allow on the PC)...");
    })));
    if let Some(n) = &opts.device_name {
        config.device_name = n.clone();
    }
    if opts.mic.is_some() {
        config = config.with_mic();
    }

    if opts.plaintext_v1 {
        let addr = opts
            .addr
            .or_else(|| opts.pair_uri.as_ref().and_then(|u| u.addrs.first().map(|ip| SocketAddr::new(*ip, u.port))))
            .unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], nd_proto::CONTROL_PORT)));
        println!("Sending plaintext v1 Hello to {addr}...");
        let g = plaintext_v1_probe(addr, &config).await?;
        let reason = GoodbyeReason::try_from(g.reason).unwrap_or(GoodbyeReason::Unspecified);
        println!("Host refused (Goodbye {reason:?}): {}", g.message);
        return Ok(ExitCode::SUCCESS);
    }

    let req = ConnectRequest {
        addr: opts.addr,
        pair_uri: opts.pair_uri.clone(),
        host_id: opts.host.clone(),
        expect_host_fp: opts.expect_fp,
    };
    if let Some(uri) = &req.pair_uri {
        println!("Pairing with '{}' ({})...", uri.host_name, uri.host_id);
    } else {
        println!("Connecting (identity dir {})...", opts.identity.display());
    }

    let session = connect_stored(&opts.identity, req, config).await?;
    let nd_fakephone::StoredSession { phone, host, trusted_on_first_use } = session;
    let phone_arc = std::sync::Arc::new(phone);
    let phone = &*phone_arc;

    if let Some(r) = phone.pair_result().await {
        println!(
            "PairResult: ok={} host='{}' ({}){}",
            r.ok,
            r.host_name,
            r.host_id,
            if r.error.is_empty() { String::new() } else { format!(" error={}", r.error) }
        );
    }
    if phone.was_pending().await {
        println!("Approved on the PC.");
    }
    println!("Own fingerprint:  {}", phone.own_fp());
    println!("Host fingerprint: {}", phone.host_fp());
    if trusted_on_first_use {
        println!("Trusted host on first use and stored its fingerprint.");
    }
    if let Some(w) = phone.welcome().await {
        println!("Connected to host '{}' (session: {}, host id: {})", w.host_name, w.session_id, w.host_id);
    }
    println!("Pinned host saved: {} {} {:?}:{}", host.host_id, host.name, host.addrs, host.port);

    match phone.ensure_layout(Duration::from_secs(opts.layout_wait)).await {
        Ok(l) => {
            println!("Received layout '{}' (grid: {}x{}, controls: {})", l.page_id, l.cols, l.rows, l.controls.len())
        }
        Err(_) => println!("No layout received yet."),
    }

    if let Some(n) = phone.latest_nav_state().await {
        println!("NavState: {}", describe_nav(&n));
    }
    if let Some(rail) = phone.latest_rail().await {
        let keys: Vec<_> = rail.controls.iter().map(|c| format!("{}('{}')", c.id, c.title)).collect();
        println!("SetRail: {} key(s) [{}]", rail.controls.len(), keys.join(", "));
    }
    if let Some((action, index)) = opts.nav {
        println!("Sending NavRequest {action:?}...");
        if action == nd_proto::v1::NavAction::GoToPage {
            phone.send_nav_go_to_page(index).await?;
        } else {
            phone.send_nav_request(action).await?;
        }
        match phone.wait_for_nav_state(Duration::from_secs(3)).await {
            Ok(n) => println!("NavState: {}", describe_nav(&n)),
            Err(_) => println!("No NavState followed."),
        }
        return Ok(ExitCode::SUCCESS);
    }

    if let Some(control_id) = &opts.tap_rail {
        println!("Simulating tap on rail key '{control_id}'...");
        phone.simulate_rail_tap(control_id).await?;
        println!("Tap sent.");
        tokio::time::sleep(Duration::from_millis(150)).await;
        return Ok(ExitCode::SUCCESS);
    }

    if let Some(control_id) = &opts.tap {
        println!("Simulating tap on control '{control_id}'...");
        phone.simulate_tap(control_id).await?;
        println!("Tap sent.");
        tokio::time::sleep(Duration::from_millis(150)).await;
        return Ok(ExitCode::SUCCESS);
    }

    #[cfg(feature = "camera")]
    let _camera = if opts.camera {
        let c = nd_fakephone::FakeCamera::attach(phone_arc.clone()).await?;
        println!("Camera ready (waiting for CameraStart).");
        Some(c)
    } else {
        None
    };

    let _mic = match opts.mic {
        Some(mode) => {
            let m = nd_fakephone::FakeMic::attach(phone_arc.clone(), mode).await?;
            println!("Microphone ready (waiting for MicStart).");
            Some(m)
        }
        None => None,
    };

    println!("Fake phone running. Responding to pings. Press Ctrl+C to exit.");
    let limit = opts.exit_after.map(Duration::from_secs);
    tokio::select! {
        closed = phone.wait_closed(limit) => {
            if !closed {
                println!("Exiting after {}s.", opts.exit_after.unwrap_or(0));
                return Ok(ExitCode::SUCCESS);
            }
        }
        _ = tokio::signal::ctrl_c() => {
            println!("Exiting.");
            return Ok(ExitCode::SUCCESS);
        }
    }
    match phone.goodbye().await {
        Some(g) => {
            let reason = GoodbyeReason::try_from(g.reason).unwrap_or(GoodbyeReason::Unspecified);
            eprintln!("Session ended: {}", describe_goodbye(reason, &g.message));
            let dropped = forget_if_invalidated(&opts.identity, &phone.host_fp(), &g)?;
            if dropped {
                eprintln!("Forgot the stored host.");
            }
            if matches!(reason, GoodbyeReason::Shutdown) {
                Ok(ExitCode::SUCCESS)
            } else if dropped {
                Ok(ExitCode::from(3))
            } else {
                Ok(ExitCode::FAILURE)
            }
        }
        None => {
            eprintln!("Connection lost.");
            Ok(ExitCode::FAILURE)
        }
    }
}

fn describe_nav(n: &nd_proto::v1::NavState) -> String {
    format!(
        "can_go_back={} auto_switch_paused={} on_home={} profile='{}' page={}/{}",
        n.can_go_back,
        n.auto_switch_paused,
        n.on_home,
        n.profile_name,
        n.page_index + 1,
        n.page_count
    )
}
