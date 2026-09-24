use std::time::Duration;

const DELAY: Duration = Duration::from_secs(2);
const MAX_DELAY: Duration = Duration::from_secs(8);
pub const MAX_RETRIES: u32 = 3;

#[derive(Default)]
pub struct RetryState {
    attempt: u32,
}

impl RetryState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn next_delay(&mut self) -> (u32, Duration) {
        self.attempt += 1;
        let delay = (DELAY.saturating_mul(self.attempt)).min(MAX_DELAY);
        let half = delay / 2;
        let jitter = Duration::from_millis(fastrand::u64(
            0..=u64::try_from(half.as_millis()).unwrap_or_else(|_| u64::MAX),
        ));
        (self.attempt, half + jitter)
    }
}
