// SPDX-License-Identifier: GPL-3.0-or-later
//! Live data sampling for controls with a [`DataSource`] (SPEC §6.4 Tiles / Audio / Media).
//!
//! [`sample_all`] reads every source once; [`crate::DeckSession::apply_live`] then writes the
//! samples into the controls and produces change-only patches.

use crate::autoswitch::ProfileActions;
use crate::camera::{zoom_to_value, CameraActions};
use crate::model::{AudioApp, DataSource};
use nd_platform::{AudioControl, MediaInfo, SystemSnapshot, SystemStats};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Platform backends used to resolve data sources.
#[derive(Clone)]
pub struct LiveProviders {
    pub audio: Arc<dyn AudioControl>,
    pub media: Arc<dyn MediaInfo>,
    pub stats: Arc<dyn SystemStats>,
    pub camera: Arc<dyn CameraActions>,
    /// Round 25: phone microphone state for `mic_state` / `mic_muted`.
    pub mic: Arc<dyn crate::mic::MicActions>,
    pub profile_actions: Arc<dyn ProfileActions>,
    pub mixer: Arc<crate::mixer::Mixer>,
    /// Round 14: OBS state for the `obs_*` sources.
    pub obs: Arc<dyn crate::obs::ObsControl>,
    /// Round 16: imports album art PNGs into the icon store (returns the hash). `None` = no art.
    /// Callers should cache by the art `Arc` pointer / track so the same bytes are not re-imported.
    pub icons: Option<crate::mixer::IconImporter>,
}

/// One sampled reading for a control. `None` fields are left untouched.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LiveSample {
    pub value: Option<f32>,
    pub title: Option<String>,
    pub subtitle: Option<String>,
    pub state_index: Option<u32>,
    /// Round 12: icon hash (`""` clears the icon).
    pub icon_hash: Option<String>,
    /// Round 12: whether the control shows a level meter (levels come from [`sample_levels`]).
    pub meter: Option<bool>,
    /// Round 12: target unavailable.
    pub dimmed: Option<bool>,
    /// Round 16: third text line (`Control.detail`).
    pub detail: Option<String>,
}

/// What a level meter measures (round 12).
#[derive(Debug, Clone, PartialEq)]
pub enum MeterTarget {
    App(crate::model::AudioApp),
    Endpoint(nd_platform::AudioTarget),
}

/// Controls whose source has a level meter: `AppAudio` (if the backend has app peaks) and `Volume`
/// (if the endpoint reports a peak). Contract; R12-3 implements.
pub fn meter_targets(sources: &[(String, DataSource)], p: &LiveProviders) -> Vec<(String, MeterTarget)> {
    let app_peaks = sources.iter().any(|(_, s)| matches!(s, DataSource::AppAudio { .. })) && p.mixer.has_app_peaks();
    sources
        .iter()
        .filter_map(|(id, src)| match src {
            DataSource::AppAudio { app } if app_peaks => Some((id.clone(), MeterTarget::App(app.clone()))),
            DataSource::Volume { target } => {
                p.mixer.endpoint_level(*target).map(|_| (id.clone(), MeterTarget::Endpoint(*target)))
            }
            _ => None,
        })
        .collect()
}

/// Fast path (~12 Hz): current level per meter control, quantised to steps of 1/50. Unresolved targets → 0.0.
pub fn sample_levels(targets: &[(String, MeterTarget)], p: &LiveProviders) -> Vec<(String, f32)> {
    let apps: Vec<crate::model::AudioApp> = targets
        .iter()
        .filter_map(|(_, t)| match t {
            MeterTarget::App(a) => Some(a.clone()),
            MeterTarget::Endpoint(_) => None,
        })
        .collect();
    let app_levels = if apps.is_empty() { Vec::new() } else { p.mixer.levels(&apps) };
    let mut app_iter = app_levels.into_iter();
    targets
        .iter()
        .map(|(id, t)| {
            let level = match t {
                MeterTarget::App(_) => app_iter.next().flatten(),
                MeterTarget::Endpoint(target) => p.mixer.endpoint_level(*target),
            };
            (id.clone(), quantise_level(level.unwrap_or(0.0)))
        })
        .collect()
}

fn quantise_level(x: f32) -> f32 {
    (x.clamp(0.0, 1.0) * 50.0).round() / 50.0
}

