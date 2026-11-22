//! H.264 sequence parameter set (ITU-T H.264 section 7.3.2.1.1): enough to size the decoder
//! from the stream. Hardware run 3 showed why the M4 pick is not enough: the TB336FU
//! negotiated level 3.2 in M4 but sent an SPS with level 4.0.
//!
//! Hostile input (AGENTS.md 1.9): every read is bounded and checked; a bad SPS is `None`.

/// The fields of an SPS the sink needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sps {
    /// `profile_idc` (66 Baseline, 77 Main, 100 High, ...).
    pub profile_idc: u8,
    /// `constraint_set0..5_flag` byte as it is in the SPS.
    pub constraint_flags: u8,
    /// `level_idc`: ten times the level (40 = 4.0); see [`Sps::level`].
    pub level_idc: u8,
    /// Display width in pixels after cropping.
    pub width: u32,
    /// Display height in pixels after cropping.
    pub height: u32,
}

impl Sps {
    /// Level as `(major, minor)` = `level_idc / 10, level_idc % 10`, e.g. `(4, 0)`. Level 1b
    /// is not special-cased (irrelevant for Miracast's 3.1+ levels).
    pub fn level(&self) -> (u8, u8) {
        (self.level_idc / 10, self.level_idc % 10)
    }

    /// Parses an SPS NAL unit (starting with the NAL header byte, type 7, no start code).
    pub fn parse(nal: &[u8]) -> Option<Sps> {
        let (&header, payload) = nal.split_first()?;
        if header & 0x1F != 7 {
            return None;
        }
        // Only the header part is parsed; 256 bytes covers it even with scaling lists.
        let rbsp = unescape(payload.get(..payload.len().min(256))?);
        parse_rbsp(&rbsp)
    }
}

/// The first SPS in an Annex B access unit, if any.
pub fn find_sps(annexb: &[u8]) -> Option<Sps> {
    nal_units(annexb)
        .filter(|n| n.first().is_some_and(|h| h & 0x1F == 7))
        .find_map(Sps::parse)
}

/// NAL units of an Annex B byte stream (start codes `00 00 01` or `00 00 00 01` removed).
pub fn nal_units(data: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = data;
    std::iter::from_fn(move || {
        let start = find_start_code(rest)?;
        let body = rest.get(start..)?;
        let end = find_start_code_pos(body).unwrap_or(body.len());
        let nal = body.get(..end)?;
        rest = body.get(end..)?;
        // Trailing zero bytes belong to the next 4-byte start code.
        let trimmed = nal.len() - nal.iter().rev().take_while(|&&b| b == 0).count();
        Some(nal.get(..trimmed).unwrap_or(nal))
    })
    .filter(|n| !n.is_empty())
}

/// Position of the next `00 00 01`.
fn find_start_code_pos(d: &[u8]) -> Option<usize> {
    d.windows(3).position(|w| w == [0, 0, 1])
}

/// Index just past the next start code.
fn find_start_code(d: &[u8]) -> Option<usize> {
    find_start_code_pos(d).map(|p| p + 3)
}

/// Removes emulation prevention bytes (`00 00 03` -> `00 00`).
fn unescape(d: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(d.len());
    let mut zeros = 0;
    for &b in d {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn bit(&mut self) -> Option<u32> {
        let byte = self.data.get(self.pos / 8)?;
        let b = (byte >> (7 - (self.pos % 8))) & 1;
        self.pos += 1;
        Some(u32::from(b))
    }

    fn bits(&mut self, n: u32) -> Option<u32> {
        (0..n).try_fold(0u32, |acc, _| Some((acc << 1) | self.bit()?))
    }

    fn flag(&mut self) -> Option<bool> {
        Some(self.bit()? == 1)
    }

    /// `ue(v)`; values above 2^31 are refused.
    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        let rest = self.bits(zeros)?;
        (1u32 << zeros).checked_sub(1)?.checked_add(rest)
    }

    /// `se(v)`.
    fn se(&mut self) -> Option<i32> {
        let k = self.ue()?;
        let mag = i32::try_from(k.div_ceil(2)).ok()?;
        Some(if k % 2 == 1 { mag } else { -mag })
    }
}

