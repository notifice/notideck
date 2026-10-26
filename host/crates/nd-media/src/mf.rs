// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows Media Foundation H.264 decoder (the OS's `CMSH264DecoderMFT`, software path with
//! optional OS-internal acceleration). Input: Annex-B; output: NV12, cropped and tightly packed.

use crate::{contains_nal_type, validate_annexb, H264Decoder, MediaError, MediaPacket, Nv12Frame};
use std::sync::OnceLock;
use windows::core::Interface;
use windows::core::VARIANT;
use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};

fn mf_startup() -> Result<(), MediaError> {
    static INIT: OnceLock<Result<(), String>> = OnceLock::new();
    INIT.get_or_init(|| unsafe {
        let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
        if hr.is_err() && hr != RPC_E_CHANGED_MODE {
            return Err(format!("CoInitializeEx: {hr:?}"));
        }
        // Process-lifetime startup, never shut down (the decoder may outlive any owner).
        MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(|e| format!("MFStartup: {e}"))
    })
    .clone()
    .map_err(|e| MediaError::Unavailable(format!("Media Foundation: {e}")))
}

fn dec_err(what: &str, e: windows::core::Error) -> MediaError {
    MediaError::Decode(format!("{what}: {e}"))
}

struct OutFormat {
    width: u32,
    height: u32,
    /// Row pitch of the Y plane in bytes.
    stride: usize,
    /// Rows of the Y plane in the buffer (UV starts after this many rows).
    buf_rows: usize,
    off_x: u32,
    off_y: u32,
}

pub struct MfDecoder {
    mft: IMFTransform,
    fmt: Option<OutFormat>,
    provides_samples: bool,
    out_size: u32,
    config: Vec<u8>,
}

// SAFETY: the transform is only ever used through `&mut self`, and MF MFTs created in an MTA are
// free-threaded for sequential calls from different threads.
unsafe impl Send for MfDecoder {}

fn create_mft() -> Result<IMFTransform, MediaError> {
    mf_startup()?;
    unsafe {
        let mft: IMFTransform = CoCreateInstance(&CLSID_MSH264DecoderMFT, None, CLSCTX_INPROC_SERVER).map_err(|e| {
            MediaError::Unavailable(format!(
                "Windows H.264 decoder (CMSH264DecoderMFT) could not be created: {e}. On Windows N/KN \
                 editions install the \"Media Feature Pack\" from Microsoft."
            ))
        })?;
        // Low latency: no frame reordering delay. Applied before the input type / first input;
        // failures are logged because a silently missing flag costs frames of latency.
        match mft.cast::<ICodecAPI>() {
            Ok(api) => {
                if let Err(e) = api.SetValue(&CODECAPI_AVLowLatencyMode, &VARIANT::from(true)) {
                    tracing::warn!("H.264 MFT: AVLowLatencyMode not applied: {e}");
                } else if api.GetValue(&CODECAPI_AVLowLatencyMode).ok().and_then(|v| bool::try_from(&v).ok())
                    != Some(true)
                {
                    tracing::warn!("H.264 MFT: AVLowLatencyMode did not read back as enabled");
                }
            }
            Err(e) => tracing::warn!("H.264 MFT has no ICodecAPI ({e}); cannot request low latency"),
        }
        match mft.GetAttributes() {
            Ok(attrs) => {
                if let Err(e) = attrs.SetUINT32(&MF_LOW_LATENCY, 1) {
                    tracing::warn!("H.264 MFT: MF_LOW_LATENCY not applied: {e}");
                }
            }
            Err(e) => tracing::debug!("H.264 MFT has no attributes ({e})"),
        }
        let it = MFCreateMediaType().map_err(|e| dec_err("MFCreateMediaType", e))?;
        it.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(|e| dec_err("major", e))?;
        it.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264).map_err(|e| dec_err("subtype", e))?;
        it.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_MixedInterlaceOrProgressive.0 as u32)
            .map_err(|e| dec_err("interlace", e))?;
        mft.SetInputType(0, &it, 0)
            .map_err(|e| MediaError::Unavailable(format!("H.264 MFT rejected the input type: {e}")))?;
        Ok(mft)
    }
}

impl MfDecoder {
    pub fn new() -> Result<Self, MediaError> {
        let mut d = Self { mft: create_mft()?, fmt: None, provides_samples: false, out_size: 0, config: vec![] };
        d.select_output_type()?;
        d.start()?;
        Ok(d)
    }

