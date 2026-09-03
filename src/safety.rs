//! Confirmation and dry-run controls for the destructive subcommands.
//!
//! `rollback`, `auth logout`, `mcp logout` and `agent stop` all destroy state
//! that the operator cannot get back. Each one takes [`SafetyFlags`] and routes
//! through [`allow`] before it touches anything.
//!
//! The prompt keeps the semantics the updater already had: only a literal `y`
//! approves. A closed or piped stdin reads as end-of-file, which leaves the
//! answer empty and declines. Non-interactive callers therefore abort by
//! default and must pass `--no-confirm` to proceed.

use std::io::Write;

use crate::cli::SafetyFlags;

/// What the safety flags ask the command to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// `--dry-run`: report the target and change nothing.
    DryRun,
    /// `--no-confirm`: proceed without asking.
    Proceed,
    /// Ask the operator first.
    Ask,
}

/// Map the flags to a decision. `--dry-run` wins over `--no-confirm`, so a
/// caller that passes both still changes nothing.
pub fn decide(flags: SafetyFlags) -> Decision {
    if flags.dry_run {
        Decision::DryRun
    } else if flags.no_confirm {
        Decision::Proceed
    } else {
        Decision::Ask
    }
}

/// Ask a yes/no question on stderr. Only a literal `y` approves.
pub fn confirm(question: &str) -> bool {
    eprint!("{question} [y/N] ");
    let _ = std::io::stderr().flush();
    let mut input = String::new();
    std::io::stdin().read_line(&mut input).is_ok() && input.trim().eq_ignore_ascii_case("y")
}

/// Gate a destructive command.
///
/// `action` is a lowercase verb phrase naming what the command destroys, for
/// example `remove stored credentials for 'openai'`. Returns `true` only when
/// the caller should go ahead.
pub fn allow(flags: SafetyFlags, action: &str) -> bool {
    match decide(flags) {
        Decision::DryRun => {
            println!("Dry run: would {action}.");
            println!("Nothing was changed.");
            false
        }
        Decision::Proceed => true,
        Decision::Ask => {
            if confirm(&format!("Really {action}?")) {
                true
            } else {
                println!("Aborted.");
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flags(dry_run: bool, no_confirm: bool) -> SafetyFlags {
        SafetyFlags {
            dry_run,
            no_confirm,
        }
    }

    #[test]
    fn bare_flags_ask_the_operator() {
        assert_eq!(decide(flags(false, false)), Decision::Ask);
    }

    #[test]
    fn no_confirm_proceeds_without_asking() {
        assert_eq!(decide(flags(false, true)), Decision::Proceed);
    }

    #[test]
    fn dry_run_reports_without_changing() {
        assert_eq!(decide(flags(true, false)), Decision::DryRun);
    }

    #[test]
    fn dry_run_wins_over_no_confirm() {
        assert_eq!(decide(flags(true, true)), Decision::DryRun);
    }

    #[test]
    fn allow_is_false_for_dry_run() {
        assert!(!allow(flags(true, false), "delete everything"));
        assert!(!allow(flags(true, true), "delete everything"));
    }

    #[test]
    fn allow_is_true_for_no_confirm() {
        assert!(allow(flags(false, true), "delete everything"));
    }

    #[test]
    fn default_flags_are_the_safe_ones() {
        let default = SafetyFlags::default();
        assert!(!default.dry_run);
        assert!(!default.no_confirm);
        assert_eq!(decide(default), Decision::Ask);
    }
}
