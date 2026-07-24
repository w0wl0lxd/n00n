//! Terminals without 24-bit color support (e.g. Terminal.app) misparse RGB
//! SGR sequences, corrupting the whole screen. When truecolor is not
//! detected, downgrade every RGB cell to the nearest xterm-256 color.
//!
//! Detection mirrors neovim: env advertisement (`COLORTERM`, `TERM`,
//! `TERM_PROGRAM`, known emulators), terminfo `RGB`/`Tc`/`setrgbf` caps,
//! and finally a DECRQSS SGR probe of the live terminal, which also works
//! over SSH where the env vars are stripped. `N00N_TRUECOLOR=0/1` overrides
//! everything.

use std::sync::OnceLock;

use ratatui::buffer::Buffer;
use ratatui::style::Color;

const CUBE_STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];
const TRUECOLOR_TERM_PROGRAMS: [&str; 7] = [
    "iTerm.app",
    "WezTerm",
    "ghostty",
    "vscode",
    "Hyper",
    "Tabby",
    "rio",
];
const TRUECOLOR_TERMS: [&str; 10] = [
    "kitty",
    "alacritty",
    "wezterm",
    "ghostty",
    "foot",
    "contour",
    "konsole",
    "iterm",
    "mintty",
    "rio",
];
const VTE_TRUECOLOR_VERSION: u32 = 3600;

static TRUECOLOR: OnceLock<bool> = OnceLock::new();

/// Must run while the terminal is in raw mode and before the input reader
/// thread spawns, so the DECRQSS probe can read replies from the tty itself.
pub(crate) fn init() {
    TRUECOLOR.get_or_init(detect);
}

pub(crate) fn downgrade_if_needed(buf: &mut Buffer) {
    if *TRUECOLOR.get_or_init(detect) {
        return;
    }
    for cell in &mut buf.content {
        cell.fg = downgrade(cell.fg);
        cell.bg = downgrade(cell.bg);
        cell.underline_color = downgrade(cell.underline_color);
    }
}

fn detect() -> bool {
    let (supported, source) = match truecolor_from_env(|var| std::env::var(var).ok()) {
        Some(v) => (v, "env"),
        None if terminfo_advertises() => (true, "terminfo"),
        None => (probe::terminal_supports_rgb(), "probe"),
    };
    tracing::info!(supported, source, "truecolor detection");
    supported
}

/// `Some` when the environment gives a definite answer, `None` to fall
/// through to terminfo and the live probe.
fn truecolor_from_env(get: impl Fn(&str) -> Option<String>) -> Option<bool> {
    if let Some(v) = get("N00N_TRUECOLOR") {
        return Some(v != "0");
    }
    let has =
        |var, needles: &[&str]| get(var).is_some_and(|v| needles.iter().any(|n| v.contains(n)));
    let advertised = has("COLORTERM", &["truecolor", "24bit"])
        || has("TERM", &["direct", "24bit"])
        || get("TERM").is_some_and(|t| {
            let t = t.to_ascii_lowercase();
            TRUECOLOR_TERMS.iter().any(|n| t.contains(n))
        })
        || get("TERM_PROGRAM").is_some_and(|p| {
            TRUECOLOR_TERM_PROGRAMS
                .iter()
                .any(|n| p.eq_ignore_ascii_case(n))
        })
        || get("KONSOLE_VERSION").is_some()
        || get("VTE_VERSION")
            .is_some_and(|v| v.parse::<u32>().is_ok_and(|v| v >= VTE_TRUECOLOR_VERSION));
    advertised.then_some(true)
}

fn terminfo_advertises() -> bool {
    let Ok(info) = termini::TermInfo::from_env() else {
        return false;
    };
    matches!(info.extended_cap("RGB"), Some(termini::Value::True))
        || matches!(info.extended_cap("Tc"), Some(termini::Value::True))
        || (info.extended_cap("setrgbf").is_some() && info.extended_cap("setrgbb").is_some())
}

/// A terminal that applied the RGB background echoes `48:2` (or `48;2`)
/// back in its DECRQSS SGR report; one that ignored it does not.
fn decrqss_reply_supports_rgb(buf: &[u8]) -> bool {
    contains(buf, b"48:2") || contains(buf, b"48;2")
}

