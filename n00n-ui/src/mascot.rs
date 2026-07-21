use std::io::IsTerminal;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use image::{DynamicImage, Rgba, RgbaImage};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::layout::Size;
use ratatui::style::Color;
use ratatui::widgets::Widget;
use ratatui_image::{
    Image, Resize,
    picker::{Picker, ProtocolType},
    protocol::Protocol as ImageProtocol,
};

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
const SUBPX_DX: [f64; 2] = [0.25, 0.75];
const SUBPX_DY: [f64; 4] = [0.125, 0.375, 0.625, 0.875];
const BAYER_THRESHOLDS: [f64; 8] = [
    0.0625, 0.5625, 0.3125, 0.8125, 0.1875, 0.6875, 0.4375, 0.9375,
];

const MASCOT_MIN_X: f64 = 10.0;
const MASCOT_MAX_X: f64 = 70.0;
const MASCOT_MIN_Y: f64 = 5.0;
const MASCOT_MAX_Y: f64 = 55.0;

const HAIR_LEFT: [(f64, f64, f64); 11] = [
    (18.0000, 18.0, 6.5000),
    (18.0650, 20.5, 6.2500),
    (18.2600, 23.0, 6.0000),
    (18.5850, 25.5, 5.7500),
    (19.0400, 28.0, 5.5000),
    (19.6250, 30.5, 5.2500),
    (20.3400, 33.0, 5.0000),
    (21.1850, 35.5, 4.7500),
    (22.1600, 38.0, 4.5000),
    (23.2650, 40.5, 4.2500),
    (24.5000, 43.0, 4.0000),
];
const HAIR_RIGHT: [(f64, f64, f64); 11] = [
    (62.0000, 18.0, 6.5000),
    (61.9350, 20.5, 6.2500),
    (61.7400, 23.0, 6.0000),
    (61.4150, 25.5, 5.7500),
    (60.9600, 28.0, 5.5000),
    (60.3750, 30.5, 5.2500),
    (59.6600, 33.0, 5.0000),
    (58.8150, 35.5, 4.7500),
    (57.8400, 38.0, 4.5000),
    (56.7350, 40.5, 4.2500),
    (55.5000, 43.0, 4.0000),
];
const BANGS: [(f64, f64, f64, f64); 5] = [
    (25.0, 18.2, 5.0, 4.0),
    (33.0, 18.0, 4.5, 4.5),
    (40.5, 18.3, 5.0, 4.2),
    (48.0, 18.0, 4.5, 4.5),
    (55.0, 18.2, 5.0, 4.0),
];
const SIDE_HAIR: [(f64, f64, f64, f64); 2] = [(23.0, 31.0, 5.0, 15.5), (57.0, 31.0, 5.0, 15.5)];

static PICKER: OnceLock<Option<Arc<Picker>>> = OnceLock::new();

fn detect_picker() -> Option<Arc<Picker>> {
    if !std::io::stdout().is_terminal() || !std::io::stdin().is_terminal() {
        return None;
    }
    Picker::from_query_stdio()
        .ok()
        .map(|mut p| {
            // Kitty/Sixel are stateful and stack/flicker when the image changes every
            // frame (gaze/blink/breathe). Halfblocks is immediate and safe for animation.
            p.set_protocol_type(ProtocolType::Halfblocks);
            p
        })
        .map(Arc::new)
}

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
    picker: Option<Arc<Picker>>,
    base_image: Option<RgbaImage>,
    last_area: Option<Rect>,
    last_font_size: Option<(u16, u16)>,
    breathe_down: bool,
    protocol: Option<ImageProtocol>,
    last_gaze_x: f64,
    last_gaze_y: f64,
    last_blinking: bool,
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
    InnerEarLeft,
    InnerEarRight,
    Face,
    BlushLeft,
    BlushRight,
    Collar,
    EyeWhiteLeft,
    EyeWhiteRight,
    IrisLeft,
    IrisRight,
    PupilLeft,
    PupilRight,
    LashLeft,
    LashRight,
    HighlightLeft,
    HighlightRight,
    BrowLeft,
    BrowRight,
    Nose,
    Mouth,
    Teeth,
    SideHairLeft,
    SideHairRight,
    Bangs,
    Ribbon,
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
    Teeth,
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
    teeth: Color,
    nose: Color,
    eye_white: Color,
    eye: Color,
    brow: Color,
    lash: Color,
    pupil: Color,
    highlight: Color,
}

