use std::time::Instant;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

use crate::theme::{Theme, lerp_u8};

const BASE_WIDTH: f64 = 80.0;
const BASE_HEIGHT: f64 = 50.0;
const GAZE_TRANSITION_MS: u128 = 48;
const BLINK_INTERVAL_MIN_MS: u64 = 2500;
const BLINK_INTERVAL_MAX_MS: u64 = 4000;
const BLINK_DURATION_MS: u128 = 120;
const BREATHE_PERIOD_S: f32 = 2.0;
const BREATHE_THRESHOLD: f32 = 0.0;
const BRAILLE_COLS: usize = 2;
const BRAILLE_ROWS: usize = 4;
const SAMPLES_PER_CELL: usize = BRAILLE_COLS * BRAILLE_ROWS;

const BRAILLE_BASE: u32 = 0x2800;

pub struct Mascot {
    enabled: bool,
    mouse_col: Option<u16>,
    mouse_row: Option<u16>,
    gaze_x: f64,
    gaze_y: f64,
    target_gaze_x: f64,
    target_gaze_y: f64,
    gaze_start: Option<Instant>,
    gaze_from_x: f64,
    gaze_from_y: f64,
    last_blink: Instant,
    next_blink_interval: u64,
    is_blinking: bool,
    blink_start: Option<Instant>,
    breathe_phase: f32,
    last_tick: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[allow(dead_code)]
enum Layer {
    None,
    HairBack,
    HairLeft,
    HairRight,
    EarLeft,
    EarRight,
    Face,
    InnerEarLeft,
    InnerEarRight,
    Bangs,
    SideHairLeft,
    SideHairRight,
    Collar,
    BlushLeft,
    BlushRight,
    Ribbon,
    BrowLeft,
    BrowRight,
    EyeWhiteLeft,
    EyeWhiteRight,
    IrisLeft,
    IrisRight,
    LashLeft,
    LashRight,
    Nose,
    Mouth,
    EyelidLeft,
    EyelidRight,
    PupilLeft,
    PupilRight,
    HighlightLeft,
    HighlightRight,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
enum Role {
    Background,
    Hair,
    Skin,
    Blush,
    Collar,
    Ribbon,
    Mouth,
    Nose,
    EyeWhite,
    Eye,
    Brow,
    Lash,
    Pupil,
    Highlight,
}

struct Palette {
    background: Color,
    hair: Color,
    skin: Color,
    blush: Color,
    collar: Color,
    ribbon: Color,
    mouth: Color,
    nose: Color,
    eye_white: Color,
    eye: Color,
    brow: Color,
    lash: Color,
    pupil: Color,
    highlight: Color,
}

impl Mascot {
    pub fn new(enabled: bool) -> Self {
        let now = Instant::now();
        Self {
            enabled,
            mouse_col: None,
            mouse_row: None,
            gaze_x: 0.0,
            gaze_y: 0.0,
            target_gaze_x: 0.0,
            target_gaze_y: 0.0,
            gaze_start: None,
            gaze_from_x: 0.0,
            gaze_from_y: 0.0,
            last_blink: now,
            next_blink_interval: random_blink_interval(),
            is_blinking: false,
            blink_start: None,
            breathe_phase: 0.0,
            last_tick: now,
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
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
        let delta = now.saturating_duration_since(self.last_tick);
        self.last_tick = now;

        self.breathe_phase = (self.breathe_phase
            + delta.as_secs_f32() * std::f32::consts::TAU / BREATHE_PERIOD_S)
            % std::f32::consts::TAU;

        if let (Some(col), Some(row)) = (self.mouse_col, self.mouse_row) {
            let (tx, ty) = self.compute_target_gaze(col, row, area);
            if (tx - self.target_gaze_x).abs() > 1e-3 || (ty - self.target_gaze_y).abs() > 1e-3 {
                self.target_gaze_x = tx;
                self.target_gaze_y = ty;
                self.gaze_start = Some(now);
                self.gaze_from_x = self.gaze_x;
                self.gaze_from_y = self.gaze_y;
            }
        }

        if let Some(start) = self.gaze_start {
            let elapsed = now.saturating_duration_since(start).as_millis();
            let t = (elapsed as f64 / GAZE_TRANSITION_MS as f64).clamp(0.0, 1.0);
            self.gaze_x = self.gaze_from_x + (self.target_gaze_x - self.gaze_from_x) * t;
            self.gaze_y = self.gaze_from_y + (self.target_gaze_y - self.gaze_from_y) * t;
            if t >= 1.0 {
                self.gaze_start = None;
            }
        }

        if self.is_blinking {
            if let Some(start) = self.blink_start
                && now.saturating_duration_since(start).as_millis() >= BLINK_DURATION_MS
            {
                self.is_blinking = false;
                self.blink_start = None;
                self.last_blink = now;
                self.next_blink_interval = random_blink_interval();
            }
        } else if now.saturating_duration_since(self.last_blink).as_millis()
            >= self.next_blink_interval as u128
        {
            self.is_blinking = true;
            self.blink_start = Some(now);
        }
    }

    fn compute_target_gaze(&self, col: u16, row: u16, area: Rect) -> (f64, f64) {
        let scale = (f64::from(area.width) / BASE_WIDTH).min(f64::from(area.height) / BASE_HEIGHT);
        let center_x = f64::from(area.x) + f64::from(area.width) / 2.0;
        let center_y = f64::from(area.y) + f64::from(area.height) / 2.0;

        let dx = (f64::from(col) - center_x) / scale * 0.05;
        let dy = (f64::from(row) - center_y) / scale * 0.05;
        (dx.clamp(-2.0, 2.0), dy.clamp(-2.0, 2.0))
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer, theme: &Theme, _accent: Color) {
        if !self.enabled || area.width < 24 || area.height < 12 {
            return;
        }

        let palette = Palette::new(theme);
        let scale = (f64::from(area.width) / BASE_WIDTH).min(f64::from(area.height) / BASE_HEIGHT);
        let draw_w = BASE_WIDTH * scale;
        let draw_h = BASE_HEIGHT * scale;
        let off_x = f64::from(area.x) + (f64::from(area.width) - draw_w) / 2.0;
        let off_y = f64::from(area.y) + (f64::from(area.height) - draw_h) / 2.0
            - if self.breathe_phase.sin() > BREATHE_THRESHOLD {
                1.0
            } else {
                0.0
            };

        let inv = 1.0 / scale;

        for ty in area.y..area.y + area.height {
            for tx in area.x..area.x + area.width {
                let mut counts = [(Layer::None, 0u8); SAMPLES_PER_CELL];
                let mut n = 0;
                for dy in 0..BRAILLE_ROWS {
                    for dx in 0..BRAILLE_COLS {
                        let bx =
                            (f64::from(tx) - off_x + (dx as f64 + 0.5) / BRAILLE_COLS as f64) * inv;
                        let by =
                            (f64::from(ty) - off_y + (dy as f64 + 0.5) / BRAILLE_ROWS as f64) * inv;
                        let layer =
                            sample_layer(bx, by, self.gaze_x, self.gaze_y, self.is_blinking);
                        if let Some(pos) = counts[..n].iter().position(|(l, _)| *l == layer) {
                            counts[pos].1 += 1;
                        } else {
                            counts[n] = (layer, 1);
                            n += 1;
                        }
                    }
                }

                counts[..n].sort_by(|(a, _), (b, _)| b.cmp(a));
                let (fg_layer, _) = counts[0];
                let bg_layer = if n >= 2 { counts[1].0 } else { fg_layer };

                if fg_layer == Layer::None && bg_layer == Layer::None {
                    continue;
                }

                let bg = palette.color(bg_layer.role());
                if fg_layer == bg_layer {
                    if let Some(cell) = buf.cell_mut((tx, ty)) {
                        cell.set_char(' ').set_bg(bg);
                    }
                    continue;
                }

                let fg = palette.color(fg_layer.role());
                let mut mask: u8 = 0;

                let sample = |dy: usize, dx: usize| {
                    let bx =
                        (f64::from(tx) - off_x + (dx as f64 + 0.5) / BRAILLE_COLS as f64) * inv;
                    let by =
                        (f64::from(ty) - off_y + (dy as f64 + 0.5) / BRAILLE_ROWS as f64) * inv;
                    sample_layer(bx, by, self.gaze_x, self.gaze_y, self.is_blinking)
                };

                for dy in 0..BRAILLE_ROWS {
                    for dx in 0..BRAILLE_COLS {
                        if sample(dy, dx) == fg_layer {
                            mask |= braille_bit(dy, dx);
                        }
                    }
                }

                let ch = char::from_u32(BRAILLE_BASE + u32::from(mask)).unwrap_or(' ');
                if let Some(cell) = buf.cell_mut((tx, ty)) {
                    cell.set_char(ch).set_fg(fg).set_bg(bg);
                }
            }
        }
    }

    pub fn is_animating(&self) -> bool {
        if !self.enabled {
            return false;
        }
        self.is_blinking || self.gaze_start.is_some()
    }
}

fn braille_bit(dy: usize, dx: usize) -> u8 {
    match (dy, dx) {
        (0, 0) => 1 << 0,
        (1, 0) => 1 << 1,
        (2, 0) => 1 << 2,
        (0, 1) => 1 << 3,
        (1, 1) => 1 << 4,
        (2, 1) => 1 << 5,
        (3, 0) => 1 << 6,
        (3, 1) => 1 << 7,
        _ => 0,
    }
}

impl Layer {
    fn role(self) -> Role {
        match self {
            Layer::None => Role::Background,
            Layer::HairBack
            | Layer::HairLeft
            | Layer::HairRight
            | Layer::EarLeft
            | Layer::EarRight
            | Layer::Bangs
            | Layer::SideHairLeft
            | Layer::SideHairRight
            | Layer::BrowLeft
            | Layer::BrowRight => Role::Hair,
            Layer::Face | Layer::InnerEarLeft | Layer::InnerEarRight => Role::Skin,
            Layer::BlushLeft | Layer::BlushRight => Role::Blush,
            Layer::Collar => Role::Collar,
            Layer::Ribbon => Role::Ribbon,
            Layer::EyeWhiteLeft | Layer::EyeWhiteRight => Role::EyeWhite,
            Layer::IrisLeft | Layer::IrisRight => Role::Eye,
            Layer::LashLeft | Layer::LashRight => Role::Lash,
            Layer::Nose => Role::Nose,
            Layer::Mouth | Layer::EyelidLeft | Layer::EyelidRight => Role::Mouth,
            Layer::PupilLeft | Layer::PupilRight => Role::Pupil,
            Layer::HighlightLeft | Layer::HighlightRight => Role::Highlight,
        }
    }
}

impl Palette {
    fn new(theme: &Theme) -> Self {
        let (bg_r, bg_g, bg_b) = extract_rgb(theme.background, (20, 20, 28));
        let luma = 0.299 * f32::from(bg_r) + 0.587 * f32::from(bg_g) + 0.114 * f32::from(bg_b);
        let dark = luma < 100.0;

        let hair_target = if dark {
            (245, 210, 215)
        } else {
            (190, 140, 145)
        };
        let skin_target = if dark {
            (255, 224, 210)
        } else {
            (210, 160, 140)
        };
        let eye_target = if dark { (90, 180, 220) } else { (70, 140, 190) };
        let pupil_target = if dark { (40, 40, 55) } else { (30, 30, 40) };
        let blush_target = if dark {
            (255, 165, 175)
        } else {
            (230, 130, 145)
        };
        let nose_target = (255, 150, 160);
        let mouth_target = if dark { (185, 90, 110) } else { (160, 70, 90) };
        let brow_target = if dark {
            (180, 140, 140)
        } else {
            (140, 100, 100)
        };
        let lash_target = (60, 50, 55);
        let collar_target = if dark {
            (140, 180, 255)
        } else {
            (110, 150, 220)
        };
        let ribbon_target = (255, 120, 160);

        Self {
            background: Color::Rgb(bg_r, bg_g, bg_b),
            hair: Color::Rgb(
                lerp_u8(bg_r, hair_target.0, 0.95),
                lerp_u8(bg_g, hair_target.1, 0.95),
                lerp_u8(bg_b, hair_target.2, 0.95),
            ),
            skin: Color::Rgb(
                lerp_u8(bg_r, skin_target.0, 0.9),
                lerp_u8(bg_g, skin_target.1, 0.9),
                lerp_u8(bg_b, skin_target.2, 0.9),
            ),
            blush: Color::Rgb(
                lerp_u8(bg_r, blush_target.0, 0.5),
                lerp_u8(bg_g, blush_target.1, 0.5),
                lerp_u8(bg_b, blush_target.2, 0.5),
            ),
            collar: Color::Rgb(
                lerp_u8(bg_r, collar_target.0, 0.85),
                lerp_u8(bg_g, collar_target.1, 0.85),
                lerp_u8(bg_b, collar_target.2, 0.85),
            ),
            ribbon: Color::Rgb(
                lerp_u8(bg_r, ribbon_target.0, 0.65),
                lerp_u8(bg_g, ribbon_target.1, 0.65),
                lerp_u8(bg_b, ribbon_target.2, 0.65),
            ),
            mouth: Color::Rgb(
                lerp_u8(bg_r, mouth_target.0, 0.8),
                lerp_u8(bg_g, mouth_target.1, 0.8),
                lerp_u8(bg_b, mouth_target.2, 0.8),
            ),
            nose: Color::Rgb(
                lerp_u8(bg_r, nose_target.0, 0.6),
                lerp_u8(bg_g, nose_target.1, 0.6),
                lerp_u8(bg_b, nose_target.2, 0.6),
            ),
            eye_white: Color::Rgb(
                lerp_u8(bg_r, 250, 0.98),
                lerp_u8(bg_g, 250, 0.98),
                lerp_u8(bg_b, 250, 0.98),
            ),
            eye: Color::Rgb(
                lerp_u8(bg_r, eye_target.0, 0.9),
                lerp_u8(bg_g, eye_target.1, 0.9),
                lerp_u8(bg_b, eye_target.2, 0.9),
            ),
            brow: Color::Rgb(
                lerp_u8(bg_r, brow_target.0, 0.85),
                lerp_u8(bg_g, brow_target.1, 0.85),
                lerp_u8(bg_b, brow_target.2, 0.85),
            ),
            lash: Color::Rgb(
                lerp_u8(bg_r, lash_target.0, 0.8),
                lerp_u8(bg_g, lash_target.1, 0.8),
                lerp_u8(bg_b, lash_target.2, 0.8),
            ),
            pupil: Color::Rgb(
                lerp_u8(bg_r, pupil_target.0, 0.8),
                lerp_u8(bg_g, pupil_target.1, 0.8),
                lerp_u8(bg_b, pupil_target.2, 0.8),
            ),
            highlight: Color::Rgb(
                lerp_u8(bg_r, 255, 0.99),
                lerp_u8(bg_g, 255, 0.99),
                lerp_u8(bg_b, 255, 0.99),
            ),
        }
    }

