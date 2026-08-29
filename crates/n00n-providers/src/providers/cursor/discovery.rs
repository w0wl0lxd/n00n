//! Cursor API2 discovery for native HTTP/2 Connect (Phase 1).
//!
//! This module is not yet wired into the Cursor provider; it's prepared for
//! future native integration to replace the cursor-agent subprocess approach.

#![allow(dead_code)]

use std::time::Duration;

use isahc::ReadResponseExt;
use isahc::config::Configurable;
use serde::Deserialize;

use crate::AgentError;

use super::wire::{CLIENT_TYPE, CLIENT_VERSION, CONNECT_PROTOCOL_VERSION};

fn validate_agent_url(url: &str) -> Result<(), AgentError> {
    let parsed = url::Url::parse(url).map_err(|error| AgentError::Config {
        message: format!("cursor agent URL parse: {error}"),
    })?;
    if parsed.scheme() != "https" {
        return Err(AgentError::Config {
            message: format!("cursor agent URL must use HTTPS, got: {}", parsed.scheme()),
        });
    }
    let host = parsed.host_str().ok_or_else(|| AgentError::Config {
        message: "cursor agent URL missing host".into(),
    })?;
    if !host.ends_with(".cursor.sh") && !host.ends_with(".cursor.com") {
        return Err(AgentError::Config {
            message: format!(
                "cursor agent URL domain must be *.cursor.sh or *.cursor.com, got: {host}"
            ),
        });
    }
    Ok(())
}

const API2_BASE: &str = "https://api2.cursor.sh";
const GET_USABLE_MODELS_PATH: &str = "/agent.v1.AgentService/GetUsableModels";
const GET_SERVER_CONFIG_PATH: &str = "/aiserver.v1.ServerConfigService/GetServerConfig";
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(30);
const EMPTY_JSON_BODY: &[u8] = b"{}";
const SERVER_CONFIG_BODY: &[u8] = br#"{"telemEnabled":false}"#;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UsableModel {
    pub model_id: String,
    pub display_model_id: String,
    pub display_name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetUsableModelsResponse {
    models: Vec<UsableModel>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetServerConfigResponse {
    agent_url_config: AgentUrlConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentUrlConfig {
    agent_url: String,
    agentn_url: String,
}

fn discovery_client() -> Result<isahc::HttpClient, AgentError> {
    isahc::HttpClient::builder()
        .timeout(DISCOVERY_TIMEOUT)
        .build()
        .map_err(|error| AgentError::Config {
            message: format!("cursor discovery http client: {error}"),
        })
}

fn cursor_json_request(
    path: &str,
    token: &str,
    body: &[u8],
) -> Result<isahc::Request<Vec<u8>>, AgentError> {
    let url = format!("{API2_BASE}{path}");
    isahc::Request::builder()
        .method("POST")
        .uri(url)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .header("connect-protocol-version", CONNECT_PROTOCOL_VERSION)
        .header("x-cursor-client-type", CLIENT_TYPE)
        .header("x-cursor-client-version", CLIENT_VERSION)
        .body(body.to_vec())
        .map_err(|error| AgentError::Config {
            message: format!("cursor discovery request: {error}"),
        })
}

fn send_json<T: for<'de> Deserialize<'de>>(
    path: &str,
    token: &str,
    body: &[u8],
) -> Result<T, AgentError> {
    let client = discovery_client()?;
    let request = cursor_json_request(path, token, body)?;
    let mut response = client.send(request).map_err(|error| AgentError::Api {
        status: 502,
        message: format!("cursor discovery request failed: {error}"),
    })?;
    let status = response.status().as_u16();
    let body_text = response.text().map_err(|error| AgentError::Api {
        status,
        message: format!("cursor discovery response body: {error}"),
    })?;
    if !(200..300).contains(&status) {
        return Err(AgentError::Api {
            status,
            message: body_text,
        });
    }
    serde_json::from_str(&body_text).map_err(|error| AgentError::Api {
        status,
        message: format!("cursor discovery json decode: {error}"),
    })
}

pub(crate) fn fetch_usable_models(token: &str) -> Result<Vec<UsableModel>, AgentError> {
    let response: GetUsableModelsResponse =
        send_json(GET_USABLE_MODELS_PATH, token, EMPTY_JSON_BODY)?;
    Ok(response.models)
}

pub(crate) fn fetch_agent_base_url(token: &str) -> Result<String, AgentError> {
    let response: GetServerConfigResponse =
        send_json(GET_SERVER_CONFIG_PATH, token, SERVER_CONFIG_BODY)?;
    let url = response.agent_url_config.agentn_url;
    if url.is_empty() {
        return Err(AgentError::Api {
            status: 502,
            message: "cursor GetServerConfig returned empty agentnUrl".into(),
        });
    }
    validate_agent_url(&url)?;
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIVE_ENV: &str = "N00N_CURSOR_LIVE_TESTS";

    fn live_enabled() -> bool {
        std::env::var(LIVE_ENV)
            .is_ok_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
    }

    #[test]
    fn fetch_usable_models_live() {
        if !live_enabled() {
            return;
        }
        let token = super::super::auth::read_ide_access_token().expect("IDE token");
        let models = fetch_usable_models(&token).expect("GetUsableModels");
        assert!(!models.is_empty());
        assert!(models.iter().any(|model| {
            model.model_id == "default"
                && model.display_model_id == "auto"
                && model.display_name.contains("Auto")
        }));
    }

    #[test]
    fn fetch_agent_base_url_live() {
        if !live_enabled() {
            return;
        }
        let token = super::super::auth::read_ide_access_token().expect("IDE token");
        let url = fetch_agent_base_url(&token).expect("GetServerConfig");
        assert!(url.contains("agentn."));
        assert!(url.contains("api5.cursor.sh"));
    }
}