/// Round 19: plugin-provided looks for on-screen plugin instances (`(control_id, instance)` from
/// [`crate::DeckSession::plugin_instances`]). Controls the plugin has said nothing about are skipped.
pub fn sample_plugins(
    instances: &[(String, crate::plugin::PluginInstance)],
    plugins: &dyn crate::plugin::PluginControl,
) -> Vec<(String, LiveSample)> {
    instances.iter().filter_map(|(id, inst)| plugins.sample(&inst.context).map(|s| (id.clone(), s))).collect()
}

/// Sample every source once. Stats are sampled at most once per call even if several controls use them.
pub fn sample_all(sources: &[(String, DataSource)], p: &LiveProviders) -> Vec<(String, LiveSample)> {
    let mut stats: Option<Option<SystemSnapshot>> = None;
    let mut snapshot = |p: &LiveProviders| -> Option<SystemSnapshot> {
        stats
            .get_or_insert_with(|| match p.stats.snapshot() {
                Ok(s) => Some(s),
                Err(e) => {
                    tracing::debug!(error = %e, "system stats unavailable");
                    None
                }
            })
            .clone()
    };

    let mut media: Option<nd_platform::Result<Option<nd_platform::NowPlaying>>> = None;
    let mut obs_snap: Option<crate::obs::ObsSnapshot> = None;
    let mut endpoint_meter: std::collections::HashMap<nd_platform::AudioTarget, bool> = Default::default();

    sources
        .iter()
        .map(|(id, src)| {
            let sample = match src {
                DataSource::Clock => clock_sample(local_time(now_unix())),
                DataSource::Cpu => snapshot(p).map_or_else(LiveSample::default, |s| {
                    let pct = s.cpu_percent.clamp(0.0, 100.0);
                    LiveSample {
                        value: Some(pct / 100.0),
                        title: Some(format!("{}%", pct.round() as u32)),
                        ..Default::default()
                    }
                }),
                DataSource::Memory => snapshot(p).map_or_else(LiveSample::default, |s| {
                    let value = (s.mem_total_bytes > 0).then(|| s.mem_used_bytes as f32 / s.mem_total_bytes as f32);
                    LiveSample {
                        value,
                        title: Some(format!("{:.1} GB", s.mem_used_bytes as f64 / 1e9)),
                        ..Default::default()
                    }
                }),
                DataSource::Network => snapshot(p).map_or_else(LiveSample::default, |s| LiveSample {
                    title: Some(format_rate(s.net_rx_bytes_per_sec)),
                    subtitle: Some(format_rate(s.net_tx_bytes_per_sec)),
                    ..Default::default()
                }),
                DataSource::Volume { target } => match p.audio.volume(*target) {
                    Ok(v) => {
                        let v = v.clamp(0.0, 1.0);
                        LiveSample {
                            value: Some(v),
                            subtitle: Some(format!("{}%", (v * 100.0).round() as u32)),
                            meter: Some(
                                *endpoint_meter
                                    .entry(*target)
                                    .or_insert_with(|| p.mixer.endpoint_level(*target).is_some()),
                            ),
                            ..Default::default()
                        }
                    }
                    Err(e) => {
                        tracing::debug!(error = %e, "volume unavailable");
                        LiveSample::default()
                    }
                },
                DataSource::Mute { target } => match p.audio.muted(*target) {
                    Ok(m) => LiveSample { state_index: Some(u32::from(m)), ..Default::default() },
                    Err(e) => {
                        tracing::debug!(error = %e, "mute state unavailable");
                        LiveSample::default()
                    }
                },
                src @ (DataSource::ObsScene { .. }
                | DataSource::ObsStream
                | DataSource::ObsRecord
                | DataSource::ObsReplayBuffer
                | DataSource::ObsVirtualCam
                | DataSource::ObsStudioMode
                | DataSource::ObsSourceVisibility { .. }
                | DataSource::ObsInput { .. }
                | DataSource::ObsStatus) => crate::obs::sample(obs_snap.get_or_insert_with(|| p.obs.snapshot()), src),
                DataSource::CameraState => {
                    LiveSample { state_index: Some(u32::from(p.camera.snapshot().active)), ..Default::default() }
                }
                DataSource::MicState => {
                    LiveSample { state_index: Some(u32::from(p.mic.snapshot().on)), ..Default::default() }
                }
                DataSource::MicMuted => {
                    LiveSample { state_index: Some(u32::from(p.mic.snapshot().muted)), ..Default::default() }
                }
                DataSource::AutoSwitchPaused => {
                    LiveSample { state_index: Some(u32::from(p.profile_actions.paused())), ..Default::default() }
                }
                DataSource::ProfileDestination { to } => match p.profile_actions.destination(to) {
                    Some((name, here)) => LiveSample {
                        subtitle: Some(name),
                        state_index: Some(u32::from(here)),
                        dimmed: Some(false),
                        ..Default::default()
                    },
                    None => LiveSample {
                        subtitle: Some(String::new()),
                        state_index: Some(0),
                        dimmed: Some(true),
                        ..Default::default()
                    },
                },
                DataSource::CurrentProfile => {
                    LiveSample { title: p.profile_actions.current_profile_name(), ..Default::default() }
                }
                DataSource::CameraTorch => {
                    LiveSample { state_index: Some(u32::from(p.camera.snapshot().torch)), ..Default::default() }
                }
                DataSource::CameraZoom => {
                    let z = p.camera.snapshot().zoom;
                    LiveSample {
                        value: Some(zoom_to_value(z)),
                        subtitle: Some(format!("{z:.1}x")),
                        ..Default::default()
                    }
                }
                DataSource::AppAudio { app } => {
                    let named = matches!(app, AudioApp::Name { .. });
                    let meter = Some(p.mixer.has_app_peaks());
                    match p.mixer.resolve(app) {
                        Some(a) => {
                            let mut s = LiveSample {
                                value: Some(a.volume),
                                subtitle: Some(format!("{}%", (a.volume * 100.0).round() as u32)),
                                state_index: Some(u32::from(a.muted)),
                                dimmed: Some(false),
                                meter,
                                ..Default::default()
                            };
                            if !named {
                                s.icon_hash = Some(p.mixer.icon_hash(&a).unwrap_or_default());
                                s.title = Some(a.display_name);
                            }
                            s
                        }
                        None => {
                            let mut s = LiveSample {
                                value: Some(0.0),
                                subtitle: Some(String::new()),
                                state_index: Some(0),
                                dimmed: Some(true),
                                meter: Some(false),
                                ..Default::default()
                            };
                            if !named {
                                s.title = Some(String::new());
                                s.icon_hash = Some(String::new());
                            }
                            s
                        }
                    }
                }
                DataSource::NowPlaying { art } => match media.get_or_insert_with(|| p.media.now_playing()) {
                    Ok(Some(np)) => {
                        let mut s = LiveSample {
                            value: Some(np.progress.unwrap_or(0.0)),
                            title: Some(np.title.clone()),
                            subtitle: Some(np.artist.clone()),
                            state_index: Some(u32::from(np.playing)),
                            detail: Some(format_times(np.position_s, np.length_s)),
                            dimmed: Some(false),
                            ..Default::default()
                        };
                        if *art {
                            s.icon_hash = Some(match (&np.art, &p.icons) {
                                (Some(bytes), Some(import)) => art_hash(bytes, import),
                                _ => String::new(),
                            });
                        }
                        s
                    }
                    Ok(None) => LiveSample {
                        title: Some(String::new()),
                        subtitle: Some(String::new()),
                        detail: Some(String::new()),
                        value: Some(0.0),
                        state_index: Some(0),
                        dimmed: Some(true),
                        icon_hash: art.then(String::new),
                        ..Default::default()
                    },
                    Err(e) => {
                        tracing::debug!(error = %e, "now playing unavailable");
                        LiveSample::default()
                    }
                },
                DataSource::MediaPlaying => match media.get_or_insert_with(|| p.media.now_playing()) {
                    Ok(Some(np)) => LiveSample {
                        state_index: Some(u32::from(np.playing)),
                        dimmed: Some(false),
                        ..Default::default()
                    },
                    Ok(None) => LiveSample { state_index: Some(0), dimmed: Some(true), ..Default::default() },
                    Err(_) => LiveSample::default(),
                },
                DataSource::MediaShuffle => match media.get_or_insert_with(|| p.media.now_playing()) {
                    Ok(Some(np)) => match np.shuffle {
                        Some(on) => {
                            LiveSample { state_index: Some(u32::from(on)), dimmed: Some(false), ..Default::default() }
                        }
                        None => LiveSample { state_index: Some(0), dimmed: Some(true), ..Default::default() },
                    },
                    Ok(None) => LiveSample { state_index: Some(0), dimmed: Some(true), ..Default::default() },
                    Err(_) => LiveSample::default(),
                },
                DataSource::MediaRepeat => match media.get_or_insert_with(|| p.media.now_playing()) {
                    Ok(Some(np)) => match np.repeat {
                        Some(mode) => LiveSample {
                            state_index: Some(match mode {
                                nd_platform::RepeatMode::Off => 0,
                                nd_platform::RepeatMode::List => 1,
                                nd_platform::RepeatMode::Track => 2,
                            }),
                            dimmed: Some(false),
                            ..Default::default()
                        },
                        None => LiveSample { state_index: Some(0), dimmed: Some(true), ..Default::default() },
                    },
                    Ok(None) => LiveSample { state_index: Some(0), dimmed: Some(true), ..Default::default() },
                    Err(_) => LiveSample::default(),
                },
            };
            (id.clone(), sample)
        })
        .collect()
}

