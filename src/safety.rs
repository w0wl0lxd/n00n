//! Confirmation and dry-run controls for the destructive subcommands.
//!
//! `rollback`, `auth logout`, `mcp logout` and `agent stop` all destroy state
//! that the operator cannot get back. Each one takes [`SafetyFlags`] and routes
//! through [`allow`] before it touches anything.
//!
//! The prompt keeps the semantics the updater already had: only a literal `y`
//! approves. Confirmation requires an interactive stdin, so automation must
//! pass `--no-confirm` explicitly.

use std::io::{self, BufRead, IsTerminal, Write};

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

#[derive(Debug, thiserror::Error)]
pub enum SafetyError {
    #[error(
        "destructive confirmation requires interactive stdin; rerun with --no-confirm to proceed"
    )]
    NonInteractive,

    #[error("failed to write destructive confirmation prompt to stderr: {0}")]
    WritePrompt(#[source] io::Error),

    #[error("failed to flush destructive confirmation prompt to stderr: {0}")]
    FlushPrompt(#[source] io::Error),

    #[error("failed to read destructive confirmation response from stdin: {0}")]
    ReadResponse(#[source] io::Error),
}

fn confirm_with_io(
    question: &str,
    input: &mut impl BufRead,
    prompt: &mut impl Write,
    stdin_is_interactive: bool,
) -> Result<bool, SafetyError> {
    if !stdin_is_interactive {
        return Err(SafetyError::NonInteractive);
    }

    write!(prompt, "{question} [y/N] ").map_err(SafetyError::WritePrompt)?;
    prompt.flush().map_err(SafetyError::FlushPrompt)?;

    let mut response = String::new();
    input
        .read_line(&mut response)
        .map_err(SafetyError::ReadResponse)?;
    Ok(response.trim().eq_ignore_ascii_case("y"))
}

/// Ask a yes/no question on stderr. Only a literal `y` approves.
pub fn confirm(question: &str) -> Result<bool, SafetyError> {
    let stdin = io::stdin();
    let stdin_is_interactive = stdin.is_terminal();
    let mut input = stdin.lock();
    let stderr = io::stderr();
    let mut prompt = stderr.lock();
    confirm_with_io(question, &mut input, &mut prompt, stdin_is_interactive)
}

/// Gate a destructive command.
///
/// `action` is a lowercase verb phrase naming what the command destroys, for
/// example `remove stored credentials for 'openai'`. Returns `true` only when
/// the caller should go ahead.
pub fn allow(flags: SafetyFlags, action: &str) -> Result<bool, SafetyError> {
    match decide(flags) {
        Decision::DryRun => {
            println!("Dry run: would {action}.");
            println!("Nothing was changed.");
            Ok(false)
        }
        Decision::Proceed => Ok(true),
        Decision::Ask => {
            if confirm(&format!("Really {action}?"))? {
                Ok(true)
            } else {
                println!("Aborted.");
                Ok(false)
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
        assert!(!allow(flags(true, false), "delete everything").expect("dry-run should succeed"));
        assert!(!allow(flags(true, true), "delete everything").expect("dry-run should win"));
    }

    #[test]
    fn allow_is_true_for_no_confirm() {
        assert!(allow(flags(false, true), "delete everything").expect("bypass should succeed"));
    }

    #[test]
    fn default_flags_are_the_safe_ones() {
        let default = SafetyFlags::default();
        assert!(!default.dry_run);
        assert!(!default.no_confirm);
        assert_eq!(decide(default), Decision::Ask);
    }

    #[test]
    fn piped_yes_is_rejected_without_reading_input() {
        let mut input = std::io::Cursor::new(b"y\n");
        let mut prompt = Vec::new();

        let error = confirm_with_io("Delete it?", &mut input, &mut prompt, false)
            .expect_err("non-interactive input must be rejected");

        assert!(matches!(error, SafetyError::NonInteractive));
        assert_eq!(
            error.to_string(),
            "destructive confirmation requires interactive stdin; rerun with --no-confirm to proceed"
        );
        assert_eq!(input.position(), 0);
        assert!(prompt.is_empty());
    }

    #[test]
    fn interactive_literal_y_confirms() {
        let mut input = std::io::Cursor::new(b"y\n");
        let mut prompt = Vec::new();

        let confirmed = confirm_with_io("Delete it?", &mut input, &mut prompt, true)
            .expect("interactive confirmation should succeed");

        assert!(confirmed);
        assert_eq!(prompt, b"Delete it? [y/N] ");
    }

    struct FlushFailingWriter;

    impl std::io::Write for FlushFailingWriter {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            Ok(buffer.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::other("broken stderr"))
        }
    }

    #[test]
    fn stderr_flush_failure_is_a_typed_helpful_error() {
        let mut input = std::io::Cursor::new(b"y\n");
        let mut prompt = FlushFailingWriter;

        let error = confirm_with_io("Delete it?", &mut input, &mut prompt, true)
            .expect_err("flush failure must be reported");

        assert!(matches!(error, SafetyError::FlushPrompt(_)));
        assert_eq!(
            error.to_string(),
            "failed to flush destructive confirmation prompt to stderr: broken stderr"
        );
    }
}
