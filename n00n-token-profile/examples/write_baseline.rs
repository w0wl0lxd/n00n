use std::collections::BTreeMap;
use std::path::PathBuf;

use n00n_token_profile::{Baseline, SurfaceLimit, SurfaceName, profile_cold_start};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let report = profile_cold_start()?;
    let mut surfaces = BTreeMap::new();
    for metric in &report.surfaces {
        surfaces.insert(metric.name.to_string(), metric.clone());
    }
    let mut limits = BTreeMap::new();
    limits.insert(
        SurfaceName::MainToolsSchemas.to_string(),
        SurfaceLimit {
            tool_count_exact: true,
            max_token_delta: Some(100),
            max_byte_delta: Some(400),
            warn_token_delta: None,
        },
    );
    limits.insert(
        SurfaceName::SystemPrompt.to_string(),
        SurfaceLimit {
            tool_count_exact: false,
            max_token_delta: Some(80),
            max_byte_delta: Some(320),
            warn_token_delta: None,
        },
    );
    limits.insert(
        SurfaceName::MainToolsPayload.to_string(),
        SurfaceLimit {
            tool_count_exact: false,
            max_token_delta: None,
            max_byte_delta: None,
            warn_token_delta: Some(200),
        },
    );
    limits.insert(
        SurfaceName::CachePrefix.to_string(),
        SurfaceLimit {
            tool_count_exact: false,
            max_token_delta: None,
            max_byte_delta: None,
            warn_token_delta: Some(200),
        },
    );
    let baseline = Baseline {
        model_id: report.model_id.clone(),
        mcp_excluded: report.mcp_excluded,
        surfaces,
        limits,
    };
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("baselines/cold_start.json");
    let json = serde_json::to_string_pretty(&baseline)?;
    std::fs::write(&path, format!("{json}\n"))?;
    println!("wrote {}", path.display());
    for s in &report.surfaces {
        println!(
            "{:?}: tools={:?} bytes={} tokens={}",
            s.name, s.tool_count, s.bytes, s.tokens
        );
    }
    Ok(())
}
