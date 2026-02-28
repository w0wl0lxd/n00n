use std::borrow::Cow;
use std::env;
use std::time::SystemTime;

pub fn env_vars() -> Vars {
    let cwd = env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".into());
    let date = format_date(SystemTime::now());
    Vars::new()
        .set("{cwd}", cwd)
        .set("{platform}", env::consts::OS)
        .set("{date}", date)
}

fn format_date(now: SystemTime) -> String {
    let secs = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = secs / 86400;
    let mut y = 1970i32;
    let mut remaining = days;
    loop {
        let year_days = if is_leap(y) { 366 } else { 365 };
        if remaining < year_days {
            break;
        }
        remaining -= year_days;
        y += 1;
    }
    let leap = is_leap(y);
    let month_days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut m = 0;
    for &md in &month_days {
        if remaining < md {
            break;
        }
        remaining -= md;
        m += 1;
    }
    format!("{y:04}-{:02}-{:02}", m + 1, remaining + 1)
}

fn is_leap(y: i32) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

#[derive(Default)]
pub struct Vars(Vec<(&'static str, String)>);

impl Vars {
    pub fn new() -> Self {
        Self(Vec::new())
    }

    pub fn set(mut self, key: &'static str, val: impl Into<String>) -> Self {
        self.0.push((key, val.into()));
        self
    }

    pub fn apply<'a>(&self, s: &'a str) -> Cow<'a, str> {
        if self.0.is_empty() || !s.contains('{') {
            return Cow::Borrowed(s);
        }
        let mut out = s.to_string();
        for (k, v) in &self.0 {
            out = out.replace(k, v);
        }
        Cow::Owned(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test_case("{cwd} on {platform}", "/home on linux" ; "multiple_keys")]
    #[test_case("{x} and {x}", "42 and 42" ; "repeated_key")]
    #[test_case("no placeholders", "no placeholders" ; "no_placeholders")]
    fn apply(input: &str, expected: &str) {
        let vars = Vars::new()
            .set("{cwd}", "/home")
            .set("{platform}", "linux")
            .set("{x}", "42");
        assert_eq!(vars.apply(input).as_ref(), expected);
    }

    #[test_case(0,            "1970-01-01" ; "unix_epoch")]
    #[test_case(1_000_000_000, "2001-09-09" ; "billion_seconds")]
    #[test_case(1_740_700_800, "2025-02-28" ; "feb_28_non_leap")]
    #[test_case(1_709_164_800, "2024-02-29" ; "leap_day_2024")]
    fn format_date_cases(secs: u64, expected: &str) {
        let time = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs);
        assert_eq!(format_date(time), expected);
    }

    #[test]
    fn env_vars_includes_date() {
        let vars = env_vars();
        let result = vars.apply("{date}");
        assert_ne!(result.as_ref(), "{date}");
    }
}
