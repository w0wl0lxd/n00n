use std::mem;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use quanta::Instant;

const SPINNER_FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const SPINNER_STRS: [&str; 10] = ["⠋ ", "⠙ ", "⠹ ", "⠸ ", "⠼ ", "⠴ ", "⠦ ", "⠧ ", "⠇ ", "⠏ "];
const SPINNER_FRAME_MS: u128 = 80;

static REDUCED_MOTION: AtomicBool = AtomicBool::new(false);

/// Turn reduced motion on or off for the whole process.
///
/// Drive this from `ui.reduced_motion` once the config is resolved. With it on,
/// every animated surface in this module reports its finished, static state, so
/// nothing in the TUI moves on its own.
pub fn set_reduced_motion(on: bool) {
    REDUCED_MOTION.store(on, Ordering::Relaxed);
}

/// Whether reduced motion is currently on.
#[must_use]
pub fn reduced_motion() -> bool {
    REDUCED_MOTION.load(Ordering::Relaxed)
}

/// Index of the frame to draw. Reduced motion pins it to the first frame, so
/// the spinner still reads as a spinner but never moves.
fn spinner_index(reduced: bool, elapsed_ms: u128, len: usize) -> usize {
    if reduced {
        return 0;
    }
    (elapsed_ms / SPINNER_FRAME_MS) as usize % len
}

#[must_use]
pub fn spinner_frame(elapsed_ms: u128) -> char {
    SPINNER_FRAMES[spinner_index(reduced_motion(), elapsed_ms, SPINNER_FRAMES.len())]
}

#[must_use]
pub fn spinner_str(elapsed_ms: u128) -> &'static str {
    SPINNER_STRS[spinner_index(reduced_motion(), elapsed_ms, SPINNER_STRS.len())]
}

/// Spinners need a consistent time reference. Using a static epoch avoids
/// passing Instant through every render call.
pub fn animation_elapsed_ms() -> u128 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed().as_millis()
}

const DEFAULT_MS_PER_CHAR: u64 = 4;
const MIN_DURATION_MS: u64 = 30;
const MAX_DURATION_MS: u64 = 1000;

pub struct Typewriter {
    buffer: String,
    visible_len: usize,
    visible_byte_offset: usize,
    anim_start_visible: usize,
    anim_target: usize,
    anim_start_at: Instant,
    anim_duration: Duration,
    ms_per_char: u64,
    char_count: usize,
    newline_count: usize,
    generation: u64,
    reduced_motion: bool,
}

impl Default for Typewriter {
    fn default() -> Self {
        Self::with_speed(DEFAULT_MS_PER_CHAR)
    }
}

impl Typewriter {
    #[must_use]
    pub fn new() -> Self {
        Self::with_speed(DEFAULT_MS_PER_CHAR)
    }

    #[must_use]
    pub fn with_speed(ms_per_char: u64) -> Self {
        Self {
            buffer: String::new(),
            visible_len: 0,
            visible_byte_offset: 0,
            anim_start_visible: 0,
            anim_target: 0,
            anim_start_at: Instant::now(),
            anim_duration: Duration::ZERO,
            ms_per_char,
            char_count: 0,
            newline_count: 0,
            generation: 1,
            reduced_motion: reduced_motion(),
        }
    }

    /// Override this typewriter's motion setting.
    ///
    /// A typewriter picks the process-wide setting up when it is built; call
    /// this to change one after the fact, such as after a config reload.
    pub fn set_reduced_motion(&mut self, on: bool) {
        self.reduced_motion = on;
        if on {
            self.tick();
        }
    }

