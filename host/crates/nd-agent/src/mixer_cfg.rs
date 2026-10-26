// SPDX-License-Identifier: GPL-3.0-or-later
//! `mixer.json` in the config dir: which apps are hidden / pinned on the mixer page.

use nd_core::MixerConfig;
use std::path::Path;
use tracing::warn;

const FILE: &str = "mixer.json";

/// Load `mixer.json`; missing is silent, invalid warns. Both give the default config.
pub fn load(root: &Path) -> MixerConfig {
    match std::fs::read(root.join(FILE)) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            warn!("mixer.json is invalid ({e}); using defaults");
            MixerConfig::default()
        }),
        Err(_) => MixerConfig::default(),
    }
}

/// Atomic save: write a temp file, then rename over the real one.
pub fn save(root: &Path, config: &MixerConfig) -> std::io::Result<()> {
    std::fs::create_dir_all(root)?;
    let json = serde_json::to_vec_pretty(config).map_err(std::io::Error::other)?;
    let tmp = root.join(format!("{FILE}.tmp"));
    std::fs::write(&tmp, json)?;
    std::fs::rename(tmp, root.join(FILE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_missing_and_invalid() {
        let dir = std::env::temp_dir().join(format!("nd-mixer-cfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(load(&dir), MixerConfig::default());
        let cfg = MixerConfig { order: vec!["spotify".into()], hidden: vec!["chrome".into()], show_system: true };
        save(&dir, &cfg).unwrap();
        assert_eq!(load(&dir), cfg);
        assert!(!dir.join("mixer.json.tmp").exists());
        std::fs::write(dir.join("mixer.json"), "{oops").unwrap();
        assert_eq!(load(&dir), MixerConfig::default());
        std::fs::remove_dir_all(&dir).ok();
    }
}
