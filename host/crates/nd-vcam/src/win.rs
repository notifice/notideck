// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows side: the shared-memory mapping (used by the DLL to create and by the app to open),
//! HKLM registration of the DLL (call only from an elevated process), and the Media Foundation
//! virtual camera object.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_ALREADY_EXISTS, HANDLE, HLOCAL, INVALID_HANDLE_VALUE,
};
use windows::Win32::Media::MediaFoundation::{
    IMFVirtualCamera, MFCreateVirtualCamera, MFShutdown, MFStartup, MFVirtualCameraAccess_CurrentUser,
    MFVirtualCameraLifetime_Session, MFVirtualCameraType_SoftwareCameraSource, MFSTARTUP_FULL, MF_VERSION,
};
use windows::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, FILE_MAP_ALL_ACCESS,
    MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READWRITE,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegGetValueW, RegSetValueExW, HKEY, HKEY_LOCAL_MACHINE,
    KEY_WOW64_64KEY, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ,
};

use crate::registration::{self, InstallState, Registry, CLSID_DESCRIPTION, DLL_NAME, INSTALL_SUBDIR};
use crate::shm::{Shm, CLSID_STR, FRIENDLY_NAME, REGION_BYTES, SECTION_NAME};

/// Allows SYSTEM, Local Service and Interactive Users full access (ADR 0005 decision 5).
pub const SECTION_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;LS)(A;;GA;;;IU)";
/// Set to skip the `Global\` namespace (tests, non-elevated experiments).
pub const LOCAL_ONLY_ENV: &str = "NOTIDECK_VCAM_LOCAL_ONLY";

/// Which object namespace the mapping lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Global,
    Local,
}

fn section_name(scope: Scope) -> HSTRING {
    match scope {
        Scope::Global => HSTRING::from(SECTION_NAME),
        Scope::Local => {
            let base = SECTION_NAME.strip_prefix("Global\\").unwrap_or(SECTION_NAME);
            HSTRING::from(format!("Local\\{base}"))
        }
    }
}

fn local_only() -> bool {
    std::env::var_os(LOCAL_ONLY_ENV).is_some()
}

/// A mapped view of the shared section. Unmaps and closes on drop.
pub struct ShmMapping {
    handle: HANDLE,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    shm: Shm,
    scope: Scope,
    created: bool,
}

// SAFETY: the handle and view are plain kernel/process resources; `Shm` is already Send + Sync.
unsafe impl Send for ShmMapping {}
unsafe impl Sync for ShmMapping {}

impl ShmMapping {
    fn from_handle(handle: HANDLE, scope: Scope, created: bool) -> Result<Self> {
        // SAFETY: handle is a valid section handle; we map the whole contract region.
        let view = unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, REGION_BYTES) };
        if view.Value.is_null() {
            let err = windows::core::Error::from_win32();
            unsafe { CloseHandle(handle).ok() };
            return Err(anyhow!(err).context("MapViewOfFile"));
        }
        // SAFETY: the view stays mapped for REGION_BYTES until Drop; page aligned.
        let shm = unsafe { Shm::from_raw(view.Value as *mut u8) };
        Ok(ShmMapping { handle, view, shm, scope, created })
    }

    /// App side: open an existing section (`Global\` first, then `Local\`). `None` while the
    /// DLL has not created and initialised it yet.
    pub fn open() -> Option<Self> {
        let scopes: &[Scope] = if local_only() { &[Scope::Local] } else { &[Scope::Global, Scope::Local] };
        for &scope in scopes {
            let name = section_name(scope);
            let h = unsafe { OpenFileMappingW(FILE_MAP_ALL_ACCESS.0, false, &name) };
            if let Ok(h) = h {
                if let Ok(m) = Self::from_handle(h, scope, false) {
                    if m.shm.is_valid() {
                        return Some(m);
                    }
                }
            }
        }
        None
    }

    /// DLL side: create the section (or open it if it already exists) and initialise a fresh
    /// region. Tries `Global\` with the SDDL above; falls back to `Local\` (e.g. when the DLL is
    /// loaded in a normal user process).
    pub fn create() -> Result<Self> {
        let mut last_err = None;
        if !local_only() {
            match Self::create_scope(Scope::Global) {
                Ok(m) => return Ok(m),
                Err(e) => last_err = Some(e),
            }
        }
        match Self::create_scope(Scope::Local) {
            Ok(m) => Ok(m),
            Err(e) => match last_err {
                Some(g) => Err(e.context(format!("Global\\ also failed: {g:#}"))),
                None => Err(e),
            },
        }
    }

    fn create_scope(scope: Scope) -> Result<Self> {
        let name = section_name(scope);
        let mut sd = PSECURITY_DESCRIPTOR::default();
        let mut sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: false.into(),
        };
        if scope == Scope::Global {
            let sddl = HSTRING::from(SECTION_SDDL);
            unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(&sddl, SDDL_REVISION_1, &mut sd, None) }
                .context("SDDL conversion")?;
            sa.lpSecurityDescriptor = sd.0;
        }
        let sa_ptr = if scope == Scope::Global { Some(&sa as *const SECURITY_ATTRIBUTES) } else { None };
        let r =
            unsafe { CreateFileMappingW(INVALID_HANDLE_VALUE, sa_ptr, PAGE_READWRITE, 0, REGION_BYTES as u32, &name) };
        // GetLastError must be read right after the call.
        let already = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        if !sd.0.is_null() {
            unsafe { LocalFree(HLOCAL(sd.0)) };
        }
        let h = r.with_context(|| format!("CreateFileMappingW({name})"))?;
        let m = Self::from_handle(h, scope, !already)?;
        if !m.shm.is_valid() {
            m.shm.init();
        }
        Ok(m)
    }

    pub fn shm(&self) -> &Shm {
        &self.shm
    }

    pub fn scope(&self) -> Scope {
        self.scope
    }

    /// True if this call created the section (false: it already existed).
    pub fn created(&self) -> bool {
        self.created
    }
}

