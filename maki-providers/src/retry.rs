use std::time::Duration;

const DELAY: Duration = Duration::from_secs(2);
const MAX_DELAY: Duration = Duration::from_secs(6);
pub const MAX_TIMEOUT_RETRIES: u32 = 10;

#[derive(Default)]
pub struct RetryState {
    attempt: u32,
}

impl RetryState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn next_delay(&mut self) -> (u32, Duration) {
        self.attempt += 1;
        let delay = (DELAY.saturating_mul(self.attempt)).min(MAX_DELAY);
        (self.attempt, delay)
    }
}
