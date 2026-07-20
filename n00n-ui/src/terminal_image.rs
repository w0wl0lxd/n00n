use base64::Engine;
use n00n_agent::ImageSource;
use ratatui::layout::Size;
use ratatui_image::Resize;
use ratatui_image::picker::Picker;
use ratatui_image::sliced::SlicedProtocol;

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
        let img = ImageBuffer::from_fn(8, 8, |x, y| image::Rgb([x as u8 * 32, y as u8 * 32, 128]));
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
}
