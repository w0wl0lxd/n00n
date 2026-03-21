use std::mem;
use std::time::{Duration, Instant};

const SPINNER_FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const SPINNER_STRS: [&str; 10] = ["⠋ ", "⠙ ", "⠹ ", "⠸ ", "⠼ ", "⠴ ", "⠦ ", "⠧ ", "⠇ ", "⠏ "];
const SPINNER_FRAME_MS: u128 = 80;

pub fn spinner_frame(elapsed_ms: u128) -> char {
    SPINNER_FRAMES[(elapsed_ms / SPINNER_FRAME_MS) as usize % SPINNER_FRAMES.len()]
}

pub fn spinner_str(elapsed_ms: u128) -> &'static str {
    SPINNER_STRS[(elapsed_ms / SPINNER_FRAME_MS) as usize % SPINNER_STRS.len()]
}

const DEFAULT_MS_PER_CHAR: u64 = 4;
const MIN_DURATION_MS: u64 = 30;
const MAX_DURATION_MS: u64 = 1000;

pub struct Typewriter {
    buffer: String,
    visible_len: usize,
    anim_start_visible: usize,
    anim_target: usize,
    anim_start_at: Instant,
    anim_duration: Duration,
    ms_per_char: u64,
}

impl Default for Typewriter {
    fn default() -> Self {
        Self::with_speed(DEFAULT_MS_PER_CHAR)
    }
}

impl Typewriter {
    pub fn new() -> Self {
        Self::with_speed(DEFAULT_MS_PER_CHAR)
    }

    pub fn with_speed(ms_per_char: u64) -> Self {
        Self {
            buffer: String::new(),
            visible_len: 0,
            anim_start_visible: 0,
            anim_target: 0,
            anim_start_at: Instant::now(),
            anim_duration: Duration::ZERO,
            ms_per_char,
        }
    }

    pub fn push(&mut self, text: &str) {
        self.buffer.push_str(text);
        self.tick();
        self.anim_start_visible = self.visible_len;
        self.anim_target = self.buffer.chars().count();
        if self.ms_per_char == 0 {
            self.visible_len = self.anim_target;
            return;
        }
        let unrevealed = self.anim_target - self.anim_start_visible;
        let ms = (unrevealed as u64 * self.ms_per_char).clamp(MIN_DURATION_MS, MAX_DURATION_MS);
        self.anim_duration = Duration::from_millis(ms);
        self.anim_start_at = Instant::now();
    }

    pub fn tick(&mut self) {
        if self.visible_len >= self.anim_target {
            return;
        }
        let elapsed = self.anim_start_at.elapsed();
        let progress = (elapsed.as_secs_f64() / self.anim_duration.as_secs_f64()).min(1.0);
        let delta = self.anim_target - self.anim_start_visible;
        self.visible_len = self.anim_start_visible + (delta as f64 * progress).round() as usize;
    }

    pub fn visible(&self) -> &str {
        let byte_offset = self
            .buffer
            .char_indices()
            .nth(self.visible_len)
            .map_or(self.buffer.len(), |(i, _)| i);
        &self.buffer[..byte_offset]
    }

    pub fn is_animating(&self) -> bool {
        self.visible_len < self.anim_target
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
        self.visible_len = 0;
        self.anim_start_visible = 0;
        self.anim_target = 0;
    }

    pub fn take_all(&mut self) -> String {
        self.visible_len = 0;
        self.anim_start_visible = 0;
        self.anim_target = 0;
        mem::take(&mut self.buffer)
    }

    #[cfg(test)]
    pub(crate) fn set_buffer(&mut self, text: &str) {
        self.buffer = text.into();
        let len = self.buffer.chars().count();
        self.visible_len = len;
        self.anim_start_visible = len;
        self.anim_target = len;
        self.anim_duration = Duration::ZERO;
    }
}

impl PartialEq<&str> for Typewriter {
    fn eq(&self, other: &&str) -> bool {
        self.buffer == *other
    }
}

impl std::fmt::Debug for Typewriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Typewriter")
            .field("buffer", &self.buffer)
            .field("visible_len", &self.visible_len)
            .field("anim_target", &self.anim_target)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spinner_wraps_around() {
        let first = spinner_frame(0);
        let wrapped = spinner_frame(SPINNER_FRAME_MS * SPINNER_FRAMES.len() as u128);
        assert_eq!(first, wrapped);
        assert_ne!(first, spinner_frame(SPINNER_FRAME_MS));
    }

    #[test]
    fn typewriter_push_does_not_reveal_immediately() {
        let mut tw = Typewriter::new();
        tw.push("hello world, this is a longer string");
        assert_eq!(tw.visible(), "");
        assert!(tw.is_animating());
    }

    #[test]
    fn typewriter_empty_push_is_noop() {
        let mut tw = Typewriter::new();
        tw.push("");
        assert!(!tw.is_animating());
        assert!(tw.is_empty());
    }

    #[test]
    fn typewriter_multibyte_visible() {
        let mut tw = Typewriter::new();
        tw.set_buffer("héllo 🌍");
        assert_eq!(tw.visible(), "héllo 🌍");
    }

    #[test]
    fn typewriter_extend_preserves_visible() {
        let mut tw = Typewriter::new();
        tw.set_buffer("ab");
        tw.push("cd");
        assert_eq!(tw.visible(), "ab");
        assert!(tw.is_animating());
    }

    #[test]
    fn typewriter_take_all_resets() {
        let mut tw = Typewriter::new();
        tw.set_buffer("data");
        let taken = tw.take_all();
        assert_eq!(taken, "data");
        assert!(tw.is_empty());
        assert!(!tw.is_animating());
        assert_eq!(tw.visible(), "");
    }

    #[test]
    fn typewriter_clear_discards_buffer() {
        let mut tw = Typewriter::new();
        tw.set_buffer("data");
        tw.push("more");
        tw.clear();
        assert!(tw.is_empty());
        assert!(!tw.is_animating());
        assert_eq!(tw.visible(), "");
    }
}
