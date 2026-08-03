use std::process::Command;

#[test]
fn tui_refuses_to_run_without_a_terminal() {
    let state_dir = std::env::temp_dir().join(format!("n00n-non-tty-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state_dir);
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
    assert_ne!(
        code, 101,
        "n00n should not panic-exit (101) without a TTY:\nstderr:\n{stderr}\nstdout:\n{stdout}"
    );

    assert!(
        stderr.contains("terminal") || code == 1,
        "n00n should report a terminal error or exit code 1 without a TTY:\nstderr:\n{stderr}\nstdout:\n{stdout}"
    );
}