/// DA1 reply (`ESC [ ? ... c`) marks the end of the probe: it is requested
/// last and every terminal answers it, so replies stay ordered.
fn da1_answered(buf: &[u8]) -> bool {
    buf.windows(3)
        .position(|w| w == b"\x1b[?")
        .is_some_and(|start| buf[start + 3..].contains(&b'c'))
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[cfg(unix)]
mod probe {
    use std::fs::File;
    use std::io::{Write, stdout};
    use std::os::fd::{AsRawFd, RawFd};
    use std::time::{Duration, Instant};

    use super::{da1_answered, decrqss_reply_supports_rgb};

    /// Set an RGB background, query it back with DECRQSS, reset, then DA1.
    const QUERY: &[u8] = b"\x1b[48;2;1;2;3m\x1bP$qm\x1b\\\x1b[0m\x1b[c";
    const TIMEOUT: Duration = Duration::from_millis(500);

    pub(super) fn terminal_supports_rgb() -> bool {
        try_probe().is_some_and(|v| v)
    }

    #[allow(unsafe_code)]
    fn try_probe() -> Option<bool> {
        let (_owned, fd) = open_tty()?;
        let mut out = stdout().lock();
        out.write_all(QUERY).ok()?;
        out.flush().ok()?;
        let deadline = Instant::now() + TIMEOUT;
        let mut buf = Vec::with_capacity(64);
        while !da1_answered(&buf) {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            if !wait_readable(fd, remaining) {
                break;
            }
            let mut chunk = [0u8; 256];
            // SAFETY: `libc::read` is called with a valid pointer to `chunk.as_mut_ptr()`
            // which points to a properly aligned buffer of size `chunk.len()`. The file
            // descriptor `fd` is either stdin (checked by `isatty`) or `/dev/tty` (opened
            // successfully). The return value is checked for errors before use.
            let n = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), chunk.len()) };
            if n <= 0 {
                break;
            }
            #[allow(clippy::cast_sign_loss)]
            buf.extend_from_slice(&chunk[..n as usize]);
        }
        Some(decrqss_reply_supports_rgb(&buf))
    }

    #[allow(unsafe_code)]
    fn open_tty() -> Option<(Option<File>, RawFd)> {
        // SAFETY: `libc::isatty` is a standard POSIX function that checks if a file
        // descriptor refers to a terminal. Passing `libc::STDIN_FILENO` is safe as it
        // is a constant representing the standard input file descriptor (0).
        if unsafe { libc::isatty(libc::STDIN_FILENO) } == 1 {
            return Some((None, libc::STDIN_FILENO));
        }
        let file = File::open("/dev/tty").ok()?;
        let fd = file.as_raw_fd();
        Some((Some(file), fd))
    }

    #[allow(unsafe_code)]
    fn wait_readable(fd: RawFd, timeout: Duration) -> bool {
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        #[allow(clippy::cast_possible_truncation)]
        let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
        // SAFETY: `libc::poll` is a standard POSIX function. The `pfd` pointer points to
        // a valid `libc::pollfd` struct with a valid file descriptor. The timeout is
        // clamped to `i32::MAX` to avoid overflow. The return value is checked for
        // errors and the revents field is checked for POLLIN.
        #[allow(clippy::borrow_as_ptr)]
        unsafe {
            libc::poll(&raw mut pfd, 1, ms) > 0 && pfd.revents & libc::POLLIN != 0
        }
    }
}

#[cfg(not(unix))]
mod probe {
    /// No tty probe off unix; Windows Terminal supports RGB and legacy
    /// consoles get colors mapped by crossterm, so assume truecolor.
    pub(super) fn terminal_supports_rgb() -> bool {
        true
    }
}

fn downgrade(color: Color) -> Color {
    match color {
        Color::Rgb(r, g, b) => Color::Indexed(nearest_indexed(r, g, b)),
        c => c,
    }
}

