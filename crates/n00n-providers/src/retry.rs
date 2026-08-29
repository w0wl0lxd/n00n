use std::time::Duration;

const DELAY: Duration = Duration::from_secs(2);
const MAX_DELAY: Duration = Duration::from_secs(8);
const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);
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
        let exp = 1u32 << self.attempt.saturating_sub(1).min(3);
        let delay = DELAY.saturating_mul(exp).min(MAX_DELAY);
        let half = delay / 2;
        let jitter = Duration::from_millis(fastrand::u64(
            0..=u64::try_from(half.as_millis()).unwrap_or_else(|_| u64::MAX),
        ));
        (self.attempt, half + jitter)
    }

    pub fn next_delay_with_retry_after(
        &mut self,
        retry_after: Option<Duration>,
    ) -> (u32, Duration) {
        if let Some(delay) = retry_after {
            let capped = delay.min(MAX_RETRY_AFTER);
            self.attempt += 1;
            let half = capped / 2;
            let jitter = Duration::from_millis(fastrand::u64(
                0..=u64::try_from(half.as_millis()).unwrap_or_else(|_| u64::MAX),
            ));
            return (self.attempt, (half + jitter).min(capped));
        }
        self.next_delay()
    }

    #[must_use]
    pub fn capped_retry_after(delay: Duration) -> Duration {
        delay.min(MAX_RETRY_AFTER)
    }

    #[must_use]
    pub fn parse_retry_after_header(value: Option<&str>) -> Option<Duration> {
        let value = value?.trim();
        if let Ok(secs) = value.parse::<u64>() {
            return Some(Duration::from_secs(secs).min(MAX_RETRY_AFTER));
        }
        if let Ok(deadline) = httpdate::parse_http_date(value) {
            let now = std::time::SystemTime::now();
            if let Ok(remaining) = deadline.duration_since(now) {
                return Some(remaining.min(MAX_RETRY_AFTER));
            }
            return Some(Duration::ZERO);
        }
        None
    }
}
