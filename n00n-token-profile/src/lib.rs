//! Offline cold-start token profiling for CI regression gates.
//!
//! Measures controllable per-turn surfaces (tool schemas, system prompt) without
//! live LLM calls. Token counts use n00n's tiktoken estimator, not provider billing.

mod baseline;
mod error;
mod fixture;
mod metrics;

pub use baseline::{Baseline, SurfaceLimit, assert_within_baseline};
pub use error::{ProfileError, RegressionError};
pub use fixture::{FIXTURE_MODEL_ID, profile_cold_start};
pub use metrics::{ProfileReport, SurfaceMetric, SurfaceName};