impl Drop for ShmMapping {
    fn drop(&mut self) {
        unsafe {
            let _ = UnmapViewOfFile(self.view);
            let _ = CloseHandle(self.handle);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Registration (elevated callers only)
// ---------------------------------------------------------------------------------------------

fn clsid_key_path() -> String {
    format!("SOFTWARE\\Classes\\CLSID\\{CLSID_STR}")
}

/// `HKLM\SOFTWARE\Classes\CLSID\{CLSID}\InprocServer32`, 64-bit view.
struct HklmRegistry;

impl HklmRegistry {
    fn set_sz(key: HKEY, name: PCWSTR, value: &str) -> Result<()> {
        let wide: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = unsafe { std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2) };
        unsafe { RegSetValueExW(key, name, 0, REG_SZ, Some(bytes)) }.ok().map_err(|e| anyhow!(e))
    }
}

impl Registry for HklmRegistry {
    fn read_server_path(&self) -> Option<PathBuf> {
        let sub = HSTRING::from(format!("{}\\InprocServer32", clsid_key_path()));
        let mut buf = vec![0u16; 1024];
        let mut len = (buf.len() * 2) as u32;
        let r = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                &sub,
                PCWSTR::null(),
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr() as *mut _),
                Some(&mut len),
            )
        };
        if r.is_err() {
            return None;
        }
        let n = (len as usize / 2).saturating_sub(1);
        Some(PathBuf::from(String::from_utf16_lossy(&buf[..n])))
    }

    fn write_server_path(&self, dll: &Path) -> Result<()> {
        let sub = HSTRING::from(format!("{}\\InprocServer32", clsid_key_path()));
        let parent = HSTRING::from(clsid_key_path());
        unsafe {
            let mut key = HKEY::default();
            RegCreateKeyExW(
                HKEY_LOCAL_MACHINE,
                &parent,
                0,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE | KEY_WOW64_64KEY,
                None,
                &mut key,
                None,
            )
            .ok()
            .context("create CLSID key")?;
            let r = Self::set_sz(key, PCWSTR::null(), CLSID_DESCRIPTION);
            let _ = RegCloseKey(key);
            r?;

            let mut key = HKEY::default();
            RegCreateKeyExW(
                HKEY_LOCAL_MACHINE,
                &sub,
                0,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE | KEY_WOW64_64KEY,
                None,
                &mut key,
                None,
            )
            .ok()
            .context("create InprocServer32 key")?;
            let r = Self::set_sz(key, PCWSTR::null(), &dll.to_string_lossy())
                .and_then(|_| Self::set_sz(key, w!("ThreadingModel"), "Both"));
            let _ = RegCloseKey(key);
            r
        }
    }

    fn delete_registration(&self) -> Result<()> {
        let parent = HSTRING::from(clsid_key_path());
        let r = unsafe { RegDeleteTreeW(HKEY_LOCAL_MACHINE, &parent) };
        // ERROR_FILE_NOT_FOUND (2) means already gone.
        if r.is_err() && r.0 != 2 {
            bail!("RegDeleteTreeW failed: {}", windows::core::Error::from(r.to_hresult()));
        }
        Ok(())
    }
}

