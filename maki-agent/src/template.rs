use std::borrow::Cow;
use std::env;

pub fn env_vars() -> Vars {
    let cwd = env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".into());
    Vars::new()
        .set("{cwd}", cwd)
        .set("{platform}", env::consts::OS)
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
}
