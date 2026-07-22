use base64::Engine;
use n00n_agent::ImageSource;
use ratatui::layout::Size;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::sliced::SlicedProtocol;
use ratatui_image::{FontSize, Resize};

pub(crate) fn picker() -> Picker {
    let mut picker = match crossterm::terminal::window_size() {
        Ok(size) => match cell_size(size.width, size.height, size.columns, size.rows) {
            Some(font_size) => picker_from_font_size(font_size),
            None => Picker::halfblocks(),
        },
        Err(error) => {
            tracing::debug!(%error, "terminal pixel size unavailable; using default image sizing");
            Picker::halfblocks()
        }
    };

    let term = env_hint("TERM");
    let kitty_window_id = env_hint("KITTY_WINDOW_ID");
    let protocol = protocol_from_hints(term.as_deref(), kitty_window_id.as_deref());
    if let Some(protocol) = protocol {
        picker.set_protocol_type(protocol);
    }
    picker
}

fn cell_size(width: u16, height: u16, columns: u16, rows: u16) -> Option<FontSize> {
    if width == 0 || height == 0 || columns == 0 || rows == 0 {
        return None;
    }
    let width = width / columns;
    let height = height / rows;
    (width > 0 && height > 0).then_some(FontSize::new(width, height))
}

#[allow(
    deprecated,
    reason = "stdio capability queries can outlive their timeout and steal TUI input"
)]
fn picker_from_font_size(font_size: FontSize) -> Picker {
    Picker::from_fontsize(font_size)
}

fn env_hint(name: &str) -> Option<String> {
    match std::env::var(name) {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => {
            tracing::debug!(name, "terminal environment hint is not valid UTF-8");
            None
        }
    }
}

fn protocol_from_hints(term: Option<&str>, kitty_window_id: Option<&str>) -> Option<ProtocolType> {
    let is_kitty = term.is_some_and(|value| value == "xterm-kitty")
        || kitty_window_id.is_some_and(|value| !value.is_empty());
    is_kitty.then_some(ProtocolType::Kitty)
}

pub struct TerminalImage {
    pub protocol: SlicedProtocol,
    pub size: Size,
}

impl TerminalImage {
    pub fn from_source(
        source: &ImageSource,
        picker: &Picker,
        max_width: u16,
    ) -> Result<Self, String> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(source.data.as_bytes())
            .map_err(|e| e.to_string())?;
        let dyn_img = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
        let target = Size::new(max_width, u16::MAX);
        let protocol = SlicedProtocol::new_with_resize(picker, dyn_img, target, Resize::Fit(None))
            .map_err(|e| e.to_string())?;
        let size = protocol.size();
        Ok(Self { protocol, size })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use image::ImageBuffer;
    use n00n_agent::{ImageMediaType, ImageSource};

    #[test]
    fn from_source_decodes_png_and_fits_width() {
        let img = ImageBuffer::from_fn(8, 8, |x, y| {
            image::Rgb([
                u8::try_from(x).unwrap_or_else(|_| u8::MAX) * 32,
                u8::try_from(y).unwrap_or_else(|_| u8::MAX) * 32,
                128,
            ])
        });
        let mut png = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let source = ImageSource {
            media_type: ImageMediaType::Png,
            data: base64::engine::general_purpose::STANDARD
                .encode(&png)
                .into(),
        };
        let picker = Picker::halfblocks();
        let term_img = TerminalImage::from_source(&source, &picker, 4).unwrap();
        assert!(term_img.size.width > 0 && term_img.size.width <= 4);
        assert!(term_img.size.height > 0);
    }

    #[test]
    fn cell_size_requires_pixel_and_cell_dimensions() {
        let size = cell_size(1200, 800, 120, 40).unwrap();
        assert_eq!((size.width, size.height), (10, 20));
        assert!(cell_size(0, 800, 120, 40).is_none());
        assert!(cell_size(1200, 800, 0, 40).is_none());
    }

    #[test]
    fn kitty_protocol_requires_a_kitty_terminal_hint() {
        assert_eq!(
            protocol_from_hints(Some("xterm-kitty"), None),
            Some(ProtocolType::Kitty)
        );
        assert_eq!(
            protocol_from_hints(Some("screen-256color"), Some("1")),
            Some(ProtocolType::Kitty)
        );
        assert_eq!(protocol_from_hints(Some("xterm-256color"), None), None);
    }
}
