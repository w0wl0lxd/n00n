//! Saturating cast helpers for UI dimensions and shader-like math.
//!
//! Terminal coordinates and animation progress are bounded by the screen size,
//! so the casts below clamp before truncating and explicitly acknowledge the
//! unavoidable precision loss when converting integer widths/counts to floats.
//!
//! Float-to-integer conversions inherently lose precision and may truncate;
//! these are clamped to valid ranges before conversion for UI safety where
//! the exact values are bounded by terminal dimensions (typically < 10,000).

#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]

pub(crate) const fn usize_to_f32(v: usize) -> f32 {
    v as f32
}

pub(crate) const fn usize_to_f64(v: usize) -> f64 {
    v as f64
}

pub(crate) fn f64_to_usize(v: f64) -> usize {
    let v = v.max(0.0);
    if v > usize::MAX as f64 {
        usize::MAX
    } else {
        v as usize
    }
}

pub(crate) fn f32_to_usize(v: f32) -> usize {
    let v = v.max(0.0);
    if v > usize::MAX as f32 {
        usize::MAX
    } else {
        v as usize
    }
}

pub(crate) fn f64_to_u16(v: f64) -> u16 {
    v.clamp(0.0, f64::from(u16::MAX)) as u16
}

pub(crate) fn f64_to_u32(v: f64) -> u32 {
    v.clamp(0.0, f64::from(u32::MAX)) as u32
}

pub(crate) fn f32_to_u8(v: f32) -> u8 {
    v.clamp(0.0, f32::from(u8::MAX)) as u8
}

pub(crate) fn usize_to_u16(v: usize) -> u16 {
    u16::try_from(v).unwrap_or_else(|_| u16::MAX)
}

pub(crate) fn u32_to_u16(v: u32) -> u16 {
    u16::try_from(v).unwrap_or_else(|_| u16::MAX)
}

pub(crate) fn usize_to_u32(v: usize) -> u32 {
    u32::try_from(v).unwrap_or_else(|_| u32::MAX)
}

pub(crate) fn u32_to_isize(v: u32) -> isize {
    isize::try_from(v).unwrap_or_else(|_| isize::MAX)
}

pub(crate) fn usize_to_isize(v: usize) -> isize {
    isize::try_from(v).unwrap_or_else(|_| isize::MAX)
}

pub(crate) fn isize_to_usize(v: isize) -> usize {
    usize::try_from(v).unwrap_or_else(|_| 0)
}