/// Profiles whose SPS carries chroma format, bit depth and scaling lists (7.3.2.1.1).
fn has_chroma_info(profile: u8) -> bool {
    matches!(
        profile,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    )
}

fn skip_scaling_list(r: &mut Bits<'_>, size: u32) -> Option<()> {
    let (mut last, mut next) = (8i32, 8i32);
    for _ in 0..size {
        if next != 0 {
            let delta = r.se()?;
            if !(-128..=127).contains(&delta) {
                return None;
            }
            next = (last + delta + 256).rem_euclid(256);
        }
        if next != 0 {
            last = next;
        }
    }
    Some(())
}

/// Largest picture dimension accepted (H.264 level 6.2 tops out at 8192x4320).
const MAX_DIM: u32 = 16_384;

fn parse_rbsp(d: &[u8]) -> Option<Sps> {
    let mut r = Bits { data: d, pos: 0 };
    let profile_idc = r.bits(8)? as u8;
    let constraint_flags = r.bits(8)? as u8;
    let level_idc = r.bits(8)? as u8;
    let _sps_id = r.ue()?;
    let mut chroma_format_idc = 1;
    let mut separate_colour_plane = false;
    if has_chroma_info(profile_idc) {
        chroma_format_idc = r.ue()?;
        if chroma_format_idc > 3 {
            return None;
        }
        if chroma_format_idc == 3 {
            separate_colour_plane = r.flag()?;
        }
        let _bit_depth_luma = r.ue()?;
        let _bit_depth_chroma = r.ue()?;
        let _transform_bypass = r.flag()?;
        if r.flag()? {
            let lists = if chroma_format_idc == 3 { 12 } else { 8 };
            for i in 0..lists {
                if r.flag()? {
                    skip_scaling_list(&mut r, if i < 6 { 16 } else { 64 })?;
                }
            }
        }
    }
    let _log2_max_frame_num = r.ue()?;
    match r.ue()? {
        0 => {
            let _log2_max_poc_lsb = r.ue()?;
        }
        1 => {
            let _delta_pic_order_always_zero = r.flag()?;
            let _offset_for_non_ref_pic = r.se()?;
            let _offset_for_top_to_bottom = r.se()?;
            let n = r.ue()?;
            if n > 255 {
                return None;
            }
            for _ in 0..n {
                r.se()?;
            }
        }
        2 => {}
        _ => return None,
    }
    let _max_num_ref_frames = r.ue()?;
    let _gaps_allowed = r.flag()?;
    let width_mbs = r.ue()?.checked_add(1)?;
    let height_map_units = r.ue()?.checked_add(1)?;
    let frame_mbs_only = r.flag()?;
    if !frame_mbs_only {
        let _mb_adaptive_frame_field = r.flag()?;
    }
    let _direct_8x8_inference = r.flag()?;
    let crop = if r.flag()? {
        [r.ue()?, r.ue()?, r.ue()?, r.ue()?]
    } else {
        [0; 4]
    };

    let field_factor = if frame_mbs_only { 1 } else { 2 };
    let chroma_array_type = if separate_colour_plane {
        0
    } else {
        chroma_format_idc
    };
    let (crop_x, crop_y) = match chroma_array_type {
        1 => (2, 2 * field_factor),
        2 => (2, field_factor),
        _ => (1, field_factor),
    };
    let full_w = width_mbs.checked_mul(16)?;
    let full_h = height_map_units
        .checked_mul(16)?
        .checked_mul(field_factor)?;
    if full_w > MAX_DIM || full_h > MAX_DIM {
        return None;
    }
    let [left, right, top, bottom] = crop;
    let width = full_w.checked_sub(left.checked_add(right)?.checked_mul(crop_x)?)?;
    let height = full_h.checked_sub(top.checked_add(bottom)?.checked_mul(crop_y)?)?;
    if width == 0 || height == 0 {
        return None;
    }
    Some(Sps {
        profile_idc,
        constraint_flags,
        level_idc,
        width,
        height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// First SPS of `stream-run3.h264` (TB336FU, hardware run 3): High, level 4.0, 1080p
    /// (1088 coded lines cropped to 1080).
    const TB336FU_SPS: [u8; 20] = [
        0x67, 0x64, 0x00, 0x28, 0xac, 0x1a, 0xd0, 0x0f, 0x00, 0x44, 0xfc, 0xb3, 0x50, 0x20, 0x20,
        0x20, 0x78, 0x44, 0x23, 0x50,
    ];

    #[test]
    fn tablet_sps_is_high_4_0_1080p() {
        let s = Sps::parse(&TB336FU_SPS).unwrap();
        assert_eq!(s.profile_idc, 100);
        assert_eq!(s.level_idc, 40);
        assert_eq!(s.level(), (4, 0));
        assert_eq!((s.width, s.height), (1920, 1080));
    }

    #[test]
    fn found_in_an_access_unit() {
        let mut au = vec![0, 0, 0, 1, 0x09, 0xF0, 0, 0, 0, 1];
        au.extend_from_slice(&TB336FU_SPS);
        au.extend_from_slice(&[0, 0, 1, 0x68, 0xea, 0x43, 0xcb, 0, 0, 1, 0x65, 0x88]);
        let s = find_sps(&au).unwrap();
        assert_eq!((s.width, s.height), (1920, 1080));
        assert_eq!(nal_units(&au).count(), 4);
        assert!(find_sps(&[0, 0, 1, 0x65, 1, 2, 3]).is_none());
    }

    #[test]
    fn baseline_640x480_without_cropping() {
        // Hand-built from 7.3.2.1.1: profile 66, level 30, sps_id 0, log2_max_frame_num-4 0,
        // poc type 2, 1 ref frame, gaps 0, width 40 MBs, height 30 MBs, frame_mbs_only,
        // direct_8x8 1, no cropping, no VUI, stop bit.
        // Bits: 1 | 1 | 011 | 010 | 0 | 00000101000 | 000011110 | 1 | 1 | 0 | 0 | 1
        let sps = [0x67, 66, 0xC0, 30, 0xDA, 0x02, 0x80, 0xF6, 0x40];
        let s = Sps::parse(&sps).unwrap();
        assert_eq!((s.profile_idc, s.level()), (66, (3, 0)));
        assert_eq!((s.width, s.height), (640, 480));
    }

    #[test]
    fn emulation_prevention_is_removed() {
        assert_eq!(
            unescape(&[0, 0, 3, 1, 0, 0, 3, 0, 3]),
            [0, 0, 1, 0, 0, 0, 3]
        );
    }

    #[test]
    fn hostile_sps_is_none_not_a_panic() {
        assert!(Sps::parse(&[]).is_none());
        assert!(Sps::parse(&[0x68, 1, 2, 3]).is_none()); // a PPS
        assert!(Sps::parse(&[0x67, 100, 0, 40]).is_none()); // truncated
        assert!(Sps::parse(&[0x67, 66, 0, 30, 0, 0, 0, 0, 0, 0]).is_none()); // ue overflow
        assert!(Sps::parse(&[0x67, 66, 0, 30, 0xFF]).is_none());
        for cut in 0..TB336FU_SPS.len() - 4 {
            let _ = Sps::parse(&TB336FU_SPS[..cut]);
        }
        // Every single-byte corruption parses or fails, never panics.
        for i in 1..TB336FU_SPS.len() {
            for v in [0x00, 0xFF, 0x03, 0x80] {
                let mut s = TB336FU_SPS;
                s[i] = v;
                let _ = Sps::parse(&s);
            }
        }
        let _ = find_sps(&[0, 0, 1]);
        let _ = find_sps(&[0, 0, 1, 0x67]);
    }
}
