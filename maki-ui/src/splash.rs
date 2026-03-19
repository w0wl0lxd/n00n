use crate::theme;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use std::time::Instant;

const LOGO: &str = "maki";
const TAGLINE: &str = "the efficient coder";
const HELP_SEGMENTS: &[(&str, bool)] = &[
    ("ctrl+h", true),
    (" help", false),
    (" · ", false),
    ("/help", true),
    (" in chat", false),
];

/// Seconds for the initial fade-in animation (ease-out cubic).
const FADE_DURATION: f64 = 1.6;
/// Seconds to wait before the logo starts appearing.
const LOGO_DELAY: f64 = 0.2;
/// Seconds over which the logo fades from dim to full brightness.
const LOGO_RAMP: f64 = 0.8;
/// Ascii chars mapped to increasing wave intensity (first must be space).
const FIELD_CHARS: &[char] = &[' ', '.', ':', '+', '*'];
const FIELD_CHAR_MAX: f64 = (FIELD_CHARS.len() - 1) as f64;
/// Number of overlapping sine wave layers in the background field.
const WAVE_LAYERS: usize = 3;
/// Peak brightness multiplier for the field. Lower = subtler background.
const INTENSITY_SCALE: f64 = 0.3;
/// Minimum intensity to render a cell. Raise this to cull more dim pixels.
const INTENSITY_THRESHOLD: f64 = 0.15;
/// How quickly the field darkens toward the edges. Higher = tighter spotlight.
const VIGNETTE_SCALE: f64 = 0.25;
/// Base opacity for the dimmest field character (0.0–1.0). Higher = less contrast between chars.
const FIELD_BASE_OPACITY: f64 = 0.5;

pub struct Splash {
    start: Instant,
    field_offset: f64,
}

impl Splash {
    pub fn new() -> Self {
        let mut buf = [0u8; 8];
        getrandom::fill(&mut buf).ok();
        let field_offset = (u64::from_le_bytes(buf) % 10_000) as f64;
        Self {
            start: Instant::now(),
            field_offset,
        }
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.width < 20 || area.height < 5 {
            return;
        }

        let t = self.start.elapsed().as_secs_f64();
        let fade = if t >= FADE_DURATION {
            1.0
        } else {
            ease_out_cubic(t / FADE_DURATION)
        };

        let total_h = 5;
        let top_y = area.y + area.height.saturating_sub(total_h) / 2;
        let tag_y = top_y + 1;
        let help_y = tag_y + 2;

        self.render_field(area, buf, t + self.field_offset, fade);
        self.render_logo(area, buf, t, fade, top_y);
        self.render_tagline(area, buf, fade, tag_y);
        self.render_help(area, buf, fade, help_y);
    }

