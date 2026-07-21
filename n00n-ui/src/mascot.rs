use std::sync::OnceLock;

use image::DynamicImage;
use image::imageops::FilterType;
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui::style::Color;
use ratatui::widgets::Widget;
use ratatui_image::{
    Image,
    protocol::{Protocol, halfblocks::Halfblocks},
};

use crate::theme::Theme;

static MASCOT_IMAGE: OnceLock<DynamicImage> = OnceLock::new();

fn mascot_image() -> DynamicImage {
    const MASCOT_PNG: &[u8] = include_bytes!("../assets/mascot.png");
    MASCOT_IMAGE
        .get_or_init(|| match image::load_from_memory(MASCOT_PNG) {
            Ok(img) => img,
            Err(_) => DynamicImage::new_rgba8(1, 1),
        })
        .clone()
}

pub struct Mascot {
    enabled: bool,
    mouse_col: Option<u16>,
    mouse_row: Option<u16>,
    last_area: Option<Rect>,
    protocol: Option<Protocol>,
}

impl Mascot {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            mouse_col: None,
            mouse_row: None,
            last_area: None,
            protocol: None,
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn on_mouse(&mut self, column: u16, row: u16) {
        if self.enabled {
            self.mouse_col = Some(column);
            self.mouse_row = Some(row);
        }
    }

    pub fn tick(&mut self, _area: Rect) {}

    pub fn is_animating(&self) -> bool {
        false
    }

    pub fn render(&mut self, area: Rect, buf: &mut Buffer, _theme: &Theme, _accent: Color) {
        if !self.enabled || area.width < 24 || area.height < 12 {
            return;
        }

        if self.last_area != Some(area) || self.protocol.is_none() {
            let target_w = u32::from(area.width);
            let target_h = u32::from(area.height) * 2;
            let resized = mascot_image().resize_to_fill(target_w, target_h, FilterType::Triangle);
            if let Ok(halfblocks) = Halfblocks::new(resized, Size::new(area.width, area.height)) {
                self.protocol = Some(Protocol::Halfblocks(halfblocks));
            }
            self.last_area = Some(area);
        }

        if let Some(protocol) = self.protocol.as_ref() {
            Image::new(protocol).render(area, buf);
        }
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

        let non_empty = buf.content.iter().filter(|c| c.symbol() != " ").count();
        assert!(non_empty > 100);
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
    fn mouse_ignored_when_disabled() {
        let mut mascot = Mascot::new(false);
        mascot.on_mouse(50, 20);
        assert!(mascot.mouse_col.is_none());
    }

    #[test]
    #[ignore = "visual dump only"]
    fn visual_dump() {
        let mut mascot = Mascot::new(true);
        let area = Rect::new(0, 0, 80, 45);
        let mut buf = Buffer::empty(area);
        let theme = theme::current();
        mascot.render(area, &mut buf, &theme, accent());

        for y in area.y..area.y + area.height {
            let mut line = String::with_capacity(area.width as usize);
            for x in area.x..area.x + area.width {
                line.push(buf[(x, y)].symbol().chars().next().unwrap_or(' '));
            }
            println!("{line}");
        }
    }
}
