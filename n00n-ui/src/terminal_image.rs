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

    let protocol = detect_protocol(
        env_hint("TERM").as_deref(),
        env_hint("TERM_PROGRAM").as_deref(),
        env_hint("LC_TERMINAL").as_deref(),
        |name| env_hint(name).is_some(),
    );
    picker.set_protocol_type(protocol);
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
        Ok(value) if !value.is_empty() => Some(value),
        Ok(_) | Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => {
            tracing::debug!(name, "terminal environment hint is not valid UTF-8");
            None
        }
    }
}

fn detect_protocol(
    term: Option<&str>,
    term_program: Option<&str>,
    lc_terminal: Option<&str>,
    env_present: impl Fn(&str) -> bool,
) -> ProtocolType {
    use ProtocolType::{Halfblocks, Iterm2, Kitty, Sixel};

    if env_present("KONSOLE_VERSION") || env_present("ALACRITTY_WINDOW_ID") {
        return Halfblocks;
    }

    if env_present("KITTY_WINDOW_ID") || env_present("GHOSTTY_RESOURCES_DIR") {
        return Kitty;
    }

    if env_present("WEZTERM_PANE")
        || env_present("WEZTERM_EXECUTABLE")
        || env_present("ITERM_SESSION_ID")
    {
        return Iterm2;
    }

    if let Some(tp) = term_program {
        match tp.to_lowercase().as_str() {
            "ghostty" | "xterm-ghostty" => return Kitty,
            "wezterm" | "iterm.app" | "rio" | "tabby" | "hyper" | "bobcat" | "mintty"
            | "vscode" => return Iterm2,
            "warpterminal" | "apple_terminal" | "alacritty" | "contour" | "ctx" | "black box" => {
                return Halfblocks;
            }
            _ => {}
        }
    }

    if lc_terminal.is_some_and(|value| value.to_lowercase().contains("iterm")) {
        return Iterm2;
    }

    if let Some(t) = term {
        let t = t.to_lowercase();
        if t.contains("kitty") || t.contains("ghostty") {
            return Kitty;
        }
        if t.starts_with("foot") || t.contains("mlterm") {
            return Sixel;
        }
        if t.contains("wezterm")
            || t.contains("iterm")
            || t.starts_with("rio")
            || t.contains("tabby")
            || t.contains("hyper")
            || t.contains("bobcat")
            || t.contains("mintty")
            || t.contains("vscode")
        {
            return Iterm2;
        }
        if t.contains("xterm")
            && env_present("XTERM_VERSION")
            && (t.contains("340") || t.contains("sixel"))
        {
            return Sixel;
        }
        if t.contains("konsole")
            || t.contains("alacritty")
            || t.contains("contour")
            || t.contains("warp")
            || t.contains("apple_terminal")
        {
            return Halfblocks;
        }
    }

    Halfblocks
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

    fn env_with<'a>(present: &'a [&str]) -> impl Fn(&str) -> bool + 'a {
        |name| present.contains(&name)
    }

    fn no_env(_: &str) -> bool {
        false
    }

    #[test]
    fn from_source_decodes_png_and_fits_width() {
        let img = ImageBuffer::from_fn(8, 8, |x, y| {
            image::Rgb([
                u8::try_from(x * 32).unwrap(),
                u8::try_from(y * 32).unwrap(),
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
    fn kitty_is_detected_from_term_and_env() {
        assert_eq!(
            detect_protocol(Some("xterm-kitty"), None, None, no_env),
            ProtocolType::Kitty
        );
        assert_eq!(
            detect_protocol(Some("xterm-ghostty"), None, None, no_env),
            ProtocolType::Kitty
        );
        assert_eq!(
            detect_protocol(
                Some("xterm-256color"),
                None,
                None,
                env_with(&["KITTY_WINDOW_ID"])
            ),
            ProtocolType::Kitty
        );
        assert_eq!(
            detect_protocol(Some("xterm-256color"), Some("ghostty"), None, no_env),
            ProtocolType::Kitty
        );
        assert_eq!(
            detect_protocol(None, None, None, env_with(&["GHOSTTY_RESOURCES_DIR"])),
            ProtocolType::Kitty
        );
    }

    #[test]
    fn iterm2_is_detected_from_term_program_and_env() {
        for tp in [
            "WezTerm",
            "iTerm.app",
            "Rio",
            "Tabby",
            "Hyper",
            "Bobcat",
            "mintty",
            "vscode",
        ] {
            assert_eq!(
                detect_protocol(Some("xterm-256color"), Some(tp), None, no_env),
                ProtocolType::Iterm2,
                "expected {tp} to use iTerm2"
            );
        }
        assert_eq!(
            detect_protocol(Some("wezterm"), None, None, no_env),
            ProtocolType::Iterm2
        );
        let iterm_env = |name: &str| {
            matches!(
                name,
                "WEZTERM_PANE" | "WEZTERM_EXECUTABLE" | "ITERM_SESSION_ID"
            )
        };
        assert_eq!(
            detect_protocol(None, None, None, iterm_env),
            ProtocolType::Iterm2
        );
        assert_eq!(
            detect_protocol(None, None, Some("iTerm2"), no_env),
            ProtocolType::Iterm2
        );
    }

    #[test]
    fn sixel_is_detected_for_foot_mlterm_and_xterm_340() {
        assert_eq!(
            detect_protocol(Some("foot"), None, None, no_env),
            ProtocolType::Sixel
        );
        assert_eq!(
            detect_protocol(Some("foot-256color"), None, None, no_env),
            ProtocolType::Sixel
        );
        assert_eq!(
            detect_protocol(Some("mlterm"), None, None, no_env),
            ProtocolType::Sixel
        );
        assert_eq!(
            detect_protocol(
                Some("xterm-340color"),
                None,
                None,
                env_with(&["XTERM_VERSION"])
            ),
            ProtocolType::Sixel
        );
    }

    #[test]
    fn broken_or_unknown_terminals_fall_back_to_halfblocks() {
        for tp in [
            "WarpTerminal",
            "Apple_Terminal",
            "Alacritty",
            "Contour",
            "ctx",
            "Black Box",
        ] {
            assert_eq!(
                detect_protocol(Some("xterm-256color"), Some(tp), None, no_env),
                ProtocolType::Halfblocks,
                "expected {tp} to fall back to halfblocks"
            );
        }
        assert_eq!(
            detect_protocol(Some("xterm-256color"), None, None, no_env),
            ProtocolType::Halfblocks
        );
        assert_eq!(
            detect_protocol(None, None, None, no_env),
            ProtocolType::Halfblocks
        );
        assert_eq!(
            detect_protocol(
                Some("xterm-256color"),
                None,
                None,
                env_with(&["KONSOLE_VERSION"])
            ),
            ProtocolType::Halfblocks
        );
        assert_eq!(
            detect_protocol(
                Some("xterm-256color"),
                None,
                None,
                env_with(&["ALACRITTY_WINDOW_ID"])
            ),
            ProtocolType::Halfblocks
        );
    }
}
