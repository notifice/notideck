// SPDX-License-Identifier: GPL-3.0-or-later
//! DLL entry points and the class factory.

use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicIsize, Ordering};

use nd_vcam::shm::CLSID_STR;
use windows::core::{implement, IUnknown, Interface, Result, GUID, HRESULT};
use windows::Win32::Foundation::{
    BOOL, CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_INVALIDARG, E_POINTER, E_UNEXPECTED, S_FALSE, S_OK,
};
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};

use crate::source::{parse_guid, MediaSource};
use crate::stream::err;
use crate::vlog;

static OBJECTS: AtomicIsize = AtomicIsize::new(0);
static LOCKS: AtomicIsize = AtomicIsize::new(0);

pub(crate) fn object_created() {
    OBJECTS.fetch_add(1, Ordering::SeqCst);
}

pub(crate) fn object_dropped() {
    OBJECTS.fetch_sub(1, Ordering::SeqCst);
}

#[implement(IClassFactory)]
struct Factory;

impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(&self, outer: Option<&IUnknown>, riid: *const GUID, ppv: *mut *mut c_void) -> Result<()> {
        crate::stream::guard(|| {
            if ppv.is_null() || riid.is_null() {
                return Err(err(E_POINTER));
            }
            unsafe { *ppv = std::ptr::null_mut() };
            if outer.is_some() {
                return Err(err(CLASS_E_NOAGGREGATION));
            }
            let src = MediaSource::create().inspect_err(|e| vlog!("MediaSource::create failed: {e}"))?;
            unsafe { src.query(riid, ppv).ok() }
        })
    }

    fn LockServer(&self, lock: BOOL) -> Result<()> {
        if lock.as_bool() {
            LOCKS.fetch_add(1, Ordering::SeqCst);
        } else {
            LOCKS.fetch_sub(1, Ordering::SeqCst);
        }
        Ok(())
    }
}

/// # Safety
/// Called by COM with valid pointers (checked for null).
#[no_mangle]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    catch_unwind(AssertUnwindSafe(|| {
        if rclsid.is_null() || riid.is_null() || ppv.is_null() {
            return E_POINTER;
        }
        *ppv = std::ptr::null_mut();
        if Some(*rclsid) != parse_guid(CLSID_STR) {
            return CLASS_E_CLASSNOTAVAILABLE;
        }
        vlog!("DllGetClassObject iid={:?}", *riid);
        let factory: IClassFactory = Factory.into();
        factory.query(riid, ppv)
    }))
    .unwrap_or(E_UNEXPECTED)
}

#[no_mangle]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    if OBJECTS.load(Ordering::SeqCst) <= 0 && LOCKS.load(Ordering::SeqCst) <= 0 {
        S_OK
    } else {
        S_FALSE
    }
}

#[allow(dead_code)]
const _: HRESULT = E_INVALIDARG;
