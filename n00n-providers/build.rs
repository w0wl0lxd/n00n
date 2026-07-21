use std::process::Command;

fn main() {
    let hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout).ok()
            } else {
                None
            }
        })
        .map_or("unknown".to_string(), |s| s.trim().to_string());
    println!("cargo:rustc-env=GIT_SHORT_HASH={hash}");
}
