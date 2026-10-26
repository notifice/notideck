// SPDX-License-Identifier: GPL-3.0-or-later
//! `obs.json` in the config dir: Notideck's OBS connection settings (round 14, M8).

use nd_obs::ObsSettings;
use std::path::Path;
use tracing::warn;

const FILE: &str = "obs.json";

/// Load `obs.json`; missing is silent, invalid warns. Both give the default settings.
pub fn load(root: &Path) -> ObsSettings {
    match std::fs::read(root.join(FILE)) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            warn!("obs.json is invalid ({e}); using defaults");
            ObsSettings::default()
        }),
        Err(_) => ObsSettings::default(),
    }
}

/// Atomic save: write a temp file, then rename over the real one.
pub fn save(root: &Path, settings: &ObsSettings) -> std::io::Result<()> {
    std::fs::create_dir_all(root)?;
    let json = serde_json::to_vec_pretty(settings).map_err(std::io::Error::other)?;
    let tmp = root.join(format!("{FILE}.tmp"));
    std::fs::write(&tmp, json)?;
    std::fs::rename(tmp, root.join(FILE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_missing_and_invalid() {
        let dir = std::env::temp_dir().join(format!("nd-obs-cfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(load(&dir), ObsSettings::default());
        let s = ObsSettings { enabled: false, auto_detect: false, host: "h".into(), port: 9, password: "pw".into() };
        save(&dir, &s).unwrap();
        assert_eq!(load(&dir), s);
        assert!(!dir.join("obs.json.tmp").exists());
        std::fs::write(dir.join("obs.json"), "{oops").unwrap();
        assert_eq!(load(&dir), ObsSettings::default());
        std::fs::remove_dir_all(&dir).ok();
    }
}