    fn start(&self) -> Result<(), MediaError> {
        unsafe {
            self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0).map_err(|e| dec_err("begin", e))?;
            self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0).map_err(|e| dec_err("start", e))
        }
    }

    /// Pick NV12 as output type and cache geometry. Used at start and on STREAM_CHANGE.
    fn select_output_type(&mut self) -> Result<(), MediaError> {
        unsafe {
            let mut i = 0;
            let ty = loop {
                let t = self
                    .mft
                    .GetOutputAvailableType(0, i)
                    .map_err(|_| MediaError::Unavailable("H.264 MFT offers no NV12 output".into()))?;
                if t.GetGUID(&MF_MT_SUBTYPE).ok() == Some(MFVideoFormat_NV12) {
                    break t;
                }
                i += 1;
            };
            self.mft.SetOutputType(0, &ty, 0).map_err(|e| dec_err("SetOutputType", e))?;
            let cur = self.mft.GetOutputCurrentType(0).map_err(|e| dec_err("GetOutputCurrentType", e))?;
            let packed = cur.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or(0);
            let (fw, fh) = ((packed >> 32) as u32, packed as u32);
            let (mut width, mut height, mut off_x, mut off_y) = (fw, fh, 0u32, 0u32);
            let mut area = MFVideoArea::default();
            if cur
                .GetBlob(
                    &MF_MT_MINIMUM_DISPLAY_APERTURE,
                    std::slice::from_raw_parts_mut(&mut area as *mut _ as *mut u8, std::mem::size_of::<MFVideoArea>()),
                    None,
                )
                .is_ok()
                && area.Area.cx > 0
                && area.Area.cy > 0
            {
                width = area.Area.cx as u32;
                height = area.Area.cy as u32;
                off_x = area.OffsetX.value.max(0) as u32;
                off_y = area.OffsetY.value.max(0) as u32;
            }
            let stride = cur.GetUINT32(&MF_MT_DEFAULT_STRIDE).map(|s| s as i32).unwrap_or(fw as i32);
            let stride = if stride > 0 { stride as usize } else { fw as usize };
            let info = self.mft.GetOutputStreamInfo(0).map_err(|e| dec_err("GetOutputStreamInfo", e))?;
            self.provides_samples = info.dwFlags
                & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) as u32
                != 0;
            self.out_size = info.cbSize;
            self.fmt = Some(OutFormat { width, height, stride, buf_rows: fh as usize, off_x, off_y });
            Ok(())
        }
    }

    fn make_input(&self, data: &[u8], pts_us: u64) -> Result<IMFSample, MediaError> {
        unsafe {
            let buf = MFCreateMemoryBuffer(data.len() as u32).map_err(|e| dec_err("MFCreateMemoryBuffer", e))?;
            let mut p = std::ptr::null_mut();
            buf.Lock(&mut p, None, None).map_err(|e| dec_err("Lock", e))?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), p, data.len());
            buf.Unlock().map_err(|e| dec_err("Unlock", e))?;
            buf.SetCurrentLength(data.len() as u32).map_err(|e| dec_err("SetCurrentLength", e))?;
            let s = MFCreateSample().map_err(|e| dec_err("MFCreateSample", e))?;
            s.AddBuffer(&buf).map_err(|e| dec_err("AddBuffer", e))?;
            s.SetSampleTime((pts_us * 10) as i64).map_err(|e| dec_err("SetSampleTime", e))?;
            Ok(s)
        }
    }

    /// Pull every frame currently available.
    fn drain(&mut self, out: &mut Vec<Nv12Frame>) -> Result<(), MediaError> {
        loop {
            let own = if self.provides_samples {
                None
            } else {
                unsafe {
                    let buf = MFCreateAlignedMemoryBuffer(self.out_size.max(1), 63)
                        .map_err(|e| dec_err("MFCreateAlignedMemoryBuffer", e))?;
                    let s = MFCreateSample().map_err(|e| dec_err("MFCreateSample", e))?;
                    s.AddBuffer(&buf).map_err(|e| dec_err("AddBuffer", e))?;
                    Some(s)
                }
            };
            let mut ob = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: std::mem::ManuallyDrop::new(own),
                dwStatus: 0,
                pEvents: std::mem::ManuallyDrop::new(None),
            }];
            let mut status = 0u32;
            let r = unsafe { self.mft.ProcessOutput(0, &mut ob, &mut status) };
            let sample = unsafe { std::mem::ManuallyDrop::take(&mut ob[0].pSample) };
            unsafe { std::mem::ManuallyDrop::drop(&mut ob[0].pEvents) };
            match r {
                Ok(()) => {
                    if let Some(s) = sample {
                        if let Some(f) = self.read_sample(&s)? {
                            out.push(f);
                        }
                    }
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => self.select_output_type()?,
                Err(e) => return Err(dec_err("ProcessOutput", e)),
            }
        }
    }

    fn read_sample(&self, s: &IMFSample) -> Result<Option<Nv12Frame>, MediaError> {
        let f = self.fmt.as_ref().ok_or_else(|| MediaError::Decode("no output format".into()))?;
        unsafe {
            let pts_us = (s.GetSampleTime().unwrap_or(0).max(0) as u64) / 10;
            let buf = s.ConvertToContiguousBuffer().map_err(|e| dec_err("ConvertToContiguousBuffer", e))?;
            let (w, h) = ((f.width & !1) as usize, (f.height & !1) as usize);
            let (ox, oy) = ((f.off_x & !1) as usize, (f.off_y & !1) as usize);
            // Prefer the 2D lock: it reports the real pitch.
            let mut base: *mut u8 = std::ptr::null_mut();
            let mut pitch = f.stride;
            let two_d = buf.cast::<IMF2DBuffer>().ok();
            let mut locked_2d = false;
            if let Some(b2) = &two_d {
                let mut p = 0i32;
                if b2.Lock2D(&mut base, &mut p).is_ok() {
                    locked_2d = true;
                    if p > 0 {
                        pitch = p as usize;
                    }
                }
            }
            let mut len = 0u32;
            if !locked_2d {
                buf.Lock(&mut base, None, Some(&mut len)).map_err(|e| dec_err("Lock", e))?;
            }
            let rows = f.buf_rows.max(h + oy);
            let needed = pitch * rows + pitch * (rows / 2);
            let ok = locked_2d || len as usize >= needed.min(pitch * (h + oy) * 3 / 2);
            let res = if ok && !base.is_null() {
                let src = std::slice::from_raw_parts(base, if locked_2d { needed } else { len as usize });
                let mut data = Vec::with_capacity(w * h * 3 / 2);
                for r in 0..h {
                    let o = (oy + r) * pitch + ox;
                    data.extend_from_slice(&src[o..o + w]);
                }
                let uv = pitch * rows;
                for r in 0..h / 2 {
                    let o = uv + (oy / 2 + r) * pitch + ox;
                    data.extend_from_slice(&src[o..o + w]);
                }
                Some(Nv12Frame { width: w as u32, height: h as u32, pts_us, data })
            } else {
                None
            };
            if locked_2d {
                let _ = two_d.unwrap().Unlock2D();
            } else {
                let _ = buf.Unlock();
            }
            res.map(Some).ok_or_else(|| MediaError::Decode("short NV12 output buffer".into()))
        }
    }
}