/// `m:ss / m:ss` (`h:mm:ss` from one hour up); only the elapsed time when the length is unknown; empty
/// when the position is unknown.
pub(crate) fn format_times(position_s: Option<f64>, length_s: Option<f64>) -> String {
    fn hms(s: f64) -> String {
        let t = if s.is_finite() { s.max(0.0) as u64 } else { 0 };
        if t >= 3600 {
            format!("{}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60)
        } else {
            format!("{}:{:02}", t / 60, t % 60)
        }
    }
    match (position_s, length_s) {
        (Some(p), Some(l)) if l > 0.0 => format!("{} / {}", hms(p), hms(l)),
        (Some(p), _) => hms(p),
        (None, _) => String::new(),
    }
}

/// Last imported album art: (importer, art bytes, hash). Holding the `Arc`s keeps the addresses unique, so
/// pointer equality means "same bytes" and a 1 Hz tick never re-imports.
type ArtCache = Option<(crate::mixer::IconImporter, Arc<[u8]>, String)>;
static ART_CACHE: std::sync::Mutex<ArtCache> = std::sync::Mutex::new(None);

fn art_hash(bytes: &Arc<[u8]>, import: &crate::mixer::IconImporter) -> String {
    let mut cache = ART_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((i, b, h)) = cache.as_ref() {
        if Arc::ptr_eq(i, import) && Arc::ptr_eq(b, bytes) {
            return h.clone();
        }
    }
    let hash = import(bytes).unwrap_or_default();
    if !hash.is_empty() {
        *cache = Some((import.clone(), bytes.clone(), hash.clone()));
    }
    hash
}

/// Human-readable byte rate: `340 kB/s`, `1.2 MB/s`.
pub(crate) fn format_rate(bytes_per_sec: u64) -> String {
    let b = bytes_per_sec as f64;
    if b >= 1e9 {
        format!("{:.1} GB/s", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB/s", b / 1e6)
    } else if b >= 1e3 {
        format!("{:.0} kB/s", b / 1e3)
    } else {
        format!("{bytes_per_sec} B/s")
    }
}

/// Broken-down local time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Civil {
    pub year: i64,
    /// 1..=12
    pub month: u32,
    /// 1..=31
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    /// 0 = Sunday
    pub weekday: u32,
}

pub(crate) fn clock_sample(t: Civil) -> LiveSample {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    LiveSample {
        title: Some(format!("{:02}:{:02}", t.hour, t.minute)),
        subtitle: Some(format!("{} {} {}", DAYS[t.weekday as usize % 7], t.day, MONTHS[(t.month as usize + 11) % 12])),
        ..Default::default()
    }
}

fn now_unix() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// Local UTC offset in seconds at `unix` (0 if the timezone can't be resolved).
fn local_offset_secs(unix: i64) -> i64 {
    use chrono::{Local, TimeZone};
    Local.timestamp_opt(unix, 0).single().map_or(0, |t| i64::from(t.offset().local_minus_utc()))
}

fn local_time(unix: i64) -> Civil {
    civil_from_unix(unix + local_offset_secs(unix))
}

/// Convert seconds since the epoch (already offset) to civil time (Hinnant's algorithm).
pub(crate) fn civil_from_unix(secs: i64) -> Civil {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    Civil {
        year,
        month,
        day,
        hour: (rem / 3_600) as u32,
        minute: (rem % 3_600 / 60) as u32,
        weekday: (days + 4).rem_euclid(7) as u32,
    }
}