impl Mascot {
    pub(crate) fn init_picker() {
        let _ = PICKER.get_or_init(detect_picker);
    }

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
            picker: PICKER.get().cloned().unwrap_or(None),
            base_image: None,
            last_area: None,
            last_font_size: None,
            breathe_down: false,
            protocol: None,
            last_gaze_x: 0.0,
            last_gaze_y: 0.0,
            last_blinking: false,
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

    pub fn render(&mut self, area: Rect, buf: &mut Buffer, theme: &Theme, _accent: Color) {
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
        let current_breathe_down = self.breathe_phase.sin() > BREATHE_THRESHOLD;

        if self.picker.is_none() {
            self.render_braille(area, buf, &palette, off_x, off_y, inv);
            return;
        }

        let picker = self.picker.as_ref().unwrap();
        let font_size = picker.font_size();
        if font_size.width == 0 || font_size.height == 0 {
            self.render_braille(area, buf, &palette, off_x, off_y, inv);
            return;
        }
        let font_dims = (font_size.width, font_size.height);
        let px_width = u32::from(area.width) * u32::from(font_size.width);
        let px_height = u32::from(area.height) * u32::from(font_size.height);

        let base_changed = self.last_area != Some(area)
            || self.last_font_size != Some(font_dims)
            || self.breathe_down != current_breathe_down
            || self.base_image.is_none();
        let eyes_changed = self.gaze_x != self.last_gaze_x
            || self.gaze_y != self.last_gaze_y
            || self.is_blinking != self.last_blinking;

        if base_changed || eyes_changed {
            let mut image = if base_changed {
                let mut base = RgbaImage::new(px_width, px_height);

                for py in 0..px_height {
                    for px in 0..px_width {
                        let tx =
                            f64::from(area.x) + (f64::from(px) + 0.5) / f64::from(font_size.width);
                        let ty =
                            f64::from(area.y) + (f64::from(py) + 0.5) / f64::from(font_size.height);
                        let bx = (tx - off_x) * inv;
                        let by = (ty - off_y) * inv;
                        let (layer, shade) = sample(bx, by, 0.0, 0.0, true);
                        base.put_pixel(px, py, mascot_pixel(layer, shade, &palette, true));
                    }
                }

                self.base_image = Some(base.clone());
                self.last_area = Some(area);
                self.last_font_size = Some(font_dims);
                self.breathe_down = current_breathe_down;
                base
            } else {
                self.base_image.as_ref().unwrap().clone()
            };

            for &(ecx, ecy) in &[(30.0, 27.5), (50.0, 27.5)] {
                let tx = ecx * scale + off_x;
                let ty = ecy * scale + off_y;
                let px0 = (tx - f64::from(area.x)) * f64::from(font_size.width);
                let py0 = (ty - f64::from(area.y)) * f64::from(font_size.height);
                let half_w = 9.0 * scale * f64::from(font_size.width);
                let half_h = 8.0 * scale * f64::from(font_size.height);

                let px_start = (px0 - half_w).floor().max(0.0) as u32;
                let px_end = (px0 + half_w).ceil().min(px_width as f64) as u32;
                let py_start = (py0 - half_h).floor().max(0.0) as u32;
                let py_end = (py0 + half_h).ceil().min(px_height as f64) as u32;

                for py in py_start..py_end {
                    for px in px_start..px_end {
                        let tx =
                            f64::from(area.x) + (f64::from(px) + 0.5) / f64::from(font_size.width);
                        let ty =
                            f64::from(area.y) + (f64::from(py) + 0.5) / f64::from(font_size.height);
                        let bx = (tx - off_x) * inv;
                        let by = (ty - off_y) * inv;
                        let (layer, shade) =
                            sample(bx, by, self.gaze_x, self.gaze_y, self.is_blinking);
                        image.put_pixel(px, py, mascot_pixel(layer, shade, &palette, false));
                    }
                }
            }

            let dyn_image = DynamicImage::ImageRgba8(image);

            match picker.new_protocol(
                dyn_image,
                Size::new(area.width, area.height),
                Resize::Fit(None),
            ) {
                Ok(protocol) => self.protocol = Some(protocol),
                Err(_) => {
                    self.picker = None;
                    self.render_braille(area, buf, &palette, off_x, off_y, inv);
                    return;
                }
            }

            self.last_gaze_x = self.gaze_x;
            self.last_gaze_y = self.gaze_y;
            self.last_blinking = self.is_blinking;
        }

        if let Some(protocol) = self.protocol.as_ref() {
            Image::new(protocol).render(area, buf);
        } else {
            self.render_braille(area, buf, &palette, off_x, off_y, inv);
        }
    }

