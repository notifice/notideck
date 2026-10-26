// SPDX-License-Identifier: GPL-3.0-or-later
//! Keyboard injection access checks and fixes (Linux: `/dev/uinput` permissions via pkexec).

use crate::{KeyboardAccess, PlatformError, Result};

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use std::fs::OpenOptions;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    const RULES: &str = include_str!("../../../../../packaging/linux/70-notideck-uinput.rules");

    /// Fixed script run as root. `$1` is the path of the temp file holding the udev rule.
    /// No user data is interpolated into it.
    const SCRIPT: &str = "modprobe uinput || true; \
        install -m 0644 \"$1\" /etc/udev/rules.d/70-notideck-uinput.rules && \
        echo uinput > /etc/modules-load.d/notideck-uinput.conf && \
        udevadm control --reload && \
        udevadm trigger --sysname-match=uinput --action=change";

    const MANUAL: &str = "Run once in a terminal: sudo modprobe uinput && \
        sudo cp packaging/linux/70-notideck-uinput.rules /etc/udev/rules.d/ && \
        sudo udevadm control --reload && sudo udevadm trigger";

    pub fn access() -> KeyboardAccess {
        match OpenOptions::new().write(true).open("/dev/uinput") {
            Ok(_) => KeyboardAccess::Ok,
            Err(e) => match e.kind() {
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::NotFound => KeyboardAccess::Missing,
                _ => KeyboardAccess::Unsupported,
            },
        }
    }

    pub fn fix() -> Result<()> {
        if access() == KeyboardAccess::Ok {
            return Ok(());
        }
        let tmp = std::env::temp_dir().join(format!("notideck-uinput-{}.rules", std::process::id()));
        std::fs::write(&tmp, RULES)?;
        let result = run_pkexec(&tmp);
        let _ = std::fs::remove_file(&tmp);
        result?;

        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if access() == KeyboardAccess::Ok {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(PlatformError::Other(
                    "the udev rule was installed but /dev/uinput is still not writable; \
                     log out and back in (or replug the session) and try again"
                        .into(),
                ));
            }
            std::thread::sleep(Duration::from_millis(150));
        }
    }

    fn run_pkexec(rules: &std::path::Path) -> Result<()> {
        let status =
            Command::new("pkexec").args(["/bin/sh", "-c", SCRIPT, "sh"]).arg(rules).stdin(Stdio::null()).status();
        match status {
            Ok(s) if s.success() => Ok(()),
            Ok(s) => match s.code() {
                // 126: not authorized / dialog dismissed, 127: authentication failed.
                Some(126) | Some(127) => Err(PlatformError::PermissionDenied("authorization was cancelled".into())),
                other => Err(PlatformError::Other(format!("pkexec failed (exit {other:?}). {MANUAL}"))),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(PlatformError::Other(format!(
                "pkexec (polkit) is not installed, so permissions cannot be fixed automatically. {MANUAL}"
            ))),
            Err(e) => Err(e.into()),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn embedded_rule_is_uaccess_uinput() {
            assert!(RULES.contains("KERNEL==\"uinput\""));
            assert!(RULES.contains("uaccess"));
        }

        #[test]
        fn script_is_fixed_and_uses_positional_arg() {
            assert!(SCRIPT.contains("\"$1\""));
            assert!(!SCRIPT.contains('{'));
        }
    }
}

pub(crate) fn access() -> KeyboardAccess {
    #[cfg(target_os = "linux")]
    return imp::access();
    #[cfg(windows)]
    return KeyboardAccess::Ok;
    #[allow(unreachable_code)]
    KeyboardAccess::Unsupported
}

pub(crate) fn fix() -> Result<()> {
    #[cfg(target_os = "linux")]
    return imp::fix();
    #[cfg(windows)]
    return Ok(());
    #[allow(unreachable_code)]
    Err(PlatformError::Unsupported("keyboard access fix"))
}