impl H264Decoder for MfDecoder {
    fn decode(&mut self, packet: &MediaPacket) -> Result<Vec<Nv12Frame>, MediaError> {
        validate_annexb(&packet.data)?;
        let mut data: &[u8] = &packet.data;
        let joined;
        if packet.header.config {
            self.config = packet.data.to_vec();
        } else if packet.header.key_frame && !self.config.is_empty() && !contains_nal_type(data, 7) {
            joined = [self.config.as_slice(), data].concat();
            data = &joined;
        }
        let mut out = Vec::new();
        let sample = self.make_input(data, packet.header.pts_us)?;
        let mut r = unsafe { self.mft.ProcessInput(0, &sample, 0) };
        if matches!(&r, Err(e) if e.code() == MF_E_NOTACCEPTING) {
            self.drain(&mut out)?;
            r = unsafe { self.mft.ProcessInput(0, &sample, 0) };
        }
        r.map_err(|e| dec_err("ProcessInput", e))?;
        self.drain(&mut out)?;
        Ok(out)
    }

    fn reset(&mut self) {
        // Recreating is the most reliable way to drop corrupt reference state and SPS/PPS.
        if let Ok(m) = create_mft() {
            self.mft = m;
            self.fmt = None;
            self.config.clear();
            if self.select_output_type().is_ok() {
                let _ = self.start();
            }
        }
    }

    fn name(&self) -> &'static str {
        "mf"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn low_latency_mode_is_applied() {
        let mft = create_mft().expect("mft");
        let api = mft.cast::<ICodecAPI>().expect("ICodecAPI");
        let v = unsafe { api.GetValue(&CODECAPI_AVLowLatencyMode) }.expect("readback");
        assert_eq!(bool::try_from(&v).ok(), Some(true), "low latency mode not enabled");
    }
}
