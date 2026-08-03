Use `ratatui::try_init()` in `TerminalGuard::init` so the TUI returns a clean `io::Error` instead of panicking when it is started without a TTY (e.g. in non-interactive subagents or CI).
