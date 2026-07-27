//! Cursor wire-level identifiers and header constants.

pub(crate) const CLIENT_TYPE: &str = "cli";
pub(crate) const CLIENT_VERSION: &str = "cli-2026.07.26-77e48ba";
pub(crate) const CONNECT_PROTOCOL_VERSION: &str = "1";
#[allow(dead_code)] // used by Run client in Phase 1
pub(crate) const CONNECT_CONTENT_TYPE: &str = "application/connect+proto";

/// Map n00n/display model ids to `AgentService/Run` wire ids.
pub(crate) fn wire_model_id(display_id: &str) -> &str {
    match display_id {
        "auto" => "default",
        _ => display_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test_case("auto", "default")]
    #[test_case("default", "default")]
    #[test_case("composer-2.5", "composer-2.5")]
    fn wire_model_id_maps(display: &str, wire: &str) {
        assert_eq!(wire_model_id(display), wire);
    }
}
