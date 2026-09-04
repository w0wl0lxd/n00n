use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

use crate::cast::{f32_to_u8, usize_to_f32, usize_to_u16, usize_to_u32};
use crate::theme::Theme;

const FRAME_INTERVAL: Duration = Duration::from_millis(100);
const LOGO_HALF_HEIGHT: f32 = 0.31;
const LOGO_HALF_WIDTH: f32 = 0.13;
const LOGO_CENTERS: [f32; 4] = [-0.52, -0.175, 0.175, 0.52];
const LOGO_STROKE: f32 = 0.025;
const LOGO_GLOW: f32 = 0.11;
const SIGNAL_PERIOD: u16 = 48;
const STAR_THRESHOLD: u32 = 248;

#[derive(Clone, Copy)]
struct ScenePixel {
    base: [u8; 3],
    glow: u8,
    signal_phase: u8,
    star_phase: u8,
}

struct SceneCache {
    area: Rect,
    background: (u8, u8, u8),
    accent: (u8, u8, u8),
    pixels: Vec<ScenePixel>,
}

pub struct Mascot {
    enabled: bool,
    mouse_col: Option<u16>,
    mouse_row: Option<u16>,
    frame: u16,
    last_frame: Instant,
    cache: Option<SceneCache>,
}

impl Mascot {
    #[must_use]
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            mouse_col: None,
            mouse_row: None,
            frame: 0,
            last_frame: Instant::now(),
            cache: None,
        }
    }

    #[must_use]
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn on_mouse(&mut self, column: u16, row: u16) {
        if self.enabled {
            self.mouse_col = Some(column);
            self.mouse_row = Some(row);
        }
    }

    pub fn tick(&mut self, _area: Rect) {
        if self.is_animating() {
            self.frame = self.frame.wrapping_add(1);
            self.last_frame = Instant::now();
        }
    }

    #[must_use]
    pub fn is_animating(&self) -> bool {
        self.enabled && self.last_frame.elapsed() >= FRAME_INTERVAL
    }

    pub fn render(&mut self, area: Rect, buf: &mut Buffer, theme: &Theme, accent: Color) {
        if !self.enabled || area.width < 24 || area.height < 12 {
            return;
        }

        let background = extract_rgb(theme.background, (12, 14, 20));
        let accent = extract_rgb(accent, (100, 150, 255));
        let needs_rebuild = self.cache.as_ref().is_none_or(|cache| {
            cache.area != area || cache.background != background || cache.accent != accent
        });
        if needs_rebuild {
            self.cache = Some(build_scene(area, background, accent));
        }

        let Some(cache) = self.cache.as_ref() else {
            return;
        };
        let width = usize::from(area.width);
        let virtual_height = usize::from(area.height) * 2;
        let mouse = mouse_position(area, self.mouse_col, self.mouse_row);
        let scan_row = usize::from(self.frame) % virtual_height;
        let pulse = triangle_wave(self.frame, 30);
        let buf_width = usize::from(buf.area().width);

        for row in 0..usize::from(area.height) {
            let top_idx = row * 2 * width;
            let bottom_idx = top_idx + width;
            let y = area.y + usize_to_u16(row);
            let row_offset = usize::from(y) * buf_width + usize::from(area.x);

            for col in 0..width {
                let top = animated_color(
                    cache.pixels[top_idx + col],
                    accent,
                    self.frame,
                    pulse,
                    top_idx / width,
                    scan_row,
                    mouse,
                    col,
                );
                let bottom = animated_color(
                    cache.pixels[bottom_idx + col],
                    accent,
                    self.frame,
                    pulse,
                    bottom_idx / width,
                    scan_row,
                    mouse,
                    col,
                );
                if let Some(cell) = buf.content.get_mut(row_offset + col) {
                    cell.set_symbol("▀")
                        .set_fg(Color::Rgb(top[0], top[1], top[2]))
                        .set_bg(Color::Rgb(bottom[0], bottom[1], bottom[2]));
                }
            }
        }
    }
}

fn build_scene(area: Rect, background: (u8, u8, u8), accent: (u8, u8, u8)) -> SceneCache {
    let width = usize::from(area.width);
    let height = usize::from(area.height) * 2;
    let inv_width = 1.0 / usize_to_f32(width);
    let inv_height = 1.0 / usize_to_f32(height);
    let aspect = usize_to_f32(width) / usize_to_f32(height).max(1.0);
    let mut pixels = Vec::with_capacity(width * height);

    for row in 0..height {
        let y = (usize_to_f32(row) + 0.5) * inv_height * 2.0 - 1.0;
        for col in 0..width {
            let x = ((usize_to_f32(col) + 0.5) * inv_width * 2.0 - 1.0) * aspect;
            let radius_squared = x * x + y * y;
            let vignette = (1.0 - radius_squared * 0.38).clamp(0.22, 1.0);
            let grid = grid_intensity(x, y, width, height);
            let logo_distance = logo_distance(x, y);
            let glow = ((LOGO_GLOW - logo_distance) / LOGO_GLOW).clamp(0.0, 1.0);
            let core = ((LOGO_STROKE - logo_distance) / LOGO_STROKE).clamp(0.0, 1.0);
            let star_hash = hash_2d(col, row);
            let hash_bytes = star_hash.to_le_bytes();
            let star = u32::from(hash_bytes[0]) > STAR_THRESHOLD;
            let background_lift = 0.72 + vignette * 0.28 + grid * 0.08;
            let accent_lift = grid * 0.035 + glow * 0.16 + core * 0.2;
            let base = mix_color(background, accent, background_lift, accent_lift);

            pixels.push(ScenePixel {
                base,
                glow: f32_to_u8((glow * 0.48 + core * 0.52) * 255.0),
                signal_phase: hash_bytes[1],
                star_phase: if star { hash_bytes[2] } else { 0 },
            });
        }
    }

    SceneCache {
        area,
        background,
        accent,
        pixels,
    }
}