    fn color(&self, role: Role) -> Color {
        match role {
            Role::Background => self.background,
            Role::Hair => self.hair,
            Role::Skin => self.skin,
            Role::Blush => self.blush,
            Role::Collar => self.collar,
            Role::Ribbon => self.ribbon,
            Role::Mouth => self.mouth,
            Role::Nose => self.nose,
            Role::EyeWhite => self.eye_white,
            Role::Eye => self.eye,
            Role::Brow => self.brow,
            Role::Lash => self.lash,
            Role::Pupil => self.pupil,
            Role::Highlight => self.highlight,
        }
    }
}

#[allow(clippy::too_many_lines)]
fn sample_layer(x: f64, y: f64, gaze_x: f64, gaze_y: f64, blink: bool) -> Layer {
    let mut layer = Layer::None;

    // long wavy side hair
    for t in 0..=10 {
        let tt = f64::from(t) / 10.0;
        let xx = 18.0 - 5.0 * tt + 2.0 * (tt * 4.0).sin();
        let yy = 20.0 + 25.0 * tt;
        let r = 6.5 - 0.25 * f64::from(t);
        if circle_contains(x, y, xx, yy, r) {
            layer = Layer::HairLeft;
        }
    }
    for t in 0..=10 {
        let tt = f64::from(t) / 10.0;
        let xx = 62.0 + 5.0 * tt - 2.0 * (tt * 4.0).sin();
        let yy = 20.0 + 25.0 * tt;
        let r = 6.5 - 0.25 * f64::from(t);
        if circle_contains(x, y, xx, yy, r) {
            layer = Layer::HairRight;
        }
    }

    if ellipse_contains(x, y, 40.0, 32.0, 23.0, 24.0) {
        layer = Layer::HairBack;
    }

    if triangle_contains(x, y, 25.0, 7.0, 33.5, 19.0, 18.0, 19.0) {
        layer = Layer::EarLeft;
    }
    if triangle_contains(x, y, 55.0, 7.0, 46.5, 19.0, 62.0, 19.0) {
        layer = Layer::EarRight;
    }

    if triangle_contains(x, y, 26.0, 10.5, 32.0, 18.0, 20.5, 18.0) {
        layer = Layer::InnerEarLeft;
    }
    if triangle_contains(x, y, 54.0, 10.5, 48.0, 18.0, 59.5, 18.0) {
        layer = Layer::InnerEarRight;
    }

    if ellipse_contains(x, y, 40.0, 31.0, 15.0, 14.5) {
        layer = Layer::Face;
    }
    if triangle_contains(x, y, 40.0, 44.0, 34.5, 36.0, 45.5, 36.0) {
        layer = Layer::Face;
    }

    for cx in [25.0_f64, 33.0, 40.0, 47.0, 55.0] {
        if ellipse_contains(x, y, cx, 19.0, 5.0, 4.0) {
            layer = Layer::Bangs;
        }
    }

    if ellipse_contains(x, y, 22.0, 30.0, 4.0, 12.0) {
        layer = Layer::SideHairLeft;
    }
    if ellipse_contains(x, y, 58.0, 30.0, 4.0, 12.0) {
        layer = Layer::SideHairRight;
    }

    if ellipse_contains(x, y, 40.0, 46.0, 13.0, 2.4) {
        layer = Layer::Collar;
    }

    if ellipse_contains(x, y, 23.0, 34.5, 3.0, 1.8) {
        layer = Layer::BlushLeft;
    }
    if ellipse_contains(x, y, 57.0, 34.5, 3.0, 1.8) {
        layer = Layer::BlushRight;
    }

    if ellipse_contains(x, y, 15.0, 24.0, 3.5, 2.5)
        || ellipse_contains(x, y, 21.0, 24.0, 3.5, 2.5)
        || circle_contains(x, y, 18.0, 24.0, 1.8)
        || ellipse_contains(x, y, 36.0, 48.0, 3.0, 2.0)
        || ellipse_contains(x, y, 44.0, 48.0, 3.0, 2.0)
        || circle_contains(x, y, 40.0, 48.0, 1.6)
    {
        layer = Layer::Ribbon;
    }

    if segment_contains(x, y, 25.0, 22.0, 34.0, 21.0, 0.45) {
        layer = Layer::BrowLeft;
    }
    if segment_contains(x, y, 46.0, 21.0, 55.0, 22.0, 0.45) {
        layer = Layer::BrowRight;
    }

    if blink {
        if ellipse_contains(x, y, 30.0, 29.0, 5.5, 6.5) {
            layer = Layer::EyeWhiteLeft;
        }
        if ellipse_contains(x, y, 50.0, 29.0, 5.5, 6.5) {
            layer = Layer::EyeWhiteRight;
        }
        if ellipse_contains(x, y, 30.0, 29.0, 5.5, 6.5) && (y - 31.0).abs() <= 0.6 {
            layer = Layer::EyelidLeft;
        }
        if ellipse_contains(x, y, 50.0, 29.0, 5.5, 6.5) && (y - 31.0).abs() <= 0.6 {
            layer = Layer::EyelidRight;
        }
    } else {
        if ellipse_contains(x, y, 30.0 + gaze_x, 29.0 + gaze_y, 5.5, 6.5) {
            layer = Layer::EyeWhiteLeft;
        }
        if ellipse_contains(x, y, 50.0 + gaze_x, 29.0 + gaze_y, 5.5, 6.5) {
            layer = Layer::EyeWhiteRight;
        }
        if ellipse_contains(x, y, 30.0 + gaze_x * 1.1, 30.0 + gaze_y, 4.2, 5.3) {
            layer = Layer::IrisLeft;
        }
        if ellipse_contains(x, y, 50.0 + gaze_x * 1.1, 30.0 + gaze_y, 4.2, 5.3) {
            layer = Layer::IrisRight;
        }
        if ellipse_contains(x, y, 30.0 + gaze_x * 1.2, 31.0 + gaze_y, 2.2, 3.6) {
            layer = Layer::PupilLeft;
        }
        if ellipse_contains(x, y, 50.0 + gaze_x * 1.2, 31.0 + gaze_y, 2.2, 3.6) {
            layer = Layer::PupilRight;
        }
        if circle_contains(x, y, 28.0 + gaze_x * 0.3, 27.0 + gaze_y * 0.3, 1.4) {
            layer = Layer::HighlightLeft;
        }
        if circle_contains(x, y, 48.0 + gaze_x * 0.3, 27.0 + gaze_y * 0.3, 1.4) {
            layer = Layer::HighlightRight;
        }
        if segment_contains(
            x,
            y,
            24.5 + gaze_x,
            25.0 + gaze_y,
            35.5 + gaze_x,
            25.0 + gaze_y,
            0.5,
        ) {
            layer = Layer::LashLeft;
        }
        if segment_contains(
            x,
            y,
            44.5 + gaze_x,
            25.0 + gaze_y,
            55.5 + gaze_x,
            25.0 + gaze_y,
            0.5,
        ) {
            layer = Layer::LashRight;
        }
    }

    if triangle_contains(x, y, 40.0, 36.0, 38.5, 38.5, 41.5, 38.5) {
        layer = Layer::Nose;
    }
    if ellipse_contains(x, y, 40.0, 41.0, 3.5, 1.2) {
        layer = Layer::Mouth;
    }

    layer
}

fn ellipse_contains(x: f64, y: f64, cx: f64, cy: f64, rx: f64, ry: f64) -> bool {
    ((x - cx) / rx).powi(2) + ((y - cy) / ry).powi(2) <= 1.0
}

fn circle_contains(x: f64, y: f64, cx: f64, cy: f64, r: f64) -> bool {
    (x - cx).powi(2) + (y - cy).powi(2) <= r * r
}

#[allow(clippy::too_many_arguments)]
fn triangle_contains(
    px: f64,
    py: f64,
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
    x3: f64,
    y3: f64,
) -> bool {
    let denom = (y2 - y3) * (x1 - x3) + (x3 - x2) * (y1 - y3);
    if denom.abs() < f64::EPSILON {
        return false;
    }
    let a = ((y2 - y3) * (px - x3) + (x3 - x2) * (py - y3)) / denom;
    let b = ((y3 - y1) * (px - x3) + (x1 - x3) * (py - y3)) / denom;
    a >= 0.0 && b >= 0.0 && a + b <= 1.0
}

fn segment_contains(px: f64, py: f64, x1: f64, y1: f64, x2: f64, y2: f64, thickness: f64) -> bool {
    let vx = x2 - x1;
    let vy = y2 - y1;
    let wx = px - x1;
    let wy = py - y1;
    let c1 = wx * vx + wy * vy;
    if c1 <= 0.0 {
        return (px - x1).powi(2) + (py - y1).powi(2) <= thickness * thickness;
    }
    let c2 = vx * vx + vy * vy;
    if c1 >= c2 {
        return (px - x2).powi(2) + (py - y2).powi(2) <= thickness * thickness;
    }
    let t = c1 / c2;
    let dx = px - (x1 + t * vx);
    let dy = py - (y1 + t * vy);
    dx * dx + dy * dy <= thickness * thickness
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
        Color::Black => (0, 0, 0),
        Color::Red => (205, 49, 49),
        Color::Green => (13, 188, 121),
        Color::Yellow => (229, 229, 16),
        Color::Blue => (36, 114, 200),
        Color::Magenta => (188, 63, 188),
        Color::Cyan => (17, 168, 161),
        Color::Gray => (128, 128, 128),
        Color::DarkGray => (85, 85, 85),
        Color::LightRed => (255, 85, 85),
        Color::LightGreen => (85, 255, 85),
        Color::LightYellow => (255, 255, 85),
        Color::LightBlue => (85, 85, 255),
        Color::LightMagenta => (255, 85, 255),
        Color::LightCyan => (85, 255, 255),
        Color::White => (255, 255, 255),
        _ => fallback,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::theme;

    fn accent() -> Color {
        Color::Rgb(120, 160, 255)
    }

    #[test]
    fn render_does_not_panic_in_empty_area() {
        let mascot = Mascot::new(true);
        let area = Rect::new(0, 0, 0, 0);
        let mut buf = Buffer::empty(area);
        let theme = theme::current();
        mascot.render(area, &mut buf, &theme, accent());
    }

    #[test]
    fn render_does_not_panic_in_small_area() {
        let mascot = Mascot::new(true);
        let area = Rect::new(0, 0, 5, 3);
        let mut buf = Buffer::empty(area);
        let theme = theme::current();
        mascot.render(area, &mut buf, &theme, accent());
    }

    #[test]
    fn render_fills_large_area() {
        let mascot = Mascot::new(true);
        let area = Rect::new(0, 0, 80, 40);
        let mut buf = Buffer::empty(area);
        let theme = theme::current();
        mascot.render(area, &mut buf, &theme, accent());

        let non_empty = buf.content.iter().filter(|c| c.symbol() != " ").count();
        assert!(non_empty > 100);
    }

    #[test]
    fn enabled_flag() {
        let enabled = Mascot::new(true);
        assert!(!enabled.is_animating());

        let disabled = Mascot::new(false);
        assert!(!disabled.is_animating());
    }

    #[test]
    fn mouse_ignored_when_disabled() {
        let mut mascot = Mascot::new(false);
        mascot.on_mouse(50, 20);
        assert!(mascot.mouse_col.is_none());
    }

    #[test]
    fn blink_timing_progression() {
        let mut mascot = Mascot::new(true);
        mascot.next_blink_interval = 100;
        mascot.last_blink = Instant::now() - Duration::from_millis(150);
        mascot.tick(Rect::new(0, 0, 80, 40));
        assert!(mascot.is_blinking);

        mascot.blink_start = Some(Instant::now() - Duration::from_millis(200));
        mascot.tick(Rect::new(0, 0, 80, 40));
        assert!(!mascot.is_blinking);
    }

    #[test]
    fn gaze_target_follows_mouse() {
        let mut mascot = Mascot::new(true);
        let area = Rect::new(0, 0, 80, 40);
        mascot.on_mouse(80, 20);
        mascot.tick(area);
        assert!(mascot.target_gaze_x > 0.0);
    }

    #[test]
    fn gaze_transition_is_smooth() {
        let mut mascot = Mascot::new(true);
        let area = Rect::new(0, 0, 80, 40);
        mascot.on_mouse(80, 20);
        mascot.tick(area);
        assert!((mascot.gaze_x - mascot.target_gaze_x).abs() > 1e-3);
    }

    #[test]
    #[ignore = "visual dump only"]
    fn visual_dump() {
        let mascot = Mascot::new(true);
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
