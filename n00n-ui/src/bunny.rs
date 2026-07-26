use std::sync::{Arc, OnceLock};

use image::imageops::FilterType;
use image::{DynamicImage, Rgba, RgbaImage};
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui::style::Color;
use ratatui::widgets::Widget;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::Protocol;
use ratatui_image::{Image, Resize};

use crate::animation::animation_elapsed_ms;
use crate::theme::Theme;

static BUNNY_IMAGE: OnceLock<DynamicImage> = OnceLock::new();

fn bunny_image() -> &'static DynamicImage {
    const BUNNY_PNG: &[u8] = include_bytes!("../assets/bunny.png");
    BUNNY_IMAGE.get_or_init(|| match image::load_from_memory(BUNNY_PNG) {
        Ok(img) => img,
        Err(error) => {
            tracing::error!(%error, "failed to load bunny.png");
            DynamicImage::new_rgba8(1, 1)
        }
    })
}

fn ratatui_color_to_rgba(color: Color) -> Option<Rgba<u8>> {
    let [r, g, b] = match color {
        Color::Rgb(red, green, blue) => [red, green, blue],
        Color::Black => [0, 0, 0],
        Color::Red => [205, 49, 49],
        Color::Green => [13, 188, 121],
        Color::Yellow => [229, 229, 16],
        Color::Blue => [36, 114, 200],
        Color::Magenta => [188, 63, 188],
        Color::Cyan => [17, 168, 205],
        Color::White => [229, 229, 229],
        Color::Gray => [136, 136, 136],
        Color::DarkGray => [85, 85, 85],
        Color::LightRed => [241, 76, 76],
        Color::LightGreen => [35, 209, 139],
        Color::LightYellow => [245, 245, 67],
        Color::LightBlue => [59, 142, 234],
        Color::LightMagenta => [214, 112, 214],
        Color::LightCyan => [41, 184, 219],
        Color::Reset | Color::Indexed(_) => return None,
    };
    Some(Rgba([r, g, b, 255]))
}

fn colorize_bunny(theme: &Theme) -> DynamicImage {
    let fg = match ratatui_color_to_rgba(theme.foreground) {
        Some(c) => c,
        None => Rgba([255, 255, 255, 255]),
    };
    let bg = match ratatui_color_to_rgba(theme.background) {
        Some(c) => c,
        None => Rgba([0, 0, 0, 255]),
    };

    let source = bunny_image().to_rgba8();
    let (width, height) = source.dimensions();
    let mut colored = RgbaImage::from_pixel(width, height, bg);

    for (x, y, pixel) in source.enumerate_pixels() {
        if pixel.0[3] > 0 {
            colored.put_pixel(x, y, fg);
        }
    }

    DynamicImage::ImageRgba8(colored)
}

pub struct Bunny {
    picker: Arc<Picker>,
    last_key: Option<(Size, Color, Color)>,
    protocol: Option<Protocol>,
}

impl Bunny {
    #[must_use]
    pub fn new(picker: Arc<Picker>) -> Self {
        Self {
            picker,
            last_key: None,
            protocol: None,
        }
    }

    #[must_use]
    pub fn is_animating(&self) -> bool {
        false
    }

    pub fn render(&mut self, area: Rect, buf: &mut Buffer, theme: &Theme, streaming: bool) {
        const CYCLE_MS: u128 = 5_000;
        const HOP_PERIOD_MS: f64 = 300.0;

        if !streaming || area.width < bunny_width() || area.height < bunny_height() {
            return;
        }

        let elapsed_ms = animation_elapsed_ms();
        let elapsed = crate::cast::u128_to_f64(elapsed_ms);
        let cycle_ms = crate::cast::u128_to_f64(CYCLE_MS);

        let max_x = f64::from(area.width) - f64::from(bunny_width());
        if max_x < 0.0 {
            return;
        }

        let t = elapsed / cycle_ms;
        let x_progress = t * 2.0;
        let x_normalized = if x_progress < 1.0 {
            x_progress
        } else {
            2.0 - x_progress
        };
        let x = crate::cast::f64_to_u16(x_normalized * max_x);

        let render_w = bunny_width().min(area.width - x);
        if render_w < 2 {
            return;
        }

        let max_y_offset = area.height.saturating_sub(bunny_height());
        let hop = (elapsed / HOP_PERIOD_MS).sin();
        let y_offset = if hop > 0.0 { 0 } else { max_y_offset };
        let y = area.y + y_offset;

        let render_area = Rect::new(area.x + x, y, render_w, bunny_height());
        let size = Size::new(render_w, bunny_height());
        let key = (size, theme.foreground, theme.background);

        if self.last_key != Some(key) || self.protocol.is_none() {
            let colored = colorize_bunny(theme);
            if let Ok(protocol) =
                self.picker
                    .new_protocol(colored, size, Resize::Fit(Some(FilterType::Lanczos3)))
            {
                self.protocol = Some(protocol);
                self.last_key = Some(key);
            }
        }

        if let Some(protocol) = self.protocol.as_ref() {
            Image::new(protocol).render(render_area, buf);
        }
    }
}

const fn bunny_width() -> u16 {
    4
}

const fn bunny_height() -> u16 {
    2
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme;

    #[test]
    fn render_does_not_panic_in_empty_area() {
        let mut bunny = Bunny::new(Arc::new(Picker::halfblocks()));
        let area = Rect::new(0, 0, 0, 0);
        let mut buf = Buffer::empty(area);
        let theme = theme::current();
        bunny.render(area, &mut buf, &theme, true);
    }

    #[test]
    fn render_does_not_panic_when_not_streaming() {
        let mut bunny = Bunny::new(Arc::new(Picker::halfblocks()));
        let area = Rect::new(0, 0, 8, 3);
        let mut buf = Buffer::empty(area);
        let theme = theme::current();
        bunny.render(area, &mut buf, &theme, false);
    }

    #[test]
    fn is_animating_is_false() {
        let bunny = Bunny::new(Arc::new(Picker::halfblocks()));
        assert!(!bunny.is_animating());
    }
}
