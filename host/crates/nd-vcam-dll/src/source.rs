// SPDX-License-Identifier: GPL-3.0-or-later
//! The media source. One object is both the `IMFActivate` the Frame Server creates from the
//! registered CLSID (VCamSample's `Activator`) and the `IMFMediaSourceEx` it activates
//! (`ActivateObject` hands out the same object), so tests and tools may also ask the class
//! factory for `IMFMediaSource` directly.

use std::sync::{Arc, Mutex, MutexGuard};

use nd_vcam::shm::CLSID_STR;
use windows::core::{implement, IUnknown, IUnknownImpl, Interface, Result, GUID, HRESULT};
use windows::Win32::Foundation::{E_INVALIDARG, E_POINTER, S_OK};
use windows::Win32::Media::KernelStreaming::{
    IKsControl, IKsControl_Impl, KSCAMERAPROFILE_HighFrameRate, KSCAMERAPROFILE_Legacy, KSIDENTIFIER,
};
use windows::Win32::Media::MediaFoundation::*;

use crate::attrs::forward_attributes;
use crate::stream::{err, guard, MediaStream, StreamShared, KS_NOT_FOUND};
use crate::vlog;

/// Parse `{E4DB89AD-622C-4168-8CD3-639FD6947B4A}`.
pub fn parse_guid(s: &str) -> Option<GUID> {
    let hex: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() != 32 {
        return None;
    }
    u128::from_str_radix(&hex, 16).ok().map(GUID::from_u128)
}

struct State {
    queue: Option<IMFMediaEventQueue>,
    descriptor: Option<IMFPresentationDescriptor>,
    stream: Option<IMFMediaStream2>,
}

#[implement(IMFMediaSourceEx, IMFActivate, IMFGetService, IKsControl, IMFSampleAllocatorControl)]
pub struct MediaSource {
    attrs: IMFAttributes,
    state: Mutex<State>,
    stream: Arc<StreamShared>,
}

impl Drop for MediaSource {
    fn drop(&mut self) {
        crate::exports::object_dropped();
    }
}

impl MediaSource {
    pub fn create() -> Result<IMFMediaSourceEx> {
        unsafe {
            let mut a = None;
            MFCreateAttributes(&mut a, 16)?;
            let attrs = a.ok_or_else(|| err(E_POINTER))?;
            attrs.SetUINT32(&MF_VIRTUALCAMERA_PROVIDE_ASSOCIATED_CAMERA_SOURCES, 1)?;
            if let Some(clsid) = parse_guid(CLSID_STR) {
                attrs.SetGUID(&MFT_TRANSFORM_CLSID_Attribute, &clsid)?;
            }
            if let Err(e) = set_sensor_profiles(&attrs) {
                // Not fatal for our fixed 30 fps stream; the Frame Server only uses it for profile selection.
                vlog!("sensor profile collection not set: {e}");
            }

            let stream = StreamShared::new(0)?;
            let stream_obj = MediaStream::create(stream.clone());
            let descriptor = MFCreatePresentationDescriptor(Some(&[Some(stream.descriptor()?)]))?;
            let queue = MFCreateEventQueue()?;

            crate::exports::object_created();
            let src: IMFMediaSourceEx = MediaSource {
                attrs,
                state: Mutex::new(State { queue: Some(queue), descriptor: Some(descriptor), stream: Some(stream_obj) }),
                stream: stream.clone(),
            }
            .into();
            stream.set_source(Some(src.cast::<IMFMediaSource>()?));
            Ok(src)
        }
    }
}

unsafe fn set_sensor_profiles(attrs: &IMFAttributes) -> Result<()> {
    let collection = MFCreateSensorProfileCollection()?;
    let legacy = MFCreateSensorProfile(&KSCAMERAPROFILE_Legacy, 0, windows::core::w!(""))?;
    legacy.AddProfileFilter(0, windows::core::w!("((RES==;FRT<=30,1;SUT==))"))?;
    collection.AddProfile(&legacy)?;
    let high = MFCreateSensorProfile(&KSCAMERAPROFILE_HighFrameRate, 0, windows::core::w!(""))?;
    high.AddProfileFilter(0, windows::core::w!("((RES==;FRT>=60,1;SUT==))"))?;
    collection.AddProfile(&high)?;
    attrs.SetUnknown(&MF_DEVICEMFT_SENSORPROFILE_COLLECTION, &collection)?;
    Ok(())
}

impl MediaSource_Impl {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn queue(&self) -> Result<IMFMediaEventQueue> {
        self.lock().queue.clone().ok_or_else(|| err(MF_E_SHUTDOWN))
    }
}

