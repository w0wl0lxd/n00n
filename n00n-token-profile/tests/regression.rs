use std::path::PathBuf;

use n00n_token_profile::{
    Baseline, SurfaceLimit, SurfaceMetric, SurfaceName, assert_within_baseline, profile_cold_start,
};

fn baseline_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("baselines/cold_start.json")
}

#[test]
fn cold_start_profile_includes_hard_gate_surfaces() {
    let report = profile_cold_start().expect("cold-start profile should build offline");
    assert!(report.mcp_excluded);
    assert_eq!(report.model_id, n00n_token_profile::FIXTURE_MODEL_ID);

    let schemas = report
        .surface(SurfaceName::MainToolsSchemas)
        .expect("main_tools_schemas");
    let tool_count = schemas.tool_count.expect("schemas tool_count");
    assert!(tool_count > 0, "expected registered main tools");
    assert!(schemas.bytes > 0);
    assert!(schemas.tokens > 0);

    let system = report
        .surface(SurfaceName::SystemPrompt)
        .expect("system_prompt");
    assert!(system.bytes > 0);
    assert!(system.tokens > 0);
}

#[test]
fn cold_start_stays_within_committed_baseline() {
    let report = profile_cold_start().expect("cold-start profile should build offline");
    let baseline = Baseline::load(&baseline_path()).expect("committed baseline must exist");
    assert_within_baseline(&report, &baseline).unwrap_or_else(|e| {
        panic!(
            "token regression: {e}\n\
             If this growth is intentional, regenerate baselines/cold_start.json in the same PR."
        )
    });
}

#[test]
fn assert_within_baseline_rejects_token_growth_past_limit() {
    let mut report = profile_cold_start().expect("profile");
    let baseline = Baseline::load(&baseline_path()).expect("baseline");
    let Some(metric) = report
        .surfaces
        .iter_mut()
        .find(|s| s.name == SurfaceName::MainToolsSchemas)
    else {
        panic!("missing schemas surface");
    };
    metric.tokens = metric.tokens.saturating_add(10_000);
    let err = assert_within_baseline(&report, &baseline).expect_err("should breach");
    let msg = err.to_string();
    assert!(
        msg.contains("main_tools_schemas") && msg.contains("tokens"),
        "unexpected error: {msg}"
    );
}

#[test]
fn surface_limit_defaults_deserialize() {
    let limit: SurfaceLimit = serde_json::from_str(
        r#"{"max_token_delta": 100, "max_byte_delta": 400, "tool_count_exact": true}"#,
    )
    .expect("limit json");
    assert!(limit.tool_count_exact);
    assert_eq!(limit.max_token_delta, Some(100));
    assert_eq!(limit.warn_token_delta, None);

    let metric = SurfaceMetric {
        name: SurfaceName::SystemPrompt,
        tool_count: None,
        bytes: 1,
        tokens: 2,
    };
    assert_eq!(metric.tokens, 2);
}
