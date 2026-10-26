// SPDX-License-Identifier: GPL-3.0-or-later
//! Volume / mute via `pactl` (PulseAudio, PipeWire-pulse) with a `wpctl` (WirePlumber) fallback.

use std::process::Command;

use crate::{AudioControl, AudioTarget, PlatformError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    Pactl,
    Wpctl,
}

/// Audio control by spawning `pactl` / `wpctl` (never through a shell).
#[derive(Debug)]
pub struct PulseAudio {
    tool: Tool,
}

impl PulseAudio {
    /// Probe `pactl`, then `wpctl`; error if neither can read the default output volume.
    pub fn open() -> Result<Self> {
        for tool in [Tool::Pactl, Tool::Wpctl] {
            let me = Self { tool };
            if me.volume(AudioTarget::Output).is_ok() {
                return Ok(me);
            }
        }
        Err(PlatformError::Other("neither pactl nor wpctl is usable".into()))
    }

    fn run(&self, args: &[&str]) -> Result<String> {
        let prog = match self.tool {
            Tool::Pactl => "pactl",
            Tool::Wpctl => "wpctl",
        };
        let out = Command::new(prog).args(args).env("LC_ALL", "C").stdin(std::process::Stdio::null()).output()?;
        if !out.status.success() {
            return Err(PlatformError::Other(format!(
                "{prog} {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn dev(&self, t: AudioTarget) -> &'static str {
        match (self.tool, t) {
            (Tool::Pactl, AudioTarget::Output) => "@DEFAULT_SINK@",
            (Tool::Pactl, AudioTarget::Input) => "@DEFAULT_SOURCE@",
            (Tool::Wpctl, AudioTarget::Output) => "@DEFAULT_AUDIO_SINK@",
            (Tool::Wpctl, AudioTarget::Input) => "@DEFAULT_AUDIO_SOURCE@",
        }
    }

    fn pactl_kind(t: AudioTarget) -> &'static str {
        match t {
            AudioTarget::Output => "sink",
            AudioTarget::Input => "source",
        }
    }
}

impl AudioControl for PulseAudio {
    fn backend_name(&self) -> &'static str {
        match self.tool {
            Tool::Pactl => "pactl",
            Tool::Wpctl => "wpctl",
        }
    }
    fn volume(&self, t: AudioTarget) -> Result<f32> {
        let dev = self.dev(t);
        match self.tool {
            Tool::Pactl => {
                let cmd = format!("get-{}-volume", Self::pactl_kind(t));
                parse_pactl_volume(&self.run(&[&cmd, dev])?)
            }
            Tool::Wpctl => parse_wpctl_volume(&self.run(&["get-volume", dev])?).map(|(v, _)| v),
        }
    }

    fn set_volume(&self, t: AudioTarget, volume: f32) -> Result<()> {
        let v = if volume.is_finite() { volume.clamp(0.0, 1.0) } else { 0.0 };
        let dev = self.dev(t);
        match self.tool {
            Tool::Pactl => {
                let cmd = format!("set-{}-volume", Self::pactl_kind(t));
                self.run(&[&cmd, dev, &format!("{}%", (v * 100.0).round() as u32)])?;
            }
            Tool::Wpctl => {
                self.run(&["set-volume", dev, &format!("{v:.2}")])?;
            }
        }
        Ok(())
    }

    fn muted(&self, t: AudioTarget) -> Result<bool> {
        let dev = self.dev(t);
        match self.tool {
            Tool::Pactl => {
                let cmd = format!("get-{}-mute", Self::pactl_kind(t));
                parse_pactl_mute(&self.run(&[&cmd, dev])?)
            }
            Tool::Wpctl => parse_wpctl_volume(&self.run(&["get-volume", dev])?).map(|(_, m)| m),
        }
    }

    fn set_muted(&self, t: AudioTarget, muted: bool) -> Result<()> {
        let dev = self.dev(t);
        let flag = if muted { "1" } else { "0" };
        match self.tool {
            Tool::Pactl => {
                let cmd = format!("set-{}-mute", Self::pactl_kind(t));
                self.run(&[&cmd, dev, flag])?;
            }
            Tool::Wpctl => {
                self.run(&["set-mute", dev, flag])?;
            }
        }
        Ok(())
    }
}

fn bad(what: &str, s: &str) -> PlatformError {
    PlatformError::Other(format!("unparseable {what} output: {:?}", s.trim()))
}

/// Parse `Volume: front-left: 29491 /  45% / -20.81 dB, front-right: ...` into the
/// channel-averaged linear volume, clamped to `0..=1`.
pub(crate) fn parse_pactl_volume(s: &str) -> Result<f32> {
    let line = s.lines().find(|l| l.trim_start().starts_with("Volume:")).ok_or_else(|| bad("pactl volume", s))?;
    let pcts: Vec<f32> = line
        .split('/')
        .filter_map(|seg| seg.trim().strip_suffix('%'))
        .filter_map(|n| n.trim().parse::<f32>().ok())
        .collect();
    if pcts.is_empty() {
        return Err(bad("pactl volume", s));
    }
    let avg = pcts.iter().sum::<f32>() / pcts.len() as f32;
    Ok((avg / 100.0).clamp(0.0, 1.0))
}

/// Parse `Mute: yes|no`.
pub(crate) fn parse_pactl_mute(s: &str) -> Result<bool> {
    let line = s.lines().find(|l| l.trim_start().starts_with("Mute:")).ok_or_else(|| bad("pactl mute", s))?;
    match line.trim_start()["Mute:".len()..].trim() {
        "yes" => Ok(true),
        "no" => Ok(false),
        _ => Err(bad("pactl mute", s)),
    }
}

/// Parse `Volume: 0.45` / `Volume: 0.45 [MUTED]` into `(volume, muted)`.
pub(crate) fn parse_wpctl_volume(s: &str) -> Result<(f32, bool)> {
    let rest = s.trim().strip_prefix("Volume:").ok_or_else(|| bad("wpctl volume", s))?;
    let v: f32 = rest.split_whitespace().next().and_then(|n| n.parse().ok()).ok_or_else(|| bad("wpctl volume", s))?;
    Ok((v.clamp(0.0, 1.0), rest.contains("[MUTED]")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pactl_stereo() {
        let s = "Volume: front-left: 29491 /  45% / -20.81 dB,   front-right: 29491 /  45% / -20.81 dB\n        balance 0.00\n";
        assert!((parse_pactl_volume(s).unwrap() - 0.45).abs() < 1e-4);
    }

    #[test]
    fn pactl_averages_and_clamps() {
        let s = "Volume: mono: 65536 / 100% / 0.00 dB\n";
        assert_eq!(parse_pactl_volume(s).unwrap(), 1.0);
        let s = "Volume: front-left: 0 /   0% / -inf dB,   front-right: 65536 / 100% / 0.00 dB\n";
        assert!((parse_pactl_volume(s).unwrap() - 0.5).abs() < 1e-4);
        let s = "Volume: mono: 98304 / 150% / 10.54 dB\n";
        assert_eq!(parse_pactl_volume(s).unwrap(), 1.0);
    }

    #[test]
    fn pactl_garbage() {
        assert!(parse_pactl_volume("").is_err());
        assert!(parse_pactl_volume("Volume: nonsense").is_err());
    }

    #[test]
    fn pactl_mute() {
        assert!(parse_pactl_mute("Mute: yes\n").unwrap());
        assert!(!parse_pactl_mute("Mute: no\n").unwrap());
        assert!(parse_pactl_mute("Mute: maybe").is_err());
    }

    #[test]
    fn wpctl() {
        assert_eq!(parse_wpctl_volume("Volume: 0.45\n").unwrap(), (0.45, false));
        assert_eq!(parse_wpctl_volume("Volume: 0.45 [MUTED]\n").unwrap(), (0.45, true));
        assert_eq!(parse_wpctl_volume("Volume: 1.30\n").unwrap().0, 1.0);
        assert!(parse_wpctl_volume("nope").is_err());
    }
}

// ---------------------------------------------------------------- per-app mixer (round 12, M4)

fn run_pactl(args: &[&str]) -> Result<String> {
    let out = Command::new("pactl").args(args).env("LC_ALL", "C").stdin(std::process::Stdio::null()).output()?;
    if !out.status.success() {
        return Err(PlatformError::Other(format!(
            "pactl {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn list_sink_inputs() -> Result<String> {
    run_pactl(&["-f", "json", "list", "sink-inputs"])
}

/// Per-app volume via `pactl -f json list sink-inputs` (PulseAudio 16+ and PipeWire-pulse).
#[derive(Debug)]
pub struct PulseMixer;

impl PulseMixer {
    /// Error if `pactl -f json list sink-inputs` doesn't work.
    pub fn open() -> Result<Self> {
        let out = list_sink_inputs().map_err(|e| {
            PlatformError::Other(format!("per-app audio needs a pactl with JSON output (-f json): {e}"))
        })?;
        parse_sink_inputs(&out)?;
        Ok(Self)
    }
}

fn check_id(id: &str) -> Result<()> {
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        return Err(PlatformError::Other(format!("invalid sink-input id {id:?}")));
    }
    Ok(())
}

/// Parse the output of `pactl -f json list sink-inputs` into sessions.
pub(crate) fn parse_sink_inputs(json: &str) -> Result<Vec<crate::AudioSession>> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| PlatformError::Other(format!("unparseable pactl JSON: {e}")))?;
    let arr = v.as_array().ok_or_else(|| PlatformError::Other("pactl JSON is not an array".into()))?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let Some(index) = item.get("index").and_then(|i| i.as_u64()) else { continue };
        let props = item.get("properties");
        let prop = |k: &str| -> Option<&str> {
            props.and_then(|p| p.get(k)).and_then(|x| x.as_str()).map(str::trim).filter(|s| !s.is_empty())
        };
        let process = prop("application.process.binary").unwrap_or("").to_string();
        let display_name = prop("application.name")
            .or_else(|| prop("application.process.binary"))
            .or_else(|| prop("media.name"))
            .unwrap_or("")
            .to_string();
        let pcts: Vec<f32> = item
            .get("volume")
            .and_then(|v| v.as_object())
            .map(|o| {
                o.values()
                    .filter_map(|c| c.get("value_percent").and_then(|p| p.as_str()))
                    .filter_map(|p| p.trim().trim_end_matches('%').trim().parse::<f32>().ok())
                    .collect()
            })
            .unwrap_or_default();
        let volume =
            if pcts.is_empty() { 0.0 } else { (pcts.iter().sum::<f32>() / pcts.len() as f32 / 100.0).clamp(0.0, 1.0) };
        out.push(crate::AudioSession {
            id: index.to_string(),
            pid: prop("application.process.id").and_then(|p| p.parse().ok()),
            process,
            display_name,
            volume,
            muted: item.get("mute").and_then(|m| m.as_bool()).unwrap_or(false),
            active: !item.get("corked").and_then(|m| m.as_bool()).unwrap_or(false),
            is_system: false,
            icon_hint: prop("application.icon_name").map(str::to_owned),
        });
    }
    Ok(out)
}

impl crate::AppAudio for PulseMixer {
    fn backend_name(&self) -> &'static str {
        "pactl"
    }
    fn sessions(&self) -> Result<Vec<crate::AudioSession>> {
        parse_sink_inputs(&list_sink_inputs()?)
    }
    fn set_session_volume(&self, id: &str, volume: f32) -> Result<()> {
        check_id(id)?;
        let pct = (volume.clamp(0.0, 1.0) * 100.0).round() as u32;
        run_pactl(&["set-sink-input-volume", id, &format!("{pct}%")]).map(|_| ())
    }
    fn set_session_muted(&self, id: &str, muted: bool) -> Result<()> {
        check_id(id)?;
        run_pactl(&["set-sink-input-mute", id, if muted { "1" } else { "0" }]).map(|_| ())
    }
}

#[cfg(test)]
mod mixer_tests {
    use super::*;
    use crate::AppAudio;

    const PIPEWIRE: &str = r#"[
 {"index": 57, "driver": "PipeWire", "owner_module": "4294967295", "client": "61", "sink": 49,
  "sample_specification": "float32le 2ch 48000Hz", "corked": false, "mute": false, "has_volume": true,
  "volume": {"front-left": {"value": 39321, "value_percent": "60%", "db": "-13.33 dB"},
             "front-right": {"value": 45874, "value_percent": "70%", "db": "-9.43 dB"}},
  "properties": {"media.name": "Playback", "application.name": "Firefox",
    "application.process.id": "2345", "application.process.binary": "firefox",
    "application.icon_name": "firefox", "media.role": "video"}},
 {"index": 60, "corked": true, "mute": true,
  "volume": {"mono": {"value": 98304, "value_percent": "150%", "db": "10.54 dB"}},
  "properties": {"application.name": "Spotify", "application.process.binary": "spotify"}}
]"#;

    const PULSE16: &str = r#"[
 {"index": 3, "corked": false, "mute": false,
  "volume": {"mono": {"value": 32768, "value_percent": "50%", "db": "-18.06 dB"}},
  "properties": {"media.name": "Notification sound"}},
 {"index": 4, "corked": false, "mute": false,
  "volume": {"front-left": {"value": 65536, "value_percent": "100%", "db": "0.00 dB"},
             "front-right": {"value": 0, "value_percent": "0%", "db": "-inf dB"}},
  "properties": {"application.name": "mpv", "application.process.id": "oops"}}
]"#;

    #[test]
    fn pipewire_shape() {
        let s = parse_sink_inputs(PIPEWIRE).unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].id, "57");
        assert_eq!(s[0].pid, Some(2345));
        assert_eq!(s[0].process, "firefox");
        assert_eq!(s[0].display_name, "Firefox");
        assert!((s[0].volume - 0.65).abs() < 1e-4);
        assert!(!s[0].muted && s[0].active && !s[0].is_system);
        assert_eq!(s[0].icon_hint.as_deref(), Some("firefox"));
    }

    #[test]
    fn clamps_mute_corked_missing_props() {
        let s = parse_sink_inputs(PIPEWIRE).unwrap();
        assert_eq!(s[1].volume, 1.0);
        assert!(s[1].muted && !s[1].active);
        assert_eq!(s[1].pid, None);
        assert_eq!(s[1].icon_hint, None);
    }

    #[test]
    fn pulse16_shape() {
        let s = parse_sink_inputs(PULSE16).unwrap();
        assert_eq!(s[0].process, "");
        assert_eq!(s[0].display_name, "Notification sound");
        assert!((s[0].volume - 0.5).abs() < 1e-4);
        assert_eq!(s[1].display_name, "mpv");
        assert_eq!(s[1].pid, None);
        assert!((s[1].volume - 0.5).abs() < 1e-4);
    }

    #[test]
    fn bad_json() {
        assert!(parse_sink_inputs("").is_err());
        assert!(parse_sink_inputs("{}").is_err());
        assert!(parse_sink_inputs("not json").is_err());
        assert!(parse_sink_inputs("[]").unwrap().is_empty());
    }

    #[test]
    fn id_validation() {
        for bad in ["", "abc", "12;ls", "-1", "1 2", "1.5"] {
            assert!(check_id(bad).is_err(), "{bad}");
            assert!(PulseMixer.set_session_volume(bad, 0.5).is_err());
            assert!(PulseMixer.set_session_muted(bad, true).is_err());
        }
        assert!(check_id("57").is_ok());
    }

    #[test]
    #[ignore = "needs a running PulseAudio/PipeWire with pactl"]
    fn live_sessions() {
        let m = PulseMixer::open().unwrap();
        println!("{:?}", m.sessions().unwrap());
    }
}
