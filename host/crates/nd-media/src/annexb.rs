// SPDX-License-Identifier: GPL-3.0-or-later
//! Minimal Annex-B helpers.

/// Iterate the NAL units (without start codes, non-empty) of an Annex-B byte stream.
pub fn nal_units(data: &[u8]) -> impl Iterator<Item = &[u8]> {
    // Positions of the first byte after each 00 00 01 start code.
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let n = starts.len();
    (0..n).filter_map(move |k| {
        let s = starts[k];
        let mut e = if k + 1 < n { starts[k + 1] - 3 } else { data.len() };
        while e > s && data[e - 1] == 0 {
            e -= 1; // trailing zero / 4-byte start code prefix
        }
        (e > s).then(|| &data[s..e])
    })
}

pub fn contains_nal_type(data: &[u8], ty: u8) -> bool {
    nal_units(data).any(|n| n[0] & 0x1f == ty)
}