fn animated_color(
    pixel: ScenePixel,
    accent: (u8, u8, u8),
    frame: u16,
    pulse: f32,
    row: usize,
    scan_row: usize,
    mouse: Option<(f32, f32)>,
    col: usize,
) -> [u8; 3] {
    let glow = f32::from(pixel.glow) / 255.0;
    let signal = signal_intensity(frame, pixel.signal_phase);
    let scan = row.abs_diff(scan_row) <= 1;
    let star = star_intensity(frame, pixel.star_phase);
    let mouse_glow = mouse.map_or(0.0, |(mx, my)| {
        let dx = usize_to_f32(col) - mx;
        let dy = usize_to_f32(row) - my;
        (1.0 - (dx * dx + dy * dy) / 160.0).clamp(0.0, 1.0) * 0.08
    });
    let lift = glow * (0.2 + pulse * 0.12 + signal * 0.34)
        + if scan { glow * 0.13 } else { 0.0 }
        + star
        + mouse_glow;

    [
        channel_lift(pixel.base[0], accent.0, lift),
        channel_lift(pixel.base[1], accent.1, lift),
        channel_lift(pixel.base[2], accent.2, lift),
    ]
}

fn logo_distance(x: f32, y: f32) -> f32 {
    let mut distance = f32::MAX;
    for (index, center) in LOGO_CENTERS.into_iter().enumerate() {
        let local_x = x - center;
        let glyph_distance = if index == 1 || index == 2 {
            zero_distance(local_x, y)
        } else {
            n_distance(local_x, y)
        };
        distance = distance.min(glyph_distance);
    }
    distance
}

fn n_distance(x: f32, y: f32) -> f32 {
    let left = segment_distance(
        x,
        y,
        -LOGO_HALF_WIDTH,
        -LOGO_HALF_HEIGHT,
        -LOGO_HALF_WIDTH,
        LOGO_HALF_HEIGHT,
    );
    let right = segment_distance(
        x,
        y,
        LOGO_HALF_WIDTH,
        -LOGO_HALF_HEIGHT,
        LOGO_HALF_WIDTH,
        LOGO_HALF_HEIGHT,
    );
    let diagonal = segment_distance(
        x,
        y,
        -LOGO_HALF_WIDTH,
        LOGO_HALF_HEIGHT,
        LOGO_HALF_WIDTH,
        -LOGO_HALF_HEIGHT,
    );
    left.min(right).min(diagonal)
}

fn zero_distance(x: f32, y: f32) -> f32 {
    let normalized_x = x / LOGO_HALF_WIDTH;
    let normalized_y = y / LOGO_HALF_HEIGHT;
    ((normalized_x * normalized_x + normalized_y * normalized_y).sqrt() - 1.0).abs()
        * LOGO_HALF_WIDTH
}

fn segment_distance(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let segment_x = bx - ax;
    let segment_y = by - ay;
    let point_x = px - ax;
    let point_y = py - ay;
    let denominator = segment_x * segment_x + segment_y * segment_y;
    let t = ((point_x * segment_x + point_y * segment_y) / denominator).clamp(0.0, 1.0);
    let dx = point_x - segment_x * t;
    let dy = point_y - segment_y * t;
    (dx * dx + dy * dy).sqrt()
}

fn grid_intensity(x: f32, y: f32, width: usize, height: usize) -> f32 {
    let column = ((x + 2.0) * usize_to_f32(width) * 0.11).fract();
    let row = ((y + 1.0) * usize_to_f32(height) * 0.055).fract();
    let line = column.min(1.0 - column).min(row.min(1.0 - row));
    ((0.035 - line) / 0.035).clamp(0.0, 1.0)
}

fn mix_color(
    background: (u8, u8, u8),
    accent: (u8, u8, u8),
    background_lift: f32,
    accent_lift: f32,
) -> [u8; 3] {
    [
        f32_to_u8(f32::from(background.0) * background_lift + f32::from(accent.0) * accent_lift),
        f32_to_u8(f32::from(background.1) * background_lift + f32::from(accent.1) * accent_lift),
        f32_to_u8(f32::from(background.2) * background_lift + f32::from(accent.2) * accent_lift),
    ]
}