#[allow(clippy::cast_possible_truncation)]
fn nearest_indexed(r: u8, g: u8, b: u8) -> u8 {
    let step = |c: u8| match c {
        0..=47 => 0usize,
        48..=114 => 1,
        _ => (c as usize - 35) / 40,
    };
    let (ri, gi, bi) = (step(r), step(g), step(b));
    let sq = |a: u8, b: u8| (i32::from(a) - i32::from(b)).pow(2);
    let dist = |cr, cg, cb| sq(cr, r) + sq(cg, g) + sq(cb, b);
    let cube_dist = dist(CUBE_STEPS[ri], CUBE_STEPS[gi], CUBE_STEPS[bi]);
    let avg = (u32::from(r) + u32::from(g) + u32::from(b)) / 3;
    let gray_idx = (avg.saturating_sub(3) / 10).min(23) as u8;
    let gray = 8 + 10 * gray_idx;
    if dist(gray, gray, gray) < cube_dist {
        232 + gray_idx
    } else {
        // The max value is 16 + 36*5 + 6*5 + 5 = 231, which fits in u8.
        (16 + 36 * ri + 6 * gi + bi) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test_case(0, 0, 0, 16; "black_maps_to_cube_origin")]
    #[test_case(255, 255, 255, 231; "white_maps_to_cube_max")]
    #[test_case(255, 0, 0, 196; "pure_red")]
    #[test_case(0, 255, 0, 46; "pure_green")]
    #[test_case(0, 0, 255, 21; "pure_blue")]
    #[test_case(0x80, 0x80, 0x80, 244; "mid_gray_uses_gray_ramp")]
    #[test_case(0x28, 0x2a, 0x36, 236; "dracula_bg_stays_dark")]
    fn nearest(r: u8, g: u8, b: u8, expected: u8) {
        assert_eq!(nearest_indexed(r, g, b), expected);
    }

    #[test_case(&[("N00N_TRUECOLOR", "1")], Some(true); "override_forces_truecolor")]
    #[test_case(&[("N00N_TRUECOLOR", "0"), ("COLORTERM", "truecolor")], Some(false); "override_forces_downgrade")]
    #[test_case(&[("COLORTERM", "truecolor")], Some(true); "colorterm_advertises")]
    #[test_case(&[("COLORTERM", "24bit")], Some(true); "colorterm_24bit")]
    #[test_case(&[("TERM", "xterm-direct")], Some(true); "term_direct")]
    #[test_case(&[("TERM", "xterm-kitty")], Some(true); "term_kitty")]
    #[test_case(&[("TERM", "alacritty")], Some(true); "term_alacritty")]
    #[test_case(&[("TERM_PROGRAM", "WezTerm")], Some(true); "term_program_wezterm")]
    #[test_case(&[("TERM_PROGRAM", "Apple_Terminal")], None; "apple_terminal_not_advertised")]
    #[test_case(&[("KONSOLE_VERSION", "230800")], Some(true); "konsole_version")]
    #[test_case(&[("VTE_VERSION", "7200")], Some(true); "vte_new_enough")]
    #[test_case(&[("VTE_VERSION", "3500")], None; "vte_too_old")]
    #[test_case(&[("TERM", "xterm-256color")], None; "plain_256color_unknown")]
    #[test_case(&[], None; "nothing_advertised")]
    fn env_detection(vars: &[(&str, &str)], expected: Option<bool>) {
        let get = |var: &str| {
            vars.iter()
                .find(|(k, _)| *k == var)
                .map(|(_, v)| (*v).to_string())
        };
        assert_eq!(truecolor_from_env(get), expected);
    }

    #[test_case(b"\x1bP1$r0;48:2:1:2:3m\x1b\\\x1b[?65;1;9c", true; "kitty_style_colon_reply")]
    #[test_case(b"\x1bP1$r0;48;2;1;2;3m\x1b\\\x1b[?65;1;9c", true; "semicolon_reply")]
    #[test_case(b"\x1bP1$r0m\x1b\\\x1b[?1;2c", false; "rgb_ignored_by_terminal")]
    #[test_case(b"\x1b[?1;2c", false; "decrqss_unanswered")]
    #[test_case(b"", false; "no_reply")]
    fn decrqss_reply(buf: &[u8], expected: bool) {
        assert_eq!(decrqss_reply_supports_rgb(buf), expected);
    }

    #[test_case(b"\x1b[?65;1;9c", true; "da1_reply")]
    #[test_case(b"\x1bP1$r0;48:2:1:2:3m\x1b\\", false; "decrqss_only")]
    #[test_case(b"\x1b[?65;1;9", false; "partial_da1")]
    #[test_case(b"abc", false; "plain_text")]
    fn da1(buf: &[u8], expected: bool) {
        assert_eq!(da1_answered(buf), expected);
    }

    #[test]
    fn non_rgb_colors_pass_through() {
        assert_eq!(downgrade(Color::Reset), Color::Reset);
        assert_eq!(downgrade(Color::Indexed(42)), Color::Indexed(42));
    }
}
