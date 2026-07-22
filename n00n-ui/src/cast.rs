//! Saturating cast helpers for UI dimensions and shader-like math.
//!
//! Terminal coordinates and animation progress are bounded by the screen size,
//! so the casts below clamp before truncating and explicitly acknowledge the
//! unavoidable precision loss when converting integer widths/counts to floats.

#[allow(clippy::cast_precision_loss, dead_code)]
pub(crate) const fn usize_to_f32(v: usize) -> f32 {
    v as f32
}

#[allow(clippy::cast_precision_loss)]
pub(crate) const fn usize_to_f64(v: usize) -> f64 {
    v as f64
}

#[allow(clippy::cast_precision_loss, dead_code)]
pub(crate) const fn u64_to_f32(v: u64) -> f32 {
    v as f32
}

#[allow(clippy::cast_precision_loss, dead_code)]
pub(crate) const fn u128_to_f64(v: u128) -> f64 {
    v as f64
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub(crate) fn f64_to_usize(v: f64) -> usize {
    v.max(0.0) as usize
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, dead_code)]
pub(crate) fn f32_to_usize(v: f32) -> usize {
    v.max(0.0) as usize
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub(crate) fn f64_to_u16(v: f64) -> u16 {
    v.clamp(0.0, f64::from(u16::MAX)) as u16
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, dead_code)]
pub(crate) fn f64_to_u32(v: f64) -> u32 {
    v.clamp(0.0, f64::from(u32::MAX)) as u32
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, dead_code)]
pub(crate) fn f32_to_u8(v: f32) -> u8 {
    v.clamp(0.0, f32::from(u8::MAX)) as u8
}

#[allow(clippy::cast_possible_truncation)]
pub(crate) fn usize_to_u16(v: usize) -> u16 {
    v.min(u16::MAX as usize) as u16
}

#[allow(clippy::cast_possible_truncation)]
pub(crate) fn u32_to_u16(v: u32) -> u16 {
    v.min(u32::from(u16::MAX)) as u16
}

#[allow(clippy::cast_possible_wrap)]
pub(crate) fn cast_signed(v: u32) -> i32 {
    v.min(i32::MAX as u32) as i32
}