    fn render_field(&self, area: Rect, buf: &mut Buffer, t: f64, fade: f64) {
        let theme = theme::current();
        let accent = theme.heading.fg.unwrap_or(theme.foreground);
        let (ac_r, ac_g, ac_b) = extract_rgb(accent, (100, 140, 255));
        let (bg_r, bg_g, bg_b) = extract_rgb(theme.background, (15, 15, 25));

        let w = area.width as usize;
        let h = area.height as usize;
        if w == 0 || h == 0 {
            return;
        }
        let inv_w = 1.0 / w as f64;
        let inv_h = 1.0 / h as f64;
        let fade_intensity = fade * INTENSITY_SCALE;

        let layers: [(f64, f64, f64, f64); WAVE_LAYERS] = std::array::from_fn(|i| {
            let lf = i as f64;
            (
                2.0 + lf * 1.8,
                1.5 + lf * 1.2,
                t * (0.3 + lf * 0.15) + lf * 2.094,
                1.0 / (1.5 + lf * 0.5),
            )
        });

        let weight_sum: f64 = layers.iter().map(|l| l.3).sum();
        let half_weight_sum = weight_sum * 0.5;
        let val_scale = fade_intensity / half_weight_sum;

        let color_lut: [(char, Style); 4] = std::array::from_fn(|i| {
            let idx = i + 1;
            let frac = idx as f64 / FIELD_CHAR_MAX;
            let t = FIELD_BASE_OPACITY + frac * (1.0 - FIELD_BASE_OPACITY);
            (
                FIELD_CHARS[idx],
                Style::new().fg(Color::Rgb(
                    lerp_u8(bg_r, ac_r, t * 0.25),
                    lerp_u8(bg_g, ac_g, t * 0.175),
                    lerp_u8(bg_b, ac_b, t * 0.325),
                )),
            )
        });

        let vignette_inv = 1.0 / VIGNETTE_SCALE;

        // Precompute vignette-x and sin/cos per column per layer.
        // sin(col_angle + row_angle) = sin(col)*cos(row) + cos(col)*sin(row)
        // This reduces sin calls from w*h*WAVE_LAYERS to w*WAVE_LAYERS + h*WAVE_LAYERS.
        let mut vx: Vec<f64> = Vec::with_capacity(w);
        let mut col_sincos: Vec<[(f64, f64); WAVE_LAYERS]> = Vec::with_capacity(w);
        for col in 0..w {
            let nx = col as f64 * inv_w;
            let d = (nx - 0.5) * 2.0;
            vx.push(d * d);
            col_sincos.push(std::array::from_fn(|i| {
                let (s, c) = (nx * layers[i].0).sin_cos();
                (s * layers[i].3, c * layers[i].3)
            }));
        }

        let col_start = vx.partition_point(|&v| v > vignette_inv);
        let col_end = w - vx
            .iter()
            .rev()
            .position(|&v| v <= vignette_inv)
            .unwrap_or(0);
        if col_start >= col_end {
            return;
        }

        for row in 0..h {
            let ny = row as f64 * inv_h;
            let d = (ny - 0.5) * 2.0;
            let vy = d * d;

            let max_vx = vignette_inv - vy;
            if max_vx <= 0.0 {
                continue;
            }

            let row_sincos: [(f64, f64); WAVE_LAYERS] =
                std::array::from_fn(|i| (ny * layers[i].1 + layers[i].2).sin_cos());

            let y = area.y + row as u16;

            let rc_start = col_start + vx[col_start..col_end].partition_point(|&v| v > max_vx);
            let rc_end = col_end
                - vx[col_start..col_end]
                    .iter()
                    .rev()
                    .position(|&v| v <= max_vx)
                    .unwrap_or(0);

            for col in rc_start..rc_end {
                let vignette = 1.0 - (vx[col] + vy) * VIGNETTE_SCALE;

                let mut wave = 0.0_f64;
                for i in 0..WAVE_LAYERS {
                    let (sc, cc) = col_sincos[col][i];
                    let (sr, cr) = row_sincos[i];
                    wave += sc * cr + cc * sr;
                }

                let val = (wave + half_weight_sum) * vignette * val_scale;
                if val < INTENSITY_THRESHOLD {
                    continue;
                }

                let idx = (val * FIELD_CHAR_MAX + 0.5) as usize;
                if idx == 0 {
                    continue;
                }
                let (ch, style) = &color_lut[idx.min(FIELD_CHARS.len() - 1) - 1];

                let x = area.x + col as u16;
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_char(*ch).set_style(*style);
                }
            }
        }
    }

    fn render_logo(&self, area: Rect, buf: &mut Buffer, t: f64, fade: f64, top_y: u16) {
        let theme = theme::current();
        let bg = theme.background;
        let accent = theme.heading.fg.unwrap_or(theme.foreground);
        let (ac_r, ac_g, ac_b) = extract_rgb(accent, (100, 140, 255));
        let (bg_r, bg_g, bg_b) = extract_rgb(bg, (15, 15, 25));

        let logo_w = LOGO.len() as u16;
        let logo_x = area.x + (area.width.saturating_sub(logo_w)) / 2;
        let logo_fade = ease_out_cubic(((t - LOGO_DELAY) / LOGO_RAMP).clamp(0.0, 1.0));

        let alpha = 0.85 * logo_fade * fade;
        let r = lerp_u8(bg_r, ac_r, alpha);
        let g = lerp_u8(bg_g, ac_g, alpha);
        let b = lerp_u8(bg_b, ac_b.saturating_add(15), alpha);
        let style = Style::new()
            .fg(Color::Rgb(r, g, b))
            .bg(bg)
            .add_modifier(Modifier::BOLD);

        for (col, ch) in LOGO.chars().enumerate() {
            let x = logo_x + col as u16;
            if x >= area.x + area.width || top_y >= area.y + area.height {
                continue;
            }
            if let Some(cell) = buf.cell_mut((x, top_y)) {
                cell.set_char(ch).set_style(style);
            }
        }
    }

    fn render_tagline(&self, area: Rect, buf: &mut Buffer, fade: f64, tag_y: u16) {
        if tag_y >= area.y + area.height {
            return;
        }

        let theme = theme::current();
        let bg = theme.background;
        let (fg_r, fg_g, fg_b) = extract_rgb(theme.foreground, (200, 200, 200));
        let (bg_r, bg_g, bg_b) = extract_rgb(bg, (15, 15, 25));

        let tag_w = TAGLINE.len() as u16;
        let tag_x = area.x + (area.width.saturating_sub(tag_w)) / 2;

        let alpha = 0.75 * fade;
        let r = lerp_u8(bg_r, fg_r, alpha);
        let g = lerp_u8(bg_g, fg_g, alpha);
        let b = lerp_u8(bg_b, fg_b, alpha);
        let style = Style::new().fg(Color::Rgb(r, g, b)).bg(bg);

        for (col, ch) in TAGLINE.chars().enumerate() {
            let x = tag_x + col as u16;
            if x >= area.x + area.width {
                break;
            }

            if let Some(cell) = buf.cell_mut((x, tag_y)) {
                cell.set_char(ch).set_style(style);
            }
        }
    }

    fn render_help(&self, area: Rect, buf: &mut Buffer, fade: f64, help_y: u16) {
        if help_y >= area.y + area.height {
            return;
        }

        let theme = theme::current();
        let bg = theme.background;
        let accent = theme.cmd_name.fg.unwrap_or(theme.foreground);
        let (ac_r, ac_g, ac_b) = extract_rgb(accent, (100, 140, 255));
        let (fg_r, fg_g, fg_b) = extract_rgb(theme.foreground, (200, 200, 200));
        let (bg_r, bg_g, bg_b) = extract_rgb(bg, (15, 15, 25));

        let total_len: u16 = HELP_SEGMENTS.iter().map(|(s, _)| s.len() as u16).sum();
        let help_x = area.x + (area.width.saturating_sub(total_len)) / 2;

        let mut col = 0usize;
        for &(segment, highlighted) in HELP_SEGMENTS {
            let (tr, tg, tb) = if highlighted {
                (ac_r, ac_g, ac_b)
            } else {
                (fg_r, fg_g, fg_b)
            };
            let base_alpha = if highlighted { 0.75 } else { 0.5 };
            let alpha = base_alpha * fade;
            let r = lerp_u8(bg_r, tr, alpha);
            let g = lerp_u8(bg_g, tg, alpha);
            let b = lerp_u8(bg_b, tb, alpha);
            let style = Style::new().fg(Color::Rgb(r, g, b)).bg(bg);

            for ch in segment.chars() {
                let x = help_x + col as u16;
                if x >= area.x + area.width {
                    break;
                }

                if let Some(cell) = buf.cell_mut((x, help_y)) {
                    cell.set_char(ch).set_style(style);
                }

                col += 1;
            }
        }
    }
}

fn extract_rgb(color: Color, fallback: (u8, u8, u8)) -> (u8, u8, u8) {
    match color {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => fallback,
    }
}

#[inline(always)]
fn lerp_u8(a: u8, b: u8, t: f64) -> u8 {
    (a as f64 + (b as f64 - a as f64) * t.clamp(0.0, 1.0)) as u8
}

fn ease_out_cubic(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}
