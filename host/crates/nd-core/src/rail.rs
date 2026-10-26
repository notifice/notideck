// SPDX-License-Identifier: GPL-3.0-or-later
//! Side-rail pinned keys (round 11): global Key controls shown on the phone's rail.

use crate::model::{Control, ControlKind};
use serde::{Deserialize, Serialize};

/// `page_id` of rail controls in `InputEvent` and `UpdateControls`.
pub const RAIL_PAGE_ID: &str = "__rail";

/// Most keys the rail can pin.
pub const MAX_RAIL_KEYS: usize = 3;

/// Contents of `rail.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RailConfig {
    #[serde(default)]
    pub keys: Vec<Control>,
}

/// Human-readable problems with a rail key list (empty = valid).
pub fn validate_rail(keys: &[Control]) -> Vec<String> {
    let mut errors = Vec::new();
    if keys.len() > MAX_RAIL_KEYS {
        errors.push(format!("The rail holds at most {MAX_RAIL_KEYS} keys (got {}).", keys.len()));
    }
    for (i, k) in keys.iter().enumerate() {
        if k.id.trim().is_empty() {
            errors.push(format!("Rail key {} has no id.", i + 1));
        }
        if k.kind != ControlKind::Key {
            errors.push(format!("Rail key '{}' must be a Key (got {:?}).", k.id, k.kind));
        }
        if keys[..i].iter().any(|o| o.id == k.id) {
            errors.push(format!("Rail key id '{}' is used twice.", k.id));
        }
    }
    errors
}

/// Distinct non-empty icon hashes used by the rail keys (including per-state icons).
pub fn rail_icon_hashes(keys: &[Control]) -> Vec<String> {
    let mut set = std::collections::BTreeSet::new();
    for c in keys {
        if !c.icon_hash.is_empty() {
            set.insert(c.icon_hash.clone());
        }
        for s in &c.states {
            if let Some(h) = s.icon_hash.as_ref().filter(|h| !h.is_empty()) {
                set.insert(h.clone());
            }
        }
    }
    set.into_iter().collect()
}

const FILE: &str = "rail.json";

/// Load `rail.json` from the config root. Missing, invalid or invalid-content files give an empty
/// rail (with a warning for the latter two).
pub fn load_rail(root: &std::path::Path) -> Vec<Control> {
    let bytes = match std::fs::read(root.join(FILE)) {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };
    match serde_json::from_slice::<RailConfig>(&bytes) {
        Ok(cfg) => {
            let errors = validate_rail(&cfg.keys);
            if errors.is_empty() {
                cfg.keys
            } else {
                tracing::warn!("rail.json is invalid ({}); the rail will be empty", errors.join(" "));
                Vec::new()
            }
        }
        Err(e) => {
            tracing::warn!("rail.json is invalid ({e}); the rail will be empty");
            Vec::new()
        }
    }
}

/// Atomic save: write a temp file, then rename over the real one.
pub fn save_rail(root: &std::path::Path, keys: &[Control]) -> std::io::Result<()> {
    std::fs::create_dir_all(root)?;
    let json = serde_json::to_vec_pretty(&RailConfig { keys: keys.to_vec() }).map_err(std::io::Error::other)?;
    let tmp = root.join(format!("{FILE}.tmp"));
    std::fs::write(&tmp, json)?;
    std::fs::rename(tmp, root.join(FILE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation() {
        let k = |id: &str| Control::key(id, 0, 0, id);
        assert!(validate_rail(&[k("a"), k("b"), k("c")]).is_empty());
        assert_eq!(validate_rail(&[k("a"), k("b"), k("c"), k("d")]).len(), 1);
        assert_eq!(validate_rail(&[k("a"), k("a")]).len(), 1);
        let mut tile = k("t");
        tile.kind = ControlKind::Tile;
        assert_eq!(validate_rail(&[tile]).len(), 1);
        assert_eq!(validate_rail(&[k("")]).len(), 1);
    }

    #[test]
    fn load_save_and_bad_files() {
        let dir = std::env::temp_dir().join(format!("nd-rail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(load_rail(&dir).is_empty());
        let keys = vec![Control::key("a", 0, 0, "A")];
        save_rail(&dir, &keys).unwrap();
        assert_eq!(load_rail(&dir), keys);
        std::fs::write(dir.join("rail.json"), "not json").unwrap();
        assert!(load_rail(&dir).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn config_roundtrip() {
        let c = RailConfig { keys: vec![Control::key("a", 0, 0, "A")] };
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<RailConfig>(&json).unwrap(), c);
        assert!(serde_json::from_str::<RailConfig>("{}").unwrap().keys.is_empty());
    }
}

#[cfg(test)]
mod session_tests {
    use super::*;
    use crate::model::{Action, Page, Profile};
    use crate::session::{DeckSession, InputReaction};
    use nd_proto::v1::{InputEvent, InputKind};

    fn session() -> DeckSession {
        let mut p = Profile::new("p", "P");
        for id in ["a", "b", "c"] {
            p.add_page(Page::new(id, id));
        }
        DeckSession::new(p)
    }

    #[test]
    fn page_position_and_go_to_index() {
        let mut s = session();
        assert_eq!(s.page_position(), (0, 3));
        assert!(s.go_to_page_index(2).unwrap().is_ok());
        assert_eq!(s.page_position(), (2, 3));
        assert!(s.go_to_page_index(3).is_none());
        s.open_folder("a").unwrap();
        assert_eq!(s.page_position(), (0, 0));
        s.go_to_page_index(1).unwrap().unwrap();
        assert!(!s.is_in_folder());
        assert_eq!(s.page_position(), (1, 3));
    }

    #[test]
    fn rail_toggle_state_is_separate_from_pages() {
        let mut s = session();
        s.set_rail(vec![Control::key("k", 0, 0, "K").with_action(Action::ToggleState)]);
        let router = crate::ActionRouter::new(
            std::sync::Arc::new(crate::mock::MockInputInjector::new()),
            std::sync::Arc::new(crate::mock::MockLauncher::new()),
        );
        let ev = InputEvent {
            page_id: RAIL_PAGE_ID.into(),
            control_id: "k".into(),
            kind: InputKind::KeyDown as i32,
            ..Default::default()
        };
        match s.handle_input(&ev, &router).unwrap() {
            InputReaction::UpdateControls(u) => {
                assert_eq!(u.page_id, RAIL_PAGE_ID);
                assert_eq!(u.patches[0].control.as_ref().unwrap().state_index, 1);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(s.rail()[0].state_index, 1);
    }
}
