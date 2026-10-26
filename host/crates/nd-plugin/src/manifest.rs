// SPDX-License-Identifier: GPL-3.0-or-later
//! `manifest.json` (docs/PLUGINS.md §2). Serialized back to the config UI as is (camelCase not used: keys
//! match the file).

use nd_core::model::{ControlKind, TriggerKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The only protocol version this agent speaks.
pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Path inside the plugin folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub protocol: u32,
    pub entry: Entry,
    #[serde(default)]
    pub permissions: Vec<Permission>,
    pub actions: Vec<ManifestAction>,
}

/// `entry`: one command for every OS, or per OS (`windows` / `linux` / `default`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Entry {
    All(Command),
    PerOs {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        windows: Option<Command>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        linux: Option<Command>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default: Option<Command>,
    },
}

/// A path relative to the plugin folder, or `[program, args...]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Command {
    Path(String),
    Argv(Vec<String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    Network,
    Exec,
    Filesystem,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestAction {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Default `[key]`.
    #[serde(default = "default_controls")]
    pub controls: Vec<ControlKind>,
    /// Bindings the UI creates; the first is the default. Default `[key_down]`.
    #[serde(default = "default_triggers")]
    pub triggers: Vec<TriggerKind>,
    #[serde(default)]
    pub settings: Vec<SettingField>,
    /// Round 20: tile face the UI gives a tile when this action is placed on it (`default`, `media`, `stream`).
    /// Keys, dials and sliders ignore it.
    #[serde(default, skip_serializing_if = "nd_core::model::ControlFace::is_default")]
    pub face: nd_core::model::ControlFace,
}

fn default_controls() -> Vec<ControlKind> {
    vec![ControlKind::Key]
}

fn default_triggers() -> Vec<TriggerKind> {
    vec![TriggerKind::KeyDown]
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingField {
    pub key: String,
    pub label: String,
    #[serde(rename = "type")]
    pub kind: FieldType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    /// `number` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<f64>,
    /// `select` only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<SelectOption>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    Text,
    Password,
    Textarea,
    Number,
    Toggle,
    Select,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SelectOption {
    pub value: String,
    pub label: String,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ManifestError {
    #[error("manifest is not valid JSON: {0}")]
    Json(String),
    #[error("{0}")]
    Invalid(String),
}

impl Manifest {
    /// Parse and validate (all rules in docs/PLUGINS.md §2: id charset/length, protocol == 1, 1–64 actions,
    /// unique action ids and setting keys, `select` has options, non-empty controls/triggers, relative paths
    /// stay inside the folder: no absolute paths, no `..`).
    pub fn parse(json: &str) -> Result<Self, ManifestError> {
        let m: Manifest = serde_json::from_str(json).map_err(|e| {
            use serde_json::error::Category;
            match e.classify() {
                Category::Data => ManifestError::Invalid(format!("manifest: {e}")),
                _ => ManifestError::Json(e.to_string()),
            }
        })?;
        m.validate()?;
        Ok(m)
    }

    fn validate(&self) -> Result<(), ManifestError> {
        let bad = |s: String| Err(ManifestError::Invalid(s));
        if !valid_plugin_id(&self.id) {
            return bad(format!(
                "id {:?} must be 3-64 characters of a-z, 0-9, '.', '-' or '_', starting with a letter",
                self.id
            ));
        }
        if self.name.trim().is_empty() {
            return bad("name is required".into());
        }
        if self.version.trim().is_empty() {
            return bad("version is required".into());
        }
        if self.protocol != PROTOCOL_VERSION {
            return bad(format!("protocol must be {PROTOCOL_VERSION}, got {}", self.protocol));
        }
        if let Some(icon) = &self.icon {
            check_rel("icon", icon)?;
        }
        self.check_entry()?;
        if self.actions.is_empty() || self.actions.len() > 64 {
            return bad(format!("a plugin needs 1-64 actions, has {}", self.actions.len()));
        }
        let mut ids = std::collections::HashSet::new();
        for a in &self.actions {
            if a.id.is_empty()
                || !a.id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
            {
                return bad(format!("action id {:?} must be non-empty and use a-z, 0-9, '_' or '-'", a.id));
            }
            if !ids.insert(a.id.as_str()) {
                return bad(format!("duplicate action id {:?}", a.id));
            }
            if a.name.trim().is_empty() {
                return bad(format!("action {:?} needs a name", a.id));
            }
            if let Some(icon) = &a.icon {
                check_rel(&format!("action {:?} icon", a.id), icon)?;
            }
            if a.controls.is_empty() {
                return bad(format!("action {:?} needs at least one control", a.id));
            }
            if a.triggers.is_empty() {
                return bad(format!("action {:?} needs at least one trigger", a.id));
            }
            let mut keys = std::collections::HashSet::new();
            for f in &a.settings {
                if f.key.is_empty() {
                    return bad(format!("action {:?} has a setting without a key", a.id));
                }
                if !keys.insert(f.key.as_str()) {
                    return bad(format!("action {:?} has a duplicate setting key {:?}", a.id, f.key));
                }
                if f.kind == FieldType::Select && f.options.is_empty() {
                    return bad(format!("select setting {:?} of action {:?} needs options", f.key, a.id));
                }
                if let (Some(lo), Some(hi)) = (f.min, f.max) {
                    if lo > hi {
                        return bad(format!("setting {:?} of action {:?}: min is above max", f.key, a.id));
                    }
                }
            }
        }
        Ok(())
    }

    fn check_entry(&self) -> Result<(), ManifestError> {
        let cmds: Vec<(&str, &Command)> = match &self.entry {
            Entry::All(c) => vec![("entry", c)],
            Entry::PerOs { windows, linux, default } => {
                [("entry.windows", windows), ("entry.linux", linux), ("entry.default", default)]
                    .into_iter()
                    .filter_map(|(n, c)| c.as_ref().map(|c| (n, c)))
                    .collect()
            }
        };
        for (name, c) in cmds {
            match c {
                Command::Path(p) => check_rel(name, p)?,
                Command::Argv(v) => {
                    let Some(prog) = v.first() else {
                        return Err(ManifestError::Invalid(format!("{name} must not be an empty array")));
                    };
                    check_rel(name, prog)?;
                }
            }
        }
        Ok(())
    }

    /// The command for the current OS (`windows` / `linux`, else `default`, else `All`), as argv.
    /// `None` = unsupported on this OS.
    pub fn command_for_os(&self) -> Option<Vec<String>> {
        let cmd = match &self.entry {
            Entry::All(c) => Some(c),
            Entry::PerOs { windows, linux, default } => {
                let own = if cfg!(windows) { windows } else { linux };
                own.as_ref().or(default.as_ref())
            }
        }?;
        Some(match cmd {
            Command::Path(p) => vec![p.clone()],
            Command::Argv(v) => v.clone(),
        })
    }

    pub fn action(&self, id: &str) -> Option<&ManifestAction> {
        self.actions.iter().find(|a| a.id == id)
    }
}

/// Plugin id rule: 3-64 chars of `[a-z0-9._-]`, starting with a letter.
pub fn valid_plugin_id(id: &str) -> bool {
    let b = id.as_bytes();
    (3..=64).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'-' | b'_'))
}

/// A path inside the plugin folder: non-empty, relative, no `..`, no drive letter.
pub fn is_safe_rel(path: &str) -> bool {
    if path.is_empty() || path.starts_with('/') || path.starts_with('\\') || path.contains('\0') {
        return false;
    }
    if path.as_bytes().get(1) == Some(&b':') {
        return false;
    }
    !path.split(['/', '\\']).any(|c| c == "..")
}

fn check_rel(what: &str, path: &str) -> Result<(), ManifestError> {
    if is_safe_rel(path) {
        Ok(())
    } else {
        Err(ManifestError::Invalid(format!("{what}: {path:?} must be a relative path inside the plugin folder")))
    }
}