    pub fn push(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.char_count += text.chars().count();
        self.newline_count += text.bytes().filter(|&b| b == b'\n').count();
        self.generation = self.generation.wrapping_add(1);
        self.buffer.push_str(text);
        self.tick();
        self.anim_start_visible = self.visible_len;
        self.anim_target = self.char_count;
        if self.ms_per_char == 0 || self.reduced_motion {
            self.advance_visible(self.anim_target);
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
        if self.reduced_motion {
            self.advance_visible(self.anim_target);
            return;
        }
        let elapsed = self.anim_start_at.elapsed();
        let progress = (elapsed.as_secs_f64() / self.anim_duration.as_secs_f64()).min(1.0);
        let delta = self.anim_target - self.anim_start_visible;
        let new_len = self.anim_start_visible
            + crate::cast::f64_to_usize((crate::cast::usize_to_f64(delta) * progress).round());
        self.advance_visible(new_len);
    }

    #[must_use]
    pub fn visible(&self) -> &str {
        &self.buffer[..self.visible_byte_offset]
    }

    #[must_use]
    pub fn is_animating(&self) -> bool {
        self.visible_len < self.anim_target
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    #[must_use]
    pub fn visible_len(&self) -> usize {
        self.visible_len
    }

    #[must_use]
    pub fn visible_byte_offset(&self) -> usize {
        self.visible_byte_offset
    }

    #[must_use]
    pub fn char_count(&self) -> usize {
        self.char_count
    }

    #[must_use]
    pub fn buffer_line_count(&self) -> usize {
        if self.buffer.is_empty() {
            0
        } else {
            self.newline_count + 1
        }
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
        self.char_count = 0;
        self.newline_count = 0;
        self.generation = 1;
        self.reset_anim();
    }

    pub fn take_all(&mut self) -> String {
        self.char_count = 0;
        self.newline_count = 0;
        self.generation = 1;
        self.reset_anim();
        mem::take(&mut self.buffer)
    }

    #[cfg(test)]
    pub(crate) fn set_buffer(&mut self, text: &str) {
        self.buffer = text.into();
        self.char_count = self.buffer.chars().count();
        self.newline_count = self.buffer.bytes().filter(|&b| b == b'\n').count();
        self.generation = text
            .bytes()
            .fold(1u64, |h, b| h.wrapping_mul(31).wrapping_add(u64::from(b)));
        self.visible_len = self.char_count;
        self.visible_byte_offset = self.buffer.len();
        self.anim_start_visible = self.char_count;
        self.anim_target = self.char_count;
        self.anim_duration = Duration::ZERO;
    }

    fn reset_anim(&mut self) {
        self.visible_len = 0;
        self.visible_byte_offset = 0;
        self.anim_start_visible = 0;
        self.anim_target = 0;
    }

    fn advance_visible(&mut self, new_len: usize) {
        let skip = new_len - self.visible_len;
        if skip > 0 {
            self.visible_byte_offset = self.buffer[self.visible_byte_offset..]
                .char_indices()
                .nth(skip)
                .map_or(self.buffer.len(), |(i, _)| self.visible_byte_offset + i);
        }
        self.visible_len = new_len;
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
            .field("visible_byte_offset", &self.visible_byte_offset)
            .field("anim_start_visible", &self.anim_start_visible)
            .field("anim_target", &self.anim_target)
            .field("anim_start_at", &self.anim_start_at)
            .field("anim_duration", &self.anim_duration)
            .field("ms_per_char", &self.ms_per_char)
            .field("char_count", &self.char_count)
            .field("newline_count", &self.newline_count)
            .field("generation", &self.generation)
            .field("reduced_motion", &self.reduced_motion)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The process-wide flag is never flipped by these tests: every guarded
    /// path is reachable through `spinner_index` or the per-typewriter
    /// override, so the tests stay independent of the runner's isolation.
    #[test]
    fn reduced_motion_is_off_by_default() {
        assert!(!reduced_motion());
    }

    #[test]
    fn reduced_motion_pins_the_spinner_to_one_frame() {
        let full: Vec<usize> = (0..12)
            .map(|i| spinner_index(false, i * SPINNER_FRAME_MS, SPINNER_FRAMES.len()))
            .collect();
        assert!(full.iter().any(|&i| i != full[0]), "motion still advances");

        for i in 0..12 {
            assert_eq!(
                spinner_index(true, i * SPINNER_FRAME_MS, SPINNER_FRAMES.len()),
                0,
                "frame {i} must not move"
            );
        }
    }

    #[test]
    fn reduced_motion_spinner_stays_a_valid_frame() {
        assert_eq!(
            SPINNER_FRAMES[spinner_index(true, 0, SPINNER_FRAMES.len())],
            '⠋'
        );
        assert_eq!(
            SPINNER_STRS[spinner_index(true, 9_999, SPINNER_STRS.len())],
            "⠋ "
        );
    }

    #[test]
    fn reduced_motion_push_reveals_everything_at_once() {
        let mut tw = Typewriter::new();
        tw.set_reduced_motion(true);
        tw.push("hello world, this is a longer string");
        assert_eq!(tw.visible(), "hello world, this is a longer string");
        assert!(!tw.is_animating(), "nothing left to animate");
    }

    #[test]
    fn reduced_motion_tick_finishes_an_in_flight_reveal() {
        let mut tw = Typewriter::new();
        tw.push("hello world, this is a longer string");
        assert!(tw.is_animating(), "starts mid-reveal at full motion");
        assert_eq!(tw.visible(), "");

        tw.set_reduced_motion(true);
        assert_eq!(tw.visible(), "hello world, this is a longer string");
        assert!(!tw.is_animating());

        tw.tick();
        assert_eq!(tw.visible(), "hello world, this is a longer string");
    }

    #[test]
    fn reduced_motion_handles_multibyte_and_repeated_pushes() {
        let mut tw = Typewriter::new();
        tw.set_reduced_motion(true);
        tw.push("héllo ");
        tw.push("🌍中");
        assert_eq!(tw.visible(), "héllo 🌍中");
        assert_eq!(tw.visible_byte_offset(), tw.visible().len());
        assert!(!tw.is_animating());

        tw.clear();
        tw.push("again 🦀");
        assert_eq!(tw.visible(), "again 🦀");
    }

    #[test]
    fn turning_reduced_motion_off_restores_animation() {
        let mut tw = Typewriter::new();
        tw.set_reduced_motion(true);
        tw.set_reduced_motion(false);
        tw.push("hello world, this is a longer string");
        assert_eq!(tw.visible(), "");
        assert!(tw.is_animating());
    }

    #[test]
    fn spinner_wraps_around() {
        let first = spinner_frame(0);
        let wrapped = spinner_frame(SPINNER_FRAME_MS * SPINNER_FRAMES.len() as u128);
        assert_eq!(first, wrapped);
        assert_ne!(first, spinner_frame(SPINNER_FRAME_MS));
    }

    #[test]
    fn push_animates_and_empty_push_is_noop() {
        let mut tw = Typewriter::new();
        tw.push("");
        assert!(!tw.is_animating());
        assert!(tw.is_empty());

        tw.push("hello world, this is a longer string");
        assert_eq!(tw.visible(), "");
        assert!(tw.is_animating());
    }

    #[test]
    fn set_buffer_makes_everything_visible() {
        let mut tw = Typewriter::new();
        tw.set_buffer("héllo 🌍");
        assert_eq!(tw.visible(), "héllo 🌍");
        assert!(!tw.is_animating());
    }

    #[test]
    fn extend_preserves_visible_and_animates_new() {
        let mut tw = Typewriter::new();
        tw.set_buffer("ab");
        tw.push("cdefghijklmnop");
        assert_eq!(tw.visible(), "ab");
        assert!(tw.is_animating());
    }

    #[test]
    fn zero_speed_sequential_pushes_multibyte() {
        let mut tw = Typewriter::with_speed(0);
        tw.push("a");
        tw.push("é");
        tw.push("中");
        tw.push("🦀");
        assert_eq!(tw.visible(), "aé中🦀");
        assert!(!tw.is_animating());
    }

    #[test]
    fn clear_and_take_all_reset_byte_offset() {
        let mut tw = Typewriter::with_speed(0);

        tw.push("🔥🔥🔥");
        assert_eq!(tw.visible(), "🔥🔥🔥");
        tw.clear();
        assert!(tw.is_empty());
        assert_eq!(tw.visible(), "");

        tw.push("日本語");
        assert_eq!(tw.visible(), "日本語");
        let taken = tw.take_all();
        assert_eq!(taken, "日本語");
        assert!(tw.is_empty());
        assert_eq!(tw.visible(), "");

        tw.push("ok");
        assert_eq!(tw.visible(), "ok");
    }

    #[test]
    fn set_buffer_then_push_multibyte() {
        let mut tw = Typewriter::with_speed(0);
        tw.set_buffer("àá");
        tw.push("â🎉ã");
        assert_eq!(tw.visible(), "àáâ🎉ã");
    }

    #[test]
    fn repeated_clear_push_cycles() {
        let mut tw = Typewriter::with_speed(0);
        for _ in 0..3 {
            tw.push("🎵test🎵");
            assert_eq!(tw.visible(), "🎵test🎵");
            tw.clear();
            assert_eq!(tw.visible(), "");
        }
    }

    #[test]
    fn partial_eq_compares_full_buffer() {
        let mut tw = Typewriter::new();
        tw.push("hello world, this is enough text");
        assert_eq!(tw, "hello world, this is enough text");
        assert_eq!(tw.visible(), "");
    }
}
