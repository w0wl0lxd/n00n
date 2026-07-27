//! On-device agent control plane: protocol, registry, worker sock proxy, UDS NDJSON.
//!
//! Hard gates (workspace + crate): no `unsafe`, no `unwrap`/`expect`/`panic`/`todo`/
//! `unimplemented`/`dbg` in production or tests. Prefer `Result` + typed `ControlError`.

#![forbid(unsafe_code)]

pub mod backend;
pub mod client;
pub mod error;
pub mod paths;
pub mod protocol;
pub mod registry;
pub mod server;

pub use error::{ControlError, ControlResult};
pub use protocol::{
    AgentRecord, BackendKind, ControlRequest, ControlResponse, MessageOpts, PROTOCOL_VERSION,
};
pub use registry::{ControlPlane, TuiCallbackBackend};