forward_attributes!(MediaSource_Impl);

impl IMFActivate_Impl for MediaSource_Impl {
    fn ActivateObject(&self, riid: *const GUID, ppv: *mut *mut core::ffi::c_void) -> Result<()> {
        guard(|| {
            if ppv.is_null() || riid.is_null() {
                return Err(err(E_POINTER));
            }
            unsafe { *ppv = std::ptr::null_mut() };
            vlog!("ActivateObject iid={:?}", unsafe { *riid });
            unsafe { self.QueryInterface(riid, ppv).ok() }
        })
    }
    fn ShutdownObject(&self) -> Result<()> {
        Ok(())
    }
    fn DetachObject(&self) -> Result<()> {
        Ok(())
    }
}

impl IMFMediaEventGenerator_Impl for MediaSource_Impl {
    fn GetEvent(&self, flags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS) -> Result<IMFMediaEvent> {
        let q = self.queue()?;
        unsafe { q.GetEvent(flags.0) }
    }
    fn BeginGetEvent(&self, cb: Option<&IMFAsyncCallback>, state: Option<&IUnknown>) -> Result<()> {
        let q = self.queue()?;
        unsafe { q.BeginGetEvent(cb, state) }
    }
    fn EndGetEvent(&self, result: Option<&IMFAsyncResult>) -> Result<IMFMediaEvent> {
        let q = self.queue()?;
        unsafe { q.EndGetEvent(result) }
    }
    fn QueueEvent(
        &self,
        met: u32,
        ext: *const GUID,
        hr: HRESULT,
        value: *const windows::core::PROPVARIANT,
    ) -> Result<()> {
        let q = self.queue()?;
        unsafe { q.QueueEventParamVar(met, ext, hr, value) }
    }
}

impl IMFMediaSource_Impl for MediaSource_Impl {
    fn GetCharacteristics(&self) -> Result<u32> {
        Ok(MFMEDIASOURCE_IS_LIVE.0 as u32)
    }

    fn CreatePresentationDescriptor(&self) -> Result<IMFPresentationDescriptor> {
        guard(|| {
            let d = self.lock().descriptor.clone().ok_or_else(|| err(MF_E_SHUTDOWN))?;
            unsafe { d.Clone() }
        })
    }

    fn Start(
        &self,
        pd: Option<&IMFPresentationDescriptor>,
        time_format: *const GUID,
        _start: *const windows::core::PROPVARIANT,
    ) -> Result<()> {
        guard(|| {
            let pd = pd.ok_or_else(|| err(E_POINTER))?;
            if !time_format.is_null() && unsafe { *time_format } != GUID::zeroed() {
                return Err(err(E_INVALIDARG));
            }
            let (queue, own_pd, stream_obj) = {
                let g = self.lock();
                (
                    g.queue.clone().ok_or_else(|| err(MF_E_SHUTDOWN))?,
                    g.descriptor.clone().ok_or_else(|| err(MF_E_SHUTDOWN))?,
                    g.stream.clone().ok_or_else(|| err(MF_E_SHUTDOWN))?,
                )
            };
            unsafe {
                let count = pd.GetStreamDescriptorCount()?;
                if count != 1 {
                    return Err(err(E_INVALIDARG));
                }
                let mut selected = windows::Win32::Foundation::BOOL(0);
                let mut desc = None;
                pd.GetStreamDescriptorByIndex(0, &mut selected, &mut desc)?;
                let desc = desc.ok_or_else(|| err(E_POINTER))?;
                if desc.GetStreamIdentifier()? != self.stream.id() {
                    return Err(err(E_INVALIDARG));
                }
                let now = windows::core::PROPVARIANT::from(MFGetSystemTime());

                let was_running = self.stream.state() != MF_STREAM_STATE_STOPPED;
                if selected.as_bool() {
                    own_pd.SelectStream(0)?;
                    let event = if was_running { MEUpdatedStream } else { MENewStream };
                    queue.QueueEventParamUnk(event.0 as u32, &GUID::zeroed(), S_OK, &stream_obj)?;
                    let ty = desc.GetMediaTypeHandler()?.GetCurrentMediaType().ok();
                    self.stream.start(ty.as_ref())?;
                } else {
                    own_pd.DeselectStream(0)?;
                    self.stream.stop()?;
                }
                let event = if was_running { MESourceSeeked } else { MESourceStarted };
                queue.QueueEventParamVar(event.0 as u32, &GUID::zeroed(), S_OK, &now)?;
            }
            Ok(())
        })
    }

