use std::sync::OnceLock;

use image::DynamicImage;
use image::imageops::FilterType;
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui::widgets::Widget;
use ratatui_image::Image;
use ratatui_image::protocol::{Protocol, halfblocks::Halfblocks};

use crate::animation::animation_elapsed_ms;

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

pub struct Bunny {
    last_area: Option<Rect>,
    protocol: Option<Protocol>,
}

impl Default for Bunny {
    fn default() -> Self {
        Self::new()
    }
}

impl Bunny {
    #[must_use]
    pub fn new() -> Self {
        Self {
            last_area: None,
            protocol: None,
        }
    }

    #[must_use]
    pub fn is_animating(&self) -> bool {
        false
    }

    pub fn render(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        _theme: &crate::theme::Theme,
        streaming: bool,
    ) {
        const CYCLE_MS: u128 = 5_000;

        if !streaming || area.width < bunny_width() || area.height < 2 {
            return;
        }

        let elapsed_ms = animation_elapsed_ms();
        let elapsed = crate::cast::u128_to_f64(elapsed_ms);
        let cycle_ms = crate::cast::u128_to_f64(CYCLE_MS);
        let target_w = u32::from(bunny_width()) * 2;
        let target_h = 6;

        let max_x = f64::from(area.width) - f64::from(bunny_width());
        if max_x < 0.0 {
            return;
        }

        let t = crate::cast::u128_to_f64(elapsed_ms % CYCLE_MS) / cycle_ms;
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

        let hop = (elapsed / 250.0).sin() * 1.5;
        let hop_offset = crate::cast::f64_to_u16(hop);
        let y = area.y.saturating_add(hop_offset);

        let render_area = Rect::new(
            area.x + x,
            y,
            render_w,
            area.height.saturating_sub(hop_offset),
        );

        if self.last_area != Some(render_area) || self.protocol.is_none() {
            let resized = bunny_image().resize_to_fill(target_w, target_h, FilterType::Triangle);
            if let Ok(halfblocks) =
                Halfblocks::new(resized, Size::new(render_w, render_area.height))
            {
                self.protocol = Some(Protocol::Halfblocks(halfblocks));
            }
            self.last_area = Some(render_area);
        }

        if let Some(protocol) = self.protocol.as_ref() {
            Image::new(protocol).render(render_area, buf);
        }
    }
}

const fn bunny_width() -> u16 {
    16
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme;

    #[test]
    fn render_does_not_panic_in_empty_area() {
        let mut bunny = Bunny::new();
        let area = Rect::new(0, 0, 0, 0);
        let mut buf = Buffer::empty(area);
        let theme = theme::current();
        bunny.render(area, &mut buf, &theme, true);
    }

    #[test]
    fn render_does_not_panic_when_not_streaming() {
        let mut bunny = Bunny::new();
        let area = Rect::new(0, 0, 16, 3);
        let mut buf = Buffer::empty(area);
        let theme = theme::current();
        bunny.render(area, &mut buf, &theme, false);
    }

    #[test]
    fn is_animating_is_false() {
        let bunny = Bunny::new();
        assert!(!bunny.is_animating());
    }
}
