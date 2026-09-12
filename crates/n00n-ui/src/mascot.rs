use std::sync::OnceLock;
use std::time::{Duration, Instant};

use image::imageops::FilterType;
use image::{DynamicImage, RgbaImage};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

use crate::cast::usize_to_u16;
use crate::theme::Theme;

const FRAME_INTERVAL: Duration = Duration::from_millis(100);
const MASCOT_SCALE_PERCENT: u32 = 100;
const MASCOT_MARGIN_CELLS: u16 = 4;
const BLINK_CYCLE_FRAMES: u16 = 53;
const LEFT_EYE: (i32, i32) = (246, 201);
const RIGHT_EYE: (i32, i32) = (316, 181);
const EYE_RADIUS: (i32, i32) = (22, 21);
const FUR: [u8; 4] = [154, 147, 169, 255];
const OUTLINE: [u8; 4] = [52, 32, 82, 255];

static MASCOT_IMAGE: OnceLock<DynamicImage> = OnceLock::new();

fn mascot_image() -> &'static DynamicImage {
    const MASCOT_PNG: &[u8] = include_bytes!("../../../site/android-chrome-512x512.png");
    MASCOT_IMAGE.get_or_init(|| match image::load_from_memory(MASCOT_PNG) {
        Ok(image) => image,
        Err(error) => {
            tracing::error!(%error, "failed to load branded mascot");
            DynamicImage::new_rgba8(1, 1)
        }
    })
}

#[derive(Clone, Copy)]
enum Blink {
    Open,
    Half,
    Closed,
}

struct SpriteFrame {
    pixels: Vec<[u8; 4]>,
}

struct SpriteCache {
    area: Rect,
    background: (u8, u8, u8),
    side: usize,
    left: i32,
    top: i32,
    frames: [SpriteFrame; 3],
}

pub struct Mascot {
    enabled: bool,
    mouse_col: Option<u16>,
    mouse_row: Option<u16>,
    frame: u16,
    last_frame: Instant,
    cache: Option<SpriteCache>,
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

    pub fn render(&mut self, area: Rect, buf: &mut Buffer, theme: &Theme, _accent: Color) {
        if !self.enabled || area.width < 24 || area.height < 12 {
            return;
        }

        let background = extract_rgb(theme.background, (12, 14, 20));
        let needs_rebuild = self
            .cache
            .as_ref()
            .is_none_or(|cache| cache.area != area || cache.background != background);
        if needs_rebuild {
            self.cache = build_cache(area, background);
        }

        let Some(cache) = self.cache.as_ref() else {
            return;
        };
        let frame = &cache.frames[blink_index(blink_for_frame(self.frame))];
        let (shift_x, shift_y) = sprite_shift(area, self.mouse_col, self.mouse_row, self.frame);
        let buf_width = usize::from(buf.area().width);

        for row in 0..usize::from(area.height) {
            let y = area.y + usize_to_u16(row);
            let row_offset = usize::from(y) * buf_width + usize::from(area.x);
            let virtual_top = i32::from(usize_to_u16(row)) * 2;
            let virtual_bottom = virtual_top.saturating_add(1);
            for col in 0..usize::from(area.width) {
                let virtual_x = i32::from(usize_to_u16(col));
                let top = sample(cache, frame, virtual_x - shift_x, virtual_top - shift_y);
                let bottom = sample(cache, frame, virtual_x - shift_x, virtual_bottom - shift_y);
                if let Some(cell) = buf.content.get_mut(row_offset + col) {
                    cell.set_symbol("▀")
                        .set_fg(Color::Rgb(top[0], top[1], top[2]))
                        .set_bg(Color::Rgb(bottom[0], bottom[1], bottom[2]));
                }
            }
        }
    }
}

fn build_cache(area: Rect, background: (u8, u8, u8)) -> Option<SpriteCache> {
    let available_width = area.width.saturating_sub(MASCOT_MARGIN_CELLS);
    let available_height = area.height.saturating_sub(2) * 2;
    let side = u32::from(available_width)
        .min(u32::from(available_height))
        .saturating_mul(MASCOT_SCALE_PERCENT)
        / 100;
    if side == 0 {
        return None;
    }

    let Ok(side_usize) = usize::try_from(side) else {
        return None;
    };
    let Ok(sprite_side) = i32::try_from(side) else {
        return None;
    };
    let left = (i32::from(area.width) - sprite_side) / 2;
    let top = (i32::from(area.height) * 2 - sprite_side) / 2;
    let frames = [
        build_frame(side, Blink::Open),
        build_frame(side, Blink::Half),
        build_frame(side, Blink::Closed),
    ];

    Some(SpriteCache {
        area,
        background,
        side: side_usize,
        left,
        top,
        frames,
    })
}

fn build_frame(side: u32, blink: Blink) -> SpriteFrame {
    let source = blink_source(blink);
    let resized = DynamicImage::ImageRgba8(source)
        .resize_exact(side, side, FilterType::Lanczos3)
        .to_rgba8();
    SpriteFrame {
        pixels: resized.pixels().map(|pixel| pixel.0).collect(),
    }
}

