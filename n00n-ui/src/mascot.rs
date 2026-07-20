use std::time::Instant;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

use crate::theme::{Theme, lerp_u8};

const SPRITE_WIDTH: u16 = 16;
const SPRITE_HEIGHT_HALF_ROWS: u16 = 8;
const TERMINAL_ROWS: u16 = SPRITE_HEIGHT_HALF_ROWS / 2;
const GAZE_TRANSITION_MS: u128 = 48;
const BLINK_INTERVAL_MIN_MS: u64 = 2500;
const BLINK_INTERVAL_MAX_MS: u64 = 4000;
const BLINK_DURATION_MS: u128 = 120;
const BREATHE_PERIOD_S: f32 = 2.0;
const BREATHE_THRESHOLD: f32 = 0.85;

const FIXED_ROWS: [&str; 6] = [
    "..kk........kk..",
    "..kHHk....kHHk..",
    ".kHHSSSSSSSSHHk.",
    "kHSSBBSkSSSBBSHk",
    "kHSSSSSSSSSSSSHk",
    "...OOOSSSSOOO...",
];

const EYES: [(&str, &str, &str, &str); 9] = [
    ("WEPS", "EEPS", "SEPW", "SEPE"),
    ("WPES", "PEES", "SPEW", "SPEE"),
    ("WESP", "EESP", "SEWP", "SEEP"),
    ("WEPS", "EEES", "SEPW", "SEEE"),
    ("WEES", "EEPS", "SEEW", "SEPE"),
    ("WPES", "PEES", "SPEW", "SEEE"),
    ("WESP", "EESP", "SEWP", "SEEE"),
    ("WPES", "PEES", "SEEW", "SPEE"),
    ("WESP", "EESP", "SEEW", "SEEP"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PixelRole {
    Transparent,
    Skin,
    Hair,
    Eye,
    Pupil,
    Highlight,
    Blush,
    Bow,
    Outline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GazeDirection {
    Straight,
    Left,
    Right,
    Up,
    Down,
    UpLeft,
    UpRight,
    DownLeft,
    DownRight,
}

impl GazeDirection {
    fn as_index(self) -> usize {
        self as usize
    }
}

pub struct Mascot {
    enabled: bool,
    frames: Vec<Frame>,
    mouse_col: Option<u16>,
    mouse_row: Option<u16>,
    current_gaze: GazeDirection,
    target_gaze: GazeDirection,
    gaze_transition_start: Option<Instant>,
    last_blink: Instant,
    next_blink_interval: u64,
    is_blinking: bool,
    blink_start: Option<Instant>,
    breathe_phase: f32,
    last_tick: Instant,
}

type Frame = Vec<Vec<(PixelRole, PixelRole)>>;

struct Palette {
    skin: Color,
    hair: Color,
    eye: Color,
    pupil: Color,
    highlight: Color,
    blush: Color,
    bow: Color,
    outline: Color,
}

impl Mascot {
    pub fn new(enabled: bool) -> Self {
        let now = Instant::now();
        Self {
            enabled,
            frames: build_frames(),
            mouse_col: None,
            mouse_row: None,
            current_gaze: GazeDirection::Straight,
            target_gaze: GazeDirection::Straight,
            gaze_transition_start: None,
            last_blink: now,
            next_blink_interval: random_blink_interval(),
            is_blinking: false,
            blink_start: None,
            breathe_phase: 0.0,
            last_tick: now,
        }
    }

    pub fn on_mouse(&mut self, column: u16, row: u16) {
        if !self.enabled {
            return;
        }
        self.mouse_col = Some(column);
        self.mouse_row = Some(row);
    }

    pub fn tick(&mut self, area: Rect) {
        if !self.enabled {
            return;
        }

        let now = Instant::now();
        let delta = now.duration_since(self.last_tick);
        self.last_tick = now;

        self.breathe_phase = (self.breathe_phase
            + delta.as_secs_f32() * std::f32::consts::TAU / BREATHE_PERIOD_S)
            % std::f32::consts::TAU;

        if let (Some(col), Some(row)) = (self.mouse_col, self.mouse_row) {
            let target = self.compute_target_gaze(col, row, area);
            if target != self.target_gaze {
                self.target_gaze = target;
                self.gaze_transition_start = Some(now);
            }
        }

        if let Some(start) = self.gaze_transition_start
            && now.duration_since(start).as_millis() >= GAZE_TRANSITION_MS
        {
            self.current_gaze = self.target_gaze;
            self.gaze_transition_start = None;
        }

        if self.is_blinking {
            if let Some(start) = self.blink_start
                && now.duration_since(start).as_millis() >= BLINK_DURATION_MS
            {
                self.is_blinking = false;
                self.blink_start = None;
                self.last_blink = now;
                self.next_blink_interval = random_blink_interval();
            }
        } else if now.duration_since(self.last_blink).as_millis()
            >= self.next_blink_interval as u128
        {
            self.is_blinking = true;
            self.blink_start = Some(now);
        }
    }

    fn compute_target_gaze(&self, col: u16, row: u16, area: Rect) -> GazeDirection {
        let face_x = area.x + (area.width.saturating_sub(SPRITE_WIDTH)) / 2 + SPRITE_WIDTH / 2;
        let face_y = area.y + 1 + TERMINAL_ROWS / 2;

        let dx = f64::from(col) - f64::from(face_x);
        let dy = f64::from(row) - f64::from(face_y);

        let abs_dx = dx.abs();
        let abs_dy = dy.abs();

        if abs_dx < 3.0 && abs_dy < 2.0 {
            return GazeDirection::Straight;
        }

        let vertical = abs_dy > abs_dx * 2.0;
        let horizontal = abs_dx > abs_dy * 2.0;

        match (dx > 0.0, dy > 0.0, horizontal, vertical) {
            (_, _, true, _) if dx > 0.0 => GazeDirection::Right,
            (_, _, true, _) => GazeDirection::Left,
            (_, _, _, true) if dy > 0.0 => GazeDirection::Down,
            (_, _, _, true) => GazeDirection::Up,
            (true, true, _, _) => GazeDirection::DownRight,
            (true, false, _, _) => GazeDirection::UpRight,
            (false, true, _, _) => GazeDirection::DownLeft,
            (false, false, _, _) => GazeDirection::UpLeft,
        }
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer, theme: &Theme, accent: Color) {
        if !self.enabled || area.width < SPRITE_WIDTH || area.height < TERMINAL_ROWS {
            return;
        }

        let palette = Palette::new(theme, accent);
        let frame = &self.frames[self.current_gaze.as_index()];
        let bounce = if self.breathe_phase.sin() > BREATHE_THRESHOLD {
            1
        } else {
            0
        };

        let sprite_x = area.x + (area.width - SPRITE_WIDTH) / 2;
        let sprite_y = area.y + 1 + bounce;

        for (row_idx, row) in frame.iter().enumerate() {
            let y = sprite_y + row_idx as u16;
            if y >= area.y + area.height {
                continue;
            }
            for (col_idx, (top, bottom)) in row.iter().enumerate() {
                let x = sprite_x + col_idx as u16;
                if x >= area.x + area.width {
                    continue;
                }

                let top_color = palette.color(*top);
                let bottom_color = palette.color(*bottom);
                if top_color.is_none() && bottom_color.is_none() {
                    continue;
                }

                if let Some(cell) = buf.cell_mut((x, y)) {
                    let existing_bg = cell.bg;
                    let (symbol, fg, bg) = match (top_color, bottom_color) {
                        (Some(t), None) => ('▀', t, existing_bg),
                        (None, Some(b)) => ('▄', b, existing_bg),
                        (Some(t), Some(b)) if t == b => ('█', t, existing_bg),
                        (Some(t), Some(b)) => ('▀', t, b),
                        (None, None) => continue,
                    };
                    cell.set_char(symbol).set_fg(fg).set_bg(bg);
                }
            }
        }
    }

    pub fn is_animating(&self) -> bool {
        if !self.enabled {
            return false;
        }
        self.is_blinking || self.current_gaze != self.target_gaze
    }
}

impl Palette {
    fn new(theme: &Theme, accent: Color) -> Self {
        let (bg_r, bg_g, bg_b) = extract_rgb(theme.background, (15, 15, 25));
        let luma = 0.299 * f32::from(bg_r) + 0.587 * f32::from(bg_g) + 0.114 * f32::from(bg_b);
        let dark = luma < 100.0;

        let skin_target = if dark {
            (251, 211, 195)
        } else {
            (192, 136, 120)
        };
        let hair_target = if dark {
            (250, 248, 245)
        } else {
            (120, 120, 115)
        };
        let pupil_target = if dark { (40, 40, 60) } else { (20, 20, 30) };
        let blush_target = if dark {
            (255, 176, 192)
        } else {
            (224, 120, 150)
        };
        let outline_target = if dark { (80, 60, 70) } else { (60, 40, 50) };

        Self {
            skin: Color::Rgb(
                lerp_u8(bg_r, skin_target.0, 0.85),
                lerp_u8(bg_g, skin_target.1, 0.85),
                lerp_u8(bg_b, skin_target.2, 0.85),
            ),
            hair: Color::Rgb(
                lerp_u8(bg_r, hair_target.0, 0.9),
                lerp_u8(bg_g, hair_target.1, 0.9),
                lerp_u8(bg_b, hair_target.2, 0.9),
            ),
            eye: accent,
            pupil: Color::Rgb(
                lerp_u8(bg_r, pupil_target.0, 0.8),
                lerp_u8(bg_g, pupil_target.1, 0.8),
                lerp_u8(bg_b, pupil_target.2, 0.8),
            ),
            highlight: Color::Rgb(
                lerp_u8(bg_r, 255, 0.95),
                lerp_u8(bg_g, 255, 0.95),
                lerp_u8(bg_b, 255, 0.95),
            ),
            blush: Color::Rgb(
                lerp_u8(bg_r, blush_target.0, 0.4),
                lerp_u8(bg_g, blush_target.1, 0.4),
                lerp_u8(bg_b, blush_target.2, 0.4),
            ),
            bow: accent,
            outline: Color::Rgb(
                lerp_u8(bg_r, outline_target.0, 0.8),
                lerp_u8(bg_g, outline_target.1, 0.8),
                lerp_u8(bg_b, outline_target.2, 0.8),
            ),
        }
    }

    fn color(&self, role: PixelRole) -> Option<Color> {
        match role {
            PixelRole::Transparent => None,
            PixelRole::Skin => Some(self.skin),
            PixelRole::Hair => Some(self.hair),
            PixelRole::Eye => Some(self.eye),
            PixelRole::Pupil => Some(self.pupil),
            PixelRole::Highlight => Some(self.highlight),
            PixelRole::Blush => Some(self.blush),
            PixelRole::Bow => Some(self.bow),
            PixelRole::Outline => Some(self.outline),
        }
    }
}

fn build_frames() -> Vec<Frame> {
    let mut frames = Vec::with_capacity(EYES.len());

    for (left_top, left_bottom, right_top, right_bottom) in EYES {
        let eye_top = format!("kHS{left_top}SS{right_top}SHk");
        let eye_bottom = format!("kHS{left_bottom}SS{right_bottom}SHk");

        let half_rows = [
            parse_line(FIXED_ROWS[0]),
            parse_line(FIXED_ROWS[1]),
            parse_line(FIXED_ROWS[2]),
            parse_line(&eye_top),
            parse_line(&eye_bottom),
            parse_line(FIXED_ROWS[3]),
            parse_line(FIXED_ROWS[4]),
            parse_line(FIXED_ROWS[5]),
        ];

        let mut rows = Vec::with_capacity(half_rows.len() / 2);
        for pair in half_rows.chunks_exact(2) {
            let top = &pair[0];
            let bottom = &pair[1];
            let mut row = Vec::with_capacity(top.len());
            for (t, b) in top.iter().zip(bottom.iter()) {
                row.push((*t, *b));
            }
            rows.push(row);
        }
        frames.push(rows);
    }

    frames
}

fn parse_line(s: &str) -> Vec<PixelRole> {
    s.chars().map(char_to_role).collect()
}

fn char_to_role(c: char) -> PixelRole {
    match c {
        '.' => PixelRole::Transparent,
        'S' => PixelRole::Skin,
        'H' => PixelRole::Hair,
        'E' => PixelRole::Eye,
        'P' => PixelRole::Pupil,
        'W' => PixelRole::Highlight,
        'B' => PixelRole::Blush,
        'O' => PixelRole::Bow,
        'k' | 'K' => PixelRole::Outline,
        _ => PixelRole::Transparent,
    }
}

fn random_blink_interval() -> u64 {
    let mut rng = [0u8; 4];
    getrandom::fill(&mut rng).ok();
    let range = (BLINK_INTERVAL_MAX_MS - BLINK_INTERVAL_MIN_MS) as u32;
    BLINK_INTERVAL_MIN_MS + (u32::from_le_bytes(rng) % range) as u64
}

fn extract_rgb(color: Color, fallback: (u8, u8, u8)) -> (u8, u8, u8) {
    match color {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => fallback,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::theme;

    fn accent() -> Color {
        Color::Rgb(100, 140, 255)
    }

    #[test]
    fn gaze_direction_mapping() {
        let mascot = Mascot::new(true);
        let area = Rect::new(0, 0, 40, 20);

        assert_eq!(
            mascot.compute_target_gaze(20, 3, area),
            GazeDirection::Straight
        );
        assert_eq!(
            mascot.compute_target_gaze(40, 3, area),
            GazeDirection::Right
        );
        assert_eq!(mascot.compute_target_gaze(0, 3, area), GazeDirection::Left);
        assert_eq!(mascot.compute_target_gaze(20, 0, area), GazeDirection::Up);
        assert_eq!(
            mascot.compute_target_gaze(20, 20, area),
            GazeDirection::Down
        );
        assert_eq!(
            mascot.compute_target_gaze(16, 1, area),
            GazeDirection::UpLeft
        );
        assert_eq!(
            mascot.compute_target_gaze(24, 1, area),
            GazeDirection::UpRight
        );
        assert_eq!(
            mascot.compute_target_gaze(10, 20, area),
            GazeDirection::DownLeft
        );
        assert_eq!(
            mascot.compute_target_gaze(35, 20, area),
            GazeDirection::DownRight
        );
    }

    #[test]
    fn render_bounds_empty_area() {
        let mascot = Mascot::new(true);
        let area = Rect::new(0, 0, 0, 0);
        let mut buf = Buffer::empty(area);
        let theme = theme::current();
        mascot.render(area, &mut buf, &theme, accent());
    }

    #[test]
    fn render_bounds_small_area() {
        let mascot = Mascot::new(true);
        let area = Rect::new(0, 0, 5, 3);
        let mut buf = Buffer::empty(area);
        let theme = theme::current();
        mascot.render(area, &mut buf, &theme, accent());
    }

    #[test]
    fn render_sufficient_area() {
        let mascot = Mascot::new(true);
        let area = Rect::new(0, 0, 40, 20);
        let mut buf = Buffer::empty(area);
        let theme = theme::current();
        mascot.render(area, &mut buf, &theme, accent());
    }

    #[test]
    fn enabled_flag() {
        let enabled = Mascot::new(true);
        assert!(
            enabled.is_animating()
                == (enabled.is_blinking || enabled.current_gaze != enabled.target_gaze)
        );

        let disabled = Mascot::new(false);
        assert!(!disabled.is_animating());
    }

    #[test]
    fn blink_timing_progression() {
        let mut mascot = Mascot::new(true);
        mascot.next_blink_interval = 100;
        mascot.last_blink = Instant::now() - Duration::from_millis(150);
        mascot.tick(Rect::new(0, 0, 40, 20));
        assert!(mascot.is_blinking);

        mascot.blink_start = Some(Instant::now() - Duration::from_millis(200));
        mascot.tick(Rect::new(0, 0, 40, 20));
        assert!(!mascot.is_blinking);
    }

    #[test]
    fn mouse_ignored_when_disabled() {
        let mut mascot = Mascot::new(false);
        mascot.on_mouse(50, 20);
        assert!(mascot.mouse_col.is_none());
    }

    #[test]
    fn gaze_transition_respects_timer() {
        let mut mascot = Mascot::new(true);
        let area = Rect::new(0, 0, 40, 20);
        mascot.on_mouse(40, 10);
        mascot.tick(area);
        assert_eq!(mascot.target_gaze, GazeDirection::Right);
        assert_eq!(mascot.current_gaze, GazeDirection::Straight);
    }

    #[test]
    fn frames_are_consistent_width() {
        let mascot = Mascot::new(true);
        for frame in &mascot.frames {
            let width = frame.first().map_or(0, |row| row.len());
            assert!(frame.iter().all(|row| row.len() == width));
            assert_eq!(width, SPRITE_WIDTH as usize);
        }
    }
}