fn channel_lift(base: u8, accent: u8, amount: f32) -> u8 {
    f32_to_u8(f32::from(base) + (f32::from(accent) - f32::from(base)) * amount.clamp(0.0, 1.0))
}

fn signal_intensity(frame: u16, phase: u8) -> f32 {
    let position = (frame.wrapping_mul(5) + u16::from(phase)) % SIGNAL_PERIOD;
    let distance = position.min(SIGNAL_PERIOD - position);
    (1.0 - f32::from(distance) / 5.0).clamp(0.0, 1.0)
}

fn star_intensity(frame: u16, phase: u8) -> f32 {
    if phase == 0 {
        return 0.0;
    }
    let wave = triangle_wave(frame.wrapping_add(u16::from(phase)), 36);
    wave * wave * 0.35
}

fn triangle_wave(frame: u16, period: u16) -> f32 {
    let position = frame % period;
    let half = period / 2;
    let distance = position.abs_diff(half);
    1.0 - f32::from(distance) / f32::from(half)
}

fn mouse_position(area: Rect, col: Option<u16>, row: Option<u16>) -> Option<(f32, f32)> {
    let col = col?.checked_sub(area.x)?;
    let row = row?.checked_sub(area.y)?;
    if col >= area.width || row >= area.height {
        return None;
    }
    Some((f32::from(col), f32::from(row) * 2.0))
}

fn hash_2d(x: usize, y: usize) -> u32 {
    let mut value =
        usize_to_u32(x).wrapping_mul(0x9e37_79b1) ^ usize_to_u32(y).wrapping_mul(0x85eb_ca77);
    value ^= value >> 16;
    value = value.wrapping_mul(0x7feb_352d);
    value ^ (value >> 15)
}

fn extract_rgb(color: Color, fallback: (u8, u8, u8)) -> (u8, u8, u8) {
    match color {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => fallback,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme;

    fn accent() -> Color {
        Color::Rgb(120, 160, 255)
    }

    #[test]
    fn render_does_not_panic_in_empty_area() {
        let mut mascot = Mascot::new(true);
        let area = Rect::new(0, 0, 0, 0);
        let mut buf = Buffer::empty(area);
        let theme = theme::current();
        mascot.render(area, &mut buf, &theme, accent());
    }

    #[test]
    fn render_does_not_panic_in_small_area() {
        let mut mascot = Mascot::new(true);
        let area = Rect::new(0, 0, 5, 3);
        let mut buf = Buffer::empty(area);
        let theme = theme::current();
        mascot.render(area, &mut buf, &theme, accent());
    }

    #[test]
    fn render_fills_large_area() {
        let mut mascot = Mascot::new(true);
        let area = Rect::new(0, 0, 80, 45);
        let mut buf = Buffer::empty(area);
        let theme = theme::current();
        mascot.render(area, &mut buf, &theme, accent());

        let painted = buf
            .content
            .iter()
            .filter(|cell| cell.symbol() == "▀")
            .count();
        assert_eq!(painted, usize::from(area.width) * usize::from(area.height));
    }

    #[test]
    fn enabled_flag() {
        let enabled = Mascot::new(true);
        assert!(enabled.enabled());
        assert!(!enabled.is_animating());

        let disabled = Mascot::new(false);
        assert!(!disabled.enabled());
        assert!(!disabled.is_animating());
    }

    #[test]
    fn animation_is_due_only_after_frame_interval() {
        let mut mascot = Mascot::new(true);
        assert!(!mascot.is_animating());

        mascot.last_frame = Instant::now().checked_sub(FRAME_INTERVAL).unwrap();
        assert!(mascot.is_animating());
    }

    #[test]
    fn tick_advances_one_frame_when_due() {
        let mut mascot = Mascot::new(true);
        mascot.last_frame = Instant::now().checked_sub(FRAME_INTERVAL).unwrap();
        mascot.tick(Rect::new(0, 0, 80, 45));
        assert_eq!(mascot.frame, 1);
        assert!(!mascot.is_animating());
    }

    #[test]
    fn mouse_ignored_when_disabled() {
        let mut mascot = Mascot::new(false);
        mascot.on_mouse(50, 20);
        assert!(mascot.mouse_col.is_none());
    }

    #[test]
    fn scene_cache_is_reused_until_geometry_or_palette_changes() {
        let mut mascot = Mascot::new(true);
        let area = Rect::new(0, 0, 80, 45);
        let mut buf = Buffer::empty(area);
        let theme = theme::current();
        mascot.render(area, &mut buf, &theme, accent());
        let first = mascot.cache.as_ref().map(|cache| cache.pixels.as_ptr());
        mascot.render(area, &mut buf, &theme, accent());
        let second = mascot.cache.as_ref().map(|cache| cache.pixels.as_ptr());
        assert_eq!(first, second);
    }
}
