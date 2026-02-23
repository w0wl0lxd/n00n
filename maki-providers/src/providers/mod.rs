use std::time::Duration;

use ureq::Agent;

pub(crate) mod anthropic;
pub(crate) mod zai;

pub use anthropic::auth;

pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const RECV_TIMEOUT: Duration = Duration::from_secs(300);

pub(crate) fn streaming_agent() -> Agent {
    Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_recv_response(Some(RECV_TIMEOUT))
        .timeout_recv_body(Some(RECV_TIMEOUT))
        .build()
        .into()
}
