use std::process::Command;

const TERMINAL_ERROR: &str =
    "n00n must be run from a terminal; use --print for non-interactive output";
const FAILURE_EXIT_CODE: i32 = 1;

#[test]
fn tui_refuses_to_run_without_a_terminal() {
    let state_dir = std::env::temp_dir().join(format!("n00n-non-tty-test-{}", std::process::id()));
    match std::fs::remove_dir_all(&state_dir) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => panic!("remove temp state dir: {error}"),
    }
    std::fs::create_dir_all(&state_dir).expect("create temp state dir");

    let output = Command::new(env!("CARGO_BIN_EXE_n00n"))
        .args(["--model", "synthetic/test", "--yolo", "--max-turns", "0"])
        .env("N00N_STATE_DIR", &state_dir)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("spawn n00n");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        !stderr.contains("panic") && !stdout.contains("panic"),
        "n00n should not panic without a TTY:\nstderr:\n{stderr}\nstdout:\n{stdout}"
    );

    let code = output.status.code().expect("n00n should exit cleanly");
    assert_eq!(
        code, FAILURE_EXIT_CODE,
        "n00n should reject non-TTY startup with exit code {FAILURE_EXIT_CODE}:\nstderr:\n{stderr}\nstdout:\n{stdout}"
    );
    assert!(
        stderr.contains(TERMINAL_ERROR),
        "n00n should report the terminal error:\nstderr:\n{stderr}\nstdout:\n{stdout}"
    );
}