fn blink_source(blink: Blink) -> RgbaImage {
    let mut image = mascot_image().to_rgba8();
    match blink {
        Blink::Open => {}
        Blink::Half => {
            paint_lid(&mut image, LEFT_EYE, false);
            paint_lid(&mut image, RIGHT_EYE, false);
        }
        Blink::Closed => {
            paint_lid(&mut image, LEFT_EYE, true);
            paint_lid(&mut image, RIGHT_EYE, true);
        }
    }
    image
}

fn paint_lid(image: &mut RgbaImage, center: (i32, i32), closed: bool) {
    let (radius_x, radius_y) = EYE_RADIUS;
    for offset_y in -radius_y..=radius_y {
        for offset_x in -radius_x..=radius_x {
            let ellipse = offset_x * offset_x * radius_y * radius_y
                + offset_y * offset_y * radius_x * radius_x;
            let boundary = radius_x * radius_x * radius_y * radius_y;
            if ellipse <= boundary && (closed || offset_y <= 4) {
                put_pixel(image, center.0 + offset_x, center.1 + offset_y, FUR);
            }
        }
    }

    let line_y = if closed { 2 } else { 5 };
    for offset_x in -(radius_x - 3)..=(radius_x - 3) {
        let curve = offset_x * offset_x / 90;
        for thickness in 0..=2 {
            put_pixel(
                image,
                center.0 + offset_x,
                center.1 + line_y + curve + thickness,
                OUTLINE,
            );
        }
    }
}

fn put_pixel(image: &mut RgbaImage, x: i32, y: i32, color: [u8; 4]) {
    let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) else {
        return;
    };
    if x < image.width() && y < image.height() {
        image.put_pixel(x, y, image::Rgba(color));
    }
}

fn sample(cache: &SpriteCache, frame: &SpriteFrame, x: i32, y: i32) -> [u8; 3] {
    let sprite_x = x - cache.left;
    let sprite_y = y - cache.top;
    let (Ok(sprite_x), Ok(sprite_y)) = (usize::try_from(sprite_x), usize::try_from(sprite_y))
    else {
        return background_color(cache.background);
    };
    if sprite_x >= cache.side || sprite_y >= cache.side {
        return background_color(cache.background);
    }

    blend(
        cache.background,
        frame.pixels[sprite_y * cache.side + sprite_x],
    )
}

fn blend(background: (u8, u8, u8), foreground: [u8; 4]) -> [u8; 3] {
    let alpha = u16::from(foreground[3]);
    let inverse = 255_u16 - alpha;
    [
        blend_channel(background.0, foreground[0], alpha, inverse),
        blend_channel(background.1, foreground[1], alpha, inverse),
        blend_channel(background.2, foreground[2], alpha, inverse),
    ]
}

fn blend_channel(background: u8, foreground: u8, alpha: u16, inverse: u16) -> u8 {
    let value = u16::from(foreground) * alpha + u16::from(background) * inverse;
    ((value + 127) / 255).to_le_bytes()[0]
}

fn background_color(background: (u8, u8, u8)) -> [u8; 3] {
    [background.0, background.1, background.2]
}

fn blink_for_frame(frame: u16) -> Blink {
    match frame % BLINK_CYCLE_FRAMES {
        48 | 51 => Blink::Half,
        49 | 50 => Blink::Closed,
        _ => Blink::Open,
    }
}

const fn blink_index(blink: Blink) -> usize {
    match blink {
        Blink::Open => 0,
        Blink::Half => 1,
        Blink::Closed => 2,
    }
}

fn sprite_shift(
    area: Rect,
    mouse_col: Option<u16>,
    mouse_row: Option<u16>,
    frame: u16,
) -> (i32, i32) {
    const BOB: [i32; 20] = [
        0, 0, 0, -1, -1, -1, -2, -2, -2, -1, -1, -1, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    let bob = BOB[usize::from(frame) % BOB.len()];
    let horizontal = axis_shift(mouse_col, area.x, area.width);
    let vertical = axis_shift(mouse_row, area.y, area.height);
    (horizontal, bob + vertical)
}

fn axis_shift(position: Option<u16>, start: u16, length: u16) -> i32 {
    position.map_or(0, |position| {
        let center = start.saturating_add(length / 2);
        let margin = length / 5;
        if position < center.saturating_sub(margin) {
            -1
        } else {
            i32::from(position > center.saturating_add(margin))
        }
    })
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
        let first = mascot.cache.as_ref().map(std::ptr::from_ref);
        mascot.render(area, &mut buf, &theme, accent());
        let second = mascot.cache.as_ref().map(std::ptr::from_ref);
        assert_eq!(first, second);
    }

    #[test]
    fn blink_sequence_has_brief_closed_frames() {
        assert!(matches!(blink_for_frame(47), Blink::Open));
        assert!(matches!(blink_for_frame(48), Blink::Half));
        assert!(matches!(blink_for_frame(49), Blink::Closed));
        assert!(matches!(blink_for_frame(51), Blink::Half));
        assert!(matches!(blink_for_frame(52), Blink::Open));
    }
}