    fn Stop(&self) -> Result<()> {
        guard(|| {
            let (queue, pd) = {
                let g = self.lock();
                (
                    g.queue.clone().ok_or_else(|| err(MF_E_SHUTDOWN))?,
                    g.descriptor.clone().ok_or_else(|| err(MF_E_SHUTDOWN))?,
                )
            };
            self.stream.stop()?;
            unsafe {
                pd.DeselectStream(0)?;
                let now = windows::core::PROPVARIANT::from(MFGetSystemTime());
                queue.QueueEventParamVar(MESourceStopped.0 as u32, &GUID::zeroed(), S_OK, &now)?;
            }
            Ok(())
        })
    }

    fn Pause(&self) -> Result<()> {
        Err(err(MF_E_INVALID_STATE_TRANSITION))
    }

    fn Shutdown(&self) -> Result<()> {
        guard(|| {
            let (queue, stream_obj) = {
                let mut g = self.lock();
                let q = g.queue.take().ok_or_else(|| err(MF_E_SHUTDOWN))?;
                g.descriptor = None;
                (q, g.stream.take())
            };
            unsafe { queue.Shutdown().ok() };
            self.stream.shutdown(); // also breaks the stream -> source reference cycle
            drop(stream_obj);
            vlog!("source shut down");
            Ok(())
        })
    }
}

impl IMFMediaSourceEx_Impl for MediaSource_Impl {
    fn GetSourceAttributes(&self) -> Result<IMFAttributes> {
        Ok(self.attrs.clone())
    }
    fn GetStreamAttributes(&self, id: u32) -> Result<IMFAttributes> {
        if id != self.stream.id() {
            return Err(err(E_INVALIDARG));
        }
        Ok(self.stream.attrs.clone())
    }
    fn SetD3DManager(&self, _manager: Option<&IUnknown>) -> Result<()> {
        // System-memory samples only; accepting the manager keeps the Frame Server happy.
        Ok(())
    }
}

impl IMFGetService_Impl for MediaSource_Impl {
    fn GetService(&self, _service: *const GUID, _riid: *const GUID, _ppv: *mut *mut core::ffi::c_void) -> Result<()> {
        Err(err(MF_E_UNSUPPORTED_SERVICE))
    }
}

impl IMFSampleAllocatorControl_Impl for MediaSource_Impl {
    fn SetDefaultAllocator(&self, id: u32, allocator: Option<&IUnknown>) -> Result<()> {
        guard(|| {
            if id != self.stream.id() {
                return Err(err(E_INVALIDARG));
            }
            self.stream.set_allocator(allocator.ok_or_else(|| err(E_POINTER))?)
        })
    }
    fn GetAllocatorUsage(&self, id: u32, input_id: *mut u32, usage: *mut MFSampleAllocatorUsage) -> Result<()> {
        if input_id.is_null() || usage.is_null() {
            return Err(err(E_POINTER));
        }
        if id != self.stream.id() {
            return Err(err(E_INVALIDARG));
        }
        unsafe {
            *input_id = id;
            *usage = MFSampleAllocatorUsage_UsesProvidedAllocator;
        }
        Ok(())
    }
}

impl IKsControl_Impl for MediaSource_Impl {
    fn KsProperty(
        &self,
        _p: *const KSIDENTIFIER,
        _l: u32,
        _d: *mut core::ffi::c_void,
        _dl: u32,
        ret: *mut u32,
    ) -> Result<()> {
        if ret.is_null() {
            return Err(err(E_POINTER));
        }
        Err(err(KS_NOT_FOUND))
    }
    fn KsMethod(
        &self,
        _p: *const KSIDENTIFIER,
        _l: u32,
        _d: *mut core::ffi::c_void,
        _dl: u32,
        ret: *mut u32,
    ) -> Result<()> {
        if ret.is_null() {
            return Err(err(E_POINTER));
        }
        Err(err(KS_NOT_FOUND))
    }
    fn KsEvent(
        &self,
        _p: *const KSIDENTIFIER,
        _l: u32,
        _d: *mut core::ffi::c_void,
        _dl: u32,
        ret: *mut u32,
    ) -> Result<()> {
        if ret.is_null() {
            return Err(err(E_POINTER));
        }
        Err(err(KS_NOT_FOUND))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clsid_parses() {
        let g = parse_guid(CLSID_STR).unwrap();
        assert_eq!(g.to_u128(), 0xE4DB89AD_622C_4168_8CD3_639FD6947B4A);
    }
}
