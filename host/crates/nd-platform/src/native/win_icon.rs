// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared Windows shell-icon helper: `IShellItemImageFactory` -> 64x64 PNG.

use std::ffi::c_void;

use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{
    DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS, HBITMAP,
};
use windows::Win32::UI::Shell::{IShellItemImageFactory, SIIGBF_ICONONLY};

struct Bitmap(HBITMAP);
impl Drop for Bitmap {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.0);
        }
    }
}

/// Render the item's icon (64x64) and encode it as an RGBA PNG.
pub(crate) fn factory_icon_png(f: &IShellItemImageFactory) -> std::result::Result<Option<Vec<u8>>, String> {
    let hbm =
        Bitmap(unsafe { f.GetImage(SIZE { cx: 64, cy: 64 }, SIIGBF_ICONONLY) }.map_err(|e| format!("GetImage: {e}"))?);
    unsafe {
        let mut bm = BITMAP::default();
        if GetObjectW(hbm.0, std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut c_void)) == 0 {
            return Err("GetObjectW failed".into());
        }
        let (w, h) = (bm.bmWidth, bm.bmHeight.abs());
        if w <= 0 || h <= 0 {
            return Ok(None);
        }
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut px = vec![0u8; (w * h * 4) as usize];
        let dc = GetDC(None);
        let got = GetDIBits(dc, hbm.0, 0, h as u32, Some(px.as_mut_ptr() as *mut c_void), &mut info, DIB_RGB_COLORS);
        ReleaseDC(None, dc);
        if got == 0 {
            return Err("GetDIBits failed".into());
        }
        // BGRA premultiplied -> RGBA straight.
        for c in px.as_chunks_mut::<4>().0 {
            let a = c[3] as u32;
            let (b, g, r) = (c[0] as u32, c[1] as u32, c[2] as u32);
            let un = |v: u32| if a == 0 || a == 255 { v as u8 } else { ((v * 255 + a / 2) / a).min(255) as u8 };
            c[0] = un(r);
            c[1] = un(g);
            c[2] = un(b);
        }
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, w as u32, h as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut wr = enc.write_header().map_err(|e| e.to_string())?;
        wr.write_image_data(&px).map_err(|e| e.to_string())?;
        wr.finish().map_err(|e| e.to_string())?;
        Ok(Some(out))
    }
}