    fn render_braille(
        &self,
        area: Rect,
        buf: &mut Buffer,
        palette: &Palette,
        off_x: f64,
        off_y: f64,
        inv: f64,
    ) {
        for ty in area.y..area.y + area.height {
            for tx in area.x..area.x + area.width {
                let mut layers = [Layer::None; SAMPLES_PER_CELL];
                let mut shades = [0.0; SAMPLES_PER_CELL];

                for i in 0..SAMPLES_PER_CELL {
                    let dy = i / BRAILLE_COLS;
                    let dx = i % BRAILLE_COLS;
                    let bx = (f64::from(tx) - off_x + SUBPX_DX[dx]) * inv;
                    let by = (f64::from(ty) - off_y + SUBPX_DY[dy]) * inv;
                    let (layer, shade) = sample(bx, by, self.gaze_x, self.gaze_y, self.is_blinking);
                    layers[i] = layer;
                    shades[i] = shade;
                }

                let mut counts = [(Layer::None, 0.0, 1usize); SAMPLES_PER_CELL];
                let mut distinct = 1;
                counts[0] = (layers[0], shades[0], 1);
                for i in 1..SAMPLES_PER_CELL {
                    let layer = layers[i];
                    let shade = shades[i];
                    if let Some(pos) = counts[..distinct].iter().position(|(l, _, _)| *l == layer) {
                        counts[pos].1 += shade;
                        counts[pos].2 += 1;
                    } else {
                        counts[distinct] = (layer, shade, 1);
                        distinct += 1;
                    }
                }

                counts[..distinct].sort_by_key(|b| std::cmp::Reverse(b.0));
                let fg = counts[0].0;
                if fg == Layer::None {
                    continue;
                }
                let bg = if distinct >= 2 { counts[1].0 } else { fg };

                let fg_color = palette.color(fg.role());
                let bg_color = if bg == fg {
                    palette.shadow(fg.role())
                } else {
                    palette.color(bg.role())
                };

                let mut mask: u8 = 0;
                for i in 0..SAMPLES_PER_CELL {
                    if layers[i] == fg && shades[i] > BAYER_THRESHOLDS[i] {
                        let dy = i / BRAILLE_COLS;
                        let dx = i % BRAILLE_COLS;
                        mask |= braille_bit(dy, dx);
                    }
                }

                let ch = char::from_u32(BRAILLE_BASE + u32::from(mask)).unwrap_or(' ');
                if let Some(cell) = buf.cell_mut((tx, ty)) {
                    cell.set_char(ch).set_fg(fg_color).set_bg(bg_color);
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
            | Layer::SideHairRight => Role::Hair,
            Layer::BrowLeft | Layer::BrowRight => Role::Brow,
            Layer::Face | Layer::InnerEarLeft | Layer::InnerEarRight => Role::Skin,
            Layer::BlushLeft | Layer::BlushRight => Role::Blush,
            Layer::Collar => Role::Collar,
            Layer::Ribbon => Role::Ribbon,
            Layer::EyeWhiteLeft | Layer::EyeWhiteRight => Role::EyeWhite,
            Layer::IrisLeft | Layer::IrisRight => Role::Eye,
            Layer::LashLeft | Layer::LashRight => Role::Lash,
            Layer::Nose => Role::Nose,
            Layer::Mouth => Role::Mouth,
            Layer::Teeth => Role::Teeth,
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
            teeth: Color::Rgb(
                lerp_u8(bg_r, 255, 0.98),
                lerp_u8(bg_g, 255, 0.98),
                lerp_u8(bg_b, 255, 0.98),
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
            Role::Teeth => self.teeth,
            Role::Nose => self.nose,
            Role::EyeWhite => self.eye_white,
            Role::Eye => self.eye,
            Role::Brow => self.brow,
            Role::Lash => self.lash,
            Role::Pupil => self.pupil,
            Role::Highlight => self.highlight,
        }
    }

    fn shadow(&self, role: Role) -> Color {
        let color = self.color(role);
        let (r, g, b) = extract_rgb(color, (0, 0, 0));
        let f = 1.0 - shadow_factor(role);
        Color::Rgb(
            (f32::from(r) * f) as u8,
            (f32::from(g) * f) as u8,
            (f32::from(b) * f) as u8,
        )
    }

    fn background(&self) -> Color {
        self.background
    }
}

fn shadow_factor(role: Role) -> f32 {
    match role {
        Role::Background => 0.0,
        Role::Highlight => 0.1,
        Role::Teeth => 0.15,
        Role::EyeWhite => 0.15,
        Role::Lash => 0.2,
        Role::Blush => 0.25,
        Role::Nose => 0.25,
        Role::Pupil => 0.25,
        Role::Ribbon => 0.3,
        Role::Skin => 0.35,
        Role::Brow => 0.35,
        Role::Mouth => 0.35,
        Role::Collar => 0.4,
        Role::Hair => 0.45,
        Role::Eye => 0.45,
    }
}

#[allow(clippy::too_many_lines)]
fn sample(x: f64, y: f64, gaze_x: f64, gaze_y: f64, blink: bool) -> (Layer, f64) {
    if !(MASCOT_MIN_X..=MASCOT_MAX_X).contains(&x) || !(MASCOT_MIN_Y..=MASCOT_MAX_Y).contains(&y) {
        return (Layer::None, 0.0);
    }

    for &(cx, cy, rx, ry) in &[
        (14.5, 22.0, 3.2, 2.2),
        (19.5, 22.0, 3.2, 2.2),
        (17.0, 22.0, 1.6, 1.6),
        (35.5, 47.5, 3.0, 2.0),
        (44.5, 47.5, 3.0, 2.0),
        (40.0, 47.5, 1.6, 1.6),
    ] {
        let d = sd_ellipse(x, y, cx, cy, rx, ry);
        if d <= 0.4 {
            return (Layer::Ribbon, aa(d, 0.4));
        }
    }

    for &(cx, cy, rx, ry) in &BANGS {
        let d = sd_ellipse(x, y, cx, cy, rx, ry);
        if d <= 0.8 {
            let grad = 1.0 - 0.4 * smoothstep(15.0, 28.0, y);
            return (Layer::Bangs, aa(d, 0.8) * grad);
        }
    }

    for &(cx, cy, rx, ry) in &SIDE_HAIR {
        let d = sd_ellipse(x, y, cx, cy, rx, ry);
        if d <= 0.8 {
            let grad = 1.0 - 0.5 * smoothstep(15.0, 45.0, y);
            let layer = if cx < 40.0 {
                Layer::SideHairLeft
            } else {
                Layer::SideHairRight
            };
            return (layer, aa(d, 0.8) * grad);
        }
    }

    let d_mouth = sd_ellipse(x, y, 40.0, 39.5, 2.2, 0.8);
    if d_mouth <= 0.5 {
        let mouth_shade = aa(d_mouth, 0.5);
        if (y - 39.2).abs() <= 0.35 && (x - 40.0).abs() <= 1.3 {
            let t = (y - 39.2).abs() / 0.35;
            return (Layer::Teeth, mouth_shade * (1.0 - smoothstep(0.0, 1.0, t)));
        }
        return (Layer::Mouth, mouth_shade);
    }

    let d_nose = sd_triangle(x, y, 40.0, 34.5, 38.8, 36.2, 41.2, 36.2);
    if d_nose <= 0.35 {
        return (Layer::Nose, aa(d_nose, 0.35));
    }

    for &(x1, y1, x2, y2) in &[(24.5, 21.0, 29.0, 19.5), (29.0, 19.5, 33.5, 20.5)] {
        let d = sd_segment(x, y, x1, y1, x2, y2, 0.3);
        if d <= 0.3 {
            return (Layer::BrowLeft, aa(d, 0.3));
        }
    }
    for &(x1, y1, x2, y2) in &[(46.5, 20.5, 51.0, 19.5), (51.0, 19.5, 55.5, 21.0)] {
        let d = sd_segment(x, y, x1, y1, x2, y2, 0.3);
        if d <= 0.3 {
            return (Layer::BrowRight, aa(d, 0.3));
        }
    }

    if !blink {
        let mut eye_layer = Layer::None;
        let mut eye_shade = 0.0;

        for &(cx_base, cy_base) in &[(30.0, 27.5), (50.0, 27.5)] {
            let cx = cx_base + gaze_x;
            let cy = cy_base + gaze_y;

            let hx = cx + 2.0;
            let hy = cy - 1.8;
            let d_highlight = sd_circle(x, y, hx, hy, 1.1);
            let layer_highlight = if cx < 40.0 {
                Layer::HighlightLeft
            } else {
                Layer::HighlightRight
            };
            if d_highlight <= 0.4 && layer_highlight > eye_layer {
                eye_layer = layer_highlight;
                eye_shade = aa(d_highlight, 0.4);
            }

            let d_pupil = sd_ellipse(x, y, cx, cy - 0.2, 2.5, 3.0);
            let layer_pupil = if cx < 40.0 {
                Layer::PupilLeft
            } else {
                Layer::PupilRight
            };
            if d_pupil <= 0.5 && layer_pupil > eye_layer {
                eye_layer = layer_pupil;
                eye_shade = aa(d_pupil, 0.5);
            }

            let d_iris = sd_ellipse(x, y, cx, cy, 5.0, 4.5);
            if d_iris <= 0.6 {
                let t = ((x - cx) / 5.0).hypot((y - cy) / 4.5);
                let grad = 1.0 - 0.5 * smoothstep(0.0, 1.0, t);
                let layer_iris = if cx < 40.0 {
                    Layer::IrisLeft
                } else {
                    Layer::IrisRight
                };
                let s = aa(d_iris, 0.6) * grad;
                if layer_iris > eye_layer {
                    eye_layer = layer_iris;
                    eye_shade = s;
                }
            }

            let d_eyewhite = sd_ellipse(x, y, cx, cy, 7.0, 5.5);
            let layer_eyewhite = if cx < 40.0 {
                Layer::EyeWhiteLeft
            } else {
                Layer::EyeWhiteRight
            };
            if d_eyewhite <= 0.6 && layer_eyewhite > eye_layer {
                eye_layer = layer_eyewhite;
                eye_shade = aa(d_eyewhite, 0.6);
            }
        }

        for &(cx_base, cy_base, x1_off, y1_off, x2_off, y2_off, thick) in &[
            (30.0, 27.5, -6.0, -4.5, 0.0, -5.5, 0.55),
            (50.0, 27.5, -6.0, -5.5, 0.0, -4.5, 0.55),
        ] {
            let cx = cx_base + gaze_x;
            let cy = cy_base + gaze_y;
            let d_lash = sd_segment(
                x,
                y,
                cx + x1_off,
                cy + y1_off,
                cx + x2_off,
                cy + y2_off,
                thick,
            );
            let layer_lash = if cx < 40.0 {
                Layer::LashLeft
            } else {
                Layer::LashRight
            };
            if d_lash <= 0.35 && layer_lash > eye_layer {
                eye_layer = layer_lash;
                eye_shade = aa(d_lash, 0.35);
            }
        }

        for &(cx_base, cy_base, x1_off, y1_off, x2_off, y2_off, thick) in &[
            (30.0, 27.5, -1.5, 4.5, 0.5, 4.0, 0.35),
            (50.0, 27.5, -0.5, 4.0, 1.5, 4.5, 0.35),
        ] {
            let cx = cx_base + gaze_x;
            let cy = cy_base + gaze_y;
            let d_lash = sd_segment(
                x,
                y,
                cx + x1_off,
                cy + y1_off,
                cx + x2_off,
                cy + y2_off,
                thick,
            );
            let layer_lash = if cx < 40.0 {
                Layer::LashLeft
            } else {
                Layer::LashRight
            };
            if d_lash <= 0.35 && layer_lash > eye_layer {
                eye_layer = layer_lash;
                eye_shade = aa(d_lash, 0.35);
            }
        }

        if eye_layer != Layer::None {
            return (eye_layer, eye_shade);
        }
    } else {
        for &(cx, cy, layer) in &[
            (30.0, 27.5, Layer::EyeWhiteLeft),
            (50.0, 27.5, Layer::EyeWhiteRight),
        ] {
            let d = sd_ellipse(x, y, cx, cy, 7.0, 5.5);
            if d <= 0.6 {
                if (y - 27.5).abs() <= 0.5 {
                    let t = (y - 27.5).abs() / 0.5;
                    let s = aa(d, 0.6) * (1.0 - smoothstep(0.0, 1.0, t));
                    let lash = if layer == Layer::EyeWhiteLeft {
                        Layer::LashLeft
                    } else {
                        Layer::LashRight
                    };
                    return (lash, s);
                }
                return (layer, aa(d, 0.6));
            }
        }
    }

    for &(cx, cy, rx, ry, layer) in &[
        (24.0, 33.5, 3.0, 1.8, Layer::BlushLeft),
        (56.0, 33.5, 3.0, 1.8, Layer::BlushRight),
    ] {
        let d = sd_ellipse(x, y, cx, cy, rx, ry);
        if d <= 0.5 {
            return (layer, aa(d, 0.5));
        }
    }

    let d_collar = sd_ellipse(x, y, 40.0, 46.5, 13.0, 2.4);
    if d_collar <= 0.4 {
        return (Layer::Collar, aa(d_collar, 0.4));
    }

    let d_top = sd_ellipse(x, y, 40.0, 28.0, 15.0, 10.5);
    let d_jaw = sd_ellipse(x, y, 40.0, 34.5, 12.5, 8.5);
    let d_neck = sd_ellipse(x, y, 40.0, 43.0, 6.5, 2.0);
    let d = d_top.min(d_jaw).min(d_neck);
    if d <= 0.6 {
        let edge = 1.0 - smoothstep(0.0, 9.0, -d);
        let chin = 1.0 - 0.15 * smoothstep(22.0, 40.0, y);
        let grad = (1.0 - 0.25 * edge) * chin;
        return (Layer::Face, aa(d, 0.6) * grad);
    }

    for &(x1, y1, x2, y2, x3, y3, layer) in &[
        (26.5, 11.0, 32.0, 18.0, 21.0, 18.0, Layer::InnerEarLeft),
        (53.5, 11.0, 48.0, 18.0, 59.0, 18.0, Layer::InnerEarRight),
    ] {
        let d = sd_triangle(x, y, x1, y1, x2, y2, x3, y3);
        if d <= 0.35 {
            return (layer, aa(d, 0.35));
        }
    }

    for &(x1, y1, x2, y2, x3, y3, layer) in &[
        (25.5, 7.5, 33.0, 18.0, 18.5, 18.0, Layer::EarLeft),
        (54.5, 7.5, 47.0, 18.0, 61.5, 18.0, Layer::EarRight),
    ] {
        let d = sd_triangle(x, y, x1, y1, x2, y2, x3, y3);
        if d <= 0.8 {
            let grad = 1.0 - 0.3 * smoothstep(15.0, 30.0, y);
            return (layer, aa(d, 0.8) * grad);
        }
    }

    for &(cx, cy, r) in &HAIR_LEFT {
        let d = sd_circle(x, y, cx, cy, r);
        if d <= 0.8 {
            let grad = 1.0 - 0.5 * smoothstep(15.0, 45.0, y);
            return (Layer::HairLeft, aa(d, 0.8) * grad);
        }
    }
    for &(cx, cy, r) in &HAIR_RIGHT {
        let d = sd_circle(x, y, cx, cy, r);
        if d <= 0.8 {
            let grad = 1.0 - 0.5 * smoothstep(15.0, 45.0, y);
            return (Layer::HairRight, aa(d, 0.8) * grad);
        }
    }

    let d_back = sd_ellipse(x, y, 40.0, 31.0, 15.0, 20.0);
    if d_back <= 0.5 {
        let grad = 1.0 - 0.6 * smoothstep(15.0, 45.0, y);
        return (Layer::HairBack, aa(d_back, 0.5) * grad);
    }

    (Layer::None, 0.0)
}

fn sd_ellipse(x: f64, y: f64, cx: f64, cy: f64, rx: f64, ry: f64) -> f64 {
    ((x - cx) / rx).hypot((y - cy) / ry) - 1.0
}

fn sd_circle(x: f64, y: f64, cx: f64, cy: f64, r: f64) -> f64 {
    (x - cx).hypot(y - cy) - r
}

#[allow(clippy::too_many_arguments)]
fn sd_triangle(px: f64, py: f64, x1: f64, y1: f64, x2: f64, y2: f64, x3: f64, y3: f64) -> f64 {
    let sign = |p1: (f64, f64), p2: (f64, f64), p3: (f64, f64)| {
        (p1.0 - p3.0) * (p2.1 - p3.1) - (p2.0 - p3.0) * (p1.1 - p3.1)
    };
    let d1 = sign((px, py), (x1, y1), (x2, y2));
    let d2 = sign((px, py), (x2, y2), (x3, y3));
    let d3 = sign((px, py), (x3, y3), (x1, y1));
    let inside = !((d1 < 0.0 || d2 < 0.0 || d3 < 0.0) && (d1 > 0.0 || d2 > 0.0 || d3 > 0.0));

    let dseg = |p: (f64, f64), a: (f64, f64), b: (f64, f64)| {
        let vx = b.0 - a.0;
        let vy = b.1 - a.1;
        let wx = p.0 - a.0;
        let wy = p.1 - a.1;
        let c1 = wx * vx + wy * vy;
        if c1 <= 0.0 {
            return (p.0 - a.0).hypot(p.1 - a.1);
        }
        let c2 = vx * vx + vy * vy;
        if c1 >= c2 {
            return (p.0 - b.0).hypot(p.1 - b.1);
        }
        let t = c1 / c2;
        (p.0 - (a.0 + t * vx)).hypot(p.1 - (a.1 + t * vy))
    };

    let d = dseg((px, py), (x1, y1), (x2, y2))
        .min(dseg((px, py), (x2, y2), (x3, y3)))
        .min(dseg((px, py), (x3, y3), (x1, y1)));
    if inside { -d } else { d }
}

fn sd_segment(px: f64, py: f64, x1: f64, y1: f64, x2: f64, y2: f64, thickness: f64) -> f64 {
    let vx = x2 - x1;
    let vy = y2 - y1;
    let wx = px - x1;
    let wy = py - y1;
    let c1 = wx * vx + wy * vy;
    if c1 <= 0.0 {
        return (px - x1).hypot(py - y1) - thickness;
    }
    let c2 = vx * vx + vy * vy;
    if c1 >= c2 {
        return (px - x2).hypot(py - y2) - thickness;
    }
    let t = c1 / c2;
    (px - (x1 + t * vx)).hypot(py - (y1 + t * vy)) - thickness
}

fn smoothstep(edge0: f64, edge1: f64, x: f64) -> f64 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn aa(d: f64, edge: f64) -> f64 {
    1.0 - smoothstep(0.0, edge, d)
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

fn extract_color(color: Color) -> (u8, u8, u8) {
    match color {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => (0, 0, 0),
    }
}

fn mascot_pixel(layer: Layer, shade: f64, palette: &Palette, opaque_background: bool) -> Rgba<u8> {
    match layer {
        Layer::None => {
            if opaque_background {
                let (r, g, b) = extract_color(palette.background());
                Rgba([r, g, b, 255])
            } else {
                Rgba([0, 0, 0, 0])
            }
        }
        _ => {
            let role = layer.role();
            let color = palette.color(role);
            let shadow = palette.shadow(role);
            let (color_r, color_g, color_b) = extract_color(color);
            let (shadow_r, shadow_g, shadow_b) = extract_color(shadow);
            let r = lerp_u8(shadow_r, color_r, shade as f32);
            let g = lerp_u8(shadow_g, color_g, shade as f32);
            let b = lerp_u8(shadow_b, color_b, shade as f32);
            Rgba([r, g, b, 255])
        }
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
