use std::time::Duration;

const INITIAL_DELAY: Duration = Duration::from_secs(2);
const BACKOFF_FACTOR: u32 = 2;
const MAX_DELAY: Duration = Duration::from_secs(30);

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
        let delay = (INITIAL_DELAY * BACKOFF_FACTOR.pow(self.attempt - 1)).min(MAX_DELAY);
        (self.attempt, delay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_increases_monotonically_and_caps_at_max() {
        let mut state = RetryState::new();
        let mut prev = Duration::ZERO;

        for i in 1..=8 {
            let (attempt, delay) = state.next_delay();
            assert_eq!(attempt, i);
            assert!(delay >= prev, "delay must be monotonically non-decreasing");
            assert!(delay <= MAX_DELAY, "delay must never exceed MAX_DELAY");
            prev = delay;
        }

        assert_eq!(state.next_delay().1, MAX_DELAY);
    }
}