fn install_dir() -> Result<PathBuf> {
    let pf = std::env::var_os("ProgramW6432")
        .or_else(|| std::env::var_os("ProgramFiles"))
        .ok_or_else(|| anyhow!("%ProgramFiles% is not set"))?;
    Ok(PathBuf::from(pf).join(INSTALL_SUBDIR))
}

/// Copy `dll_src` to `%ProgramFiles%\Notideck\VirtualCamera` and register it under HKLM.
/// **Elevated callers only.** Returns the registered DLL path.
pub fn install(dll_src: &Path) -> Result<PathBuf> {
    registration::install_into(dll_src, &install_dir()?, &HklmRegistry)
}

/// Remove the HKLM registration and the installed files. **Elevated callers only.**
pub fn uninstall() -> Result<()> {
    registration::uninstall_from(&install_dir()?, &HklmRegistry)
}

/// Compare the installed registration with the DLL shipped next to the app. Read-only.
pub fn install_state(app_dll: &Path) -> InstallState {
    registration::compute_state(&HklmRegistry, app_dll)
}

/// Name of the DLL file as shipped and installed.
pub fn dll_file_name() -> &'static str {
    DLL_NAME
}

// ---------------------------------------------------------------------------------------------
// Virtual camera object
// ---------------------------------------------------------------------------------------------

/// The registered "Notideck Camera" (session lifetime, current user). Exists while this value
/// lives; dropping removes the camera. Requires the DLL to be installed (HKLM) first.
pub struct VirtualCamera {
    cam: IMFVirtualCamera,
}

impl VirtualCamera {
    pub fn create_and_start() -> Result<Self> {
        unsafe {
            // Already-initialised (any apartment) is fine.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, MFSTARTUP_FULL).context("MFStartup")?;
            let name = HSTRING::from(FRIENDLY_NAME);
            let clsid = HSTRING::from(CLSID_STR);
            let cam = MFCreateVirtualCamera(
                MFVirtualCameraType_SoftwareCameraSource,
                MFVirtualCameraLifetime_Session,
                MFVirtualCameraAccess_CurrentUser,
                &name,
                &clsid,
                None,
            )
            .context("MFCreateVirtualCamera")?;
            cam.Start(None).context("IMFVirtualCamera::Start")?;
            Ok(VirtualCamera { cam })
        }
    }
}

impl Drop for VirtualCamera {
    fn drop(&mut self) {
        unsafe {
            let _ = self.cam.Stop();
            let _ = self.cam.Remove();
            let _ = self.cam.Shutdown();
            let _ = MFShutdown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shm::{nv12_len, ReadResult};

    /// Creates the section as an ordinary user (Global\ is refused without a privilege, so this
    /// exercises the Local\ fallback; elevated it would land in Global\) and opens it again the
    /// way the app does. Touches no registry and no files.
    #[test]
    fn create_falls_back_and_open_round_trips() {
        std::env::remove_var(LOCAL_ONLY_ENV);
        let dll_side = ShmMapping::create().expect("create");
        eprintln!("section scope: {:?}, created: {}", dll_side.scope(), dll_side.created());
        assert!(dll_side.shm().is_valid());
        let app_side = ShmMapping::open().expect("open");
        assert_eq!(app_side.scope(), dll_side.scope());

        dll_side.shm().set_consumer(true, 640, 360, 30);
        assert_eq!(app_side.shm().wanted(), Some((640, 360, 30)));
        let frame = vec![5u8; nv12_len(640, 360)];
        assert!(app_side.shm().write_frame(640, 360, 7, &frame));
        let mut out = vec![0u8; frame.len()];
        assert!(matches!(dll_side.shm().read_frame(0, 640, 360, &mut out), ReadResult::Frame(_)));
        assert_eq!(out, frame);
        dll_side.shm().set_consumer(false, 640, 360, 30);
    }

    #[test]
    fn local_name_derivation() {
        assert_eq!(section_name(Scope::Local).to_string(), "Local\\NotideckVirtualCamera-v1");
        assert_eq!(section_name(Scope::Global).to_string(), SECTION_NAME);
    }

    #[test]
    fn sddl_parses() {
        let sddl = HSTRING::from(SECTION_SDDL);
        let mut sd = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(&sddl, SDDL_REVISION_1, &mut sd, None).unwrap();
            assert!(!sd.0.is_null());
            LocalFree(HLOCAL(sd.0));
        }
    }
}
