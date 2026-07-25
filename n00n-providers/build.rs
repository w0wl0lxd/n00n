use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;

fn fail(message: &str) -> ! {
    eprintln!("build error: {message}");
    process::exit(1);
}

fn main() {
    println!("cargo:rerun-if-changed=src/providers");

    let git_hash = env::var("GIT_SHORT_HASH").unwrap_or_else(|_| {
        let output = process::Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .output();
        match output {
            Ok(out) if out.status.success() => {
                String::from_utf8_lossy(&out.stdout).trim().to_string()
            }
            _ => "dev".to_string(),
        }
    });
    println!("cargo:rustc-env=GIT_SHORT_HASH={git_hash}");

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap_or_else(|_| fail("OUT_DIR not set")))
        .join("provider_configs");
    fs::create_dir_all(&out_dir)
        .unwrap_or_else(|e| fail(&format!("failed to create provider_configs directory: {e}")));

    let providers_dir = Path::new("src/providers");
    let entries = fs::read_dir(providers_dir)
        .unwrap_or_else(|e| fail(&format!("failed to read providers directory: {e}")));

    for entry in entries {
        let entry = entry.unwrap_or_else(|e| fail(&format!("failed to read directory entry: {e}")));
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("toml") {
            continue;
        }

        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_else(|| fail("invalid TOML file name"));
        let contents = fs::read_to_string(&path)
            .unwrap_or_else(|e| fail(&format!("failed to read {stem}.toml: {e}")));
        let table: toml::Table = contents
            .parse()
            .unwrap_or_else(|e| fail(&format!("failed to parse {stem}.toml: {e}")));

        let get_str = |key: &str| -> &str {
            table
                .get(key)
                .and_then(toml::Value::as_str)
                .unwrap_or_else(|| {
                    fail(&format!(
                        "missing or invalid string field '{key}' in {stem}.toml"
                    ))
                })
        };
        let get_bool = |key: &str| -> bool {
            table
                .get(key)
                .and_then(toml::Value::as_bool)
                .unwrap_or_else(|| {
                    fail(&format!(
                        "missing or invalid bool field '{key}' in {stem}.toml"
                    ))
                })
        };

        let const_name = get_str("const_name");
        let generated = format!(
            r#"static {const_name}: crate::providers::openai_compat::OpenAiCompatConfig = crate::providers::openai_compat::OpenAiCompatConfig {{
    slug: "{}",
    api_key_env: "{}",
    base_url: "{}",
    max_tokens_field: "{}",
    include_stream_usage: {},
    provider_name: "{}",
    supports_prompt_cache_key: {},
    supports_prompt_cache_breakpoint: {},
    emit_reasoning_content: {},
}};
"#,
            get_str("slug"),
            get_str("api_key_env"),
            get_str("base_url"),
            get_str("max_tokens_field"),
            get_bool("include_stream_usage"),
            get_str("provider_name"),
            get_bool("supports_prompt_cache_key"),
            get_bool("supports_prompt_cache_breakpoint"),
            get_bool("emit_reasoning_content"),
        );

        let out_file = out_dir.join(format!("{stem}.rs"));
        fs::write(&out_file, generated)
            .unwrap_or_else(|e| fail(&format!("failed to write generated provider config: {e}")));
    }
}
