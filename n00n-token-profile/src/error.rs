use thiserror::Error;

use crate::metrics::SurfaceName;

#[derive(Debug, Error)]
pub enum ProfileError {
    #[error("model '{0}' is not resolvable")]
    Model(String),
    #[error("plugin host failed: {0}")]
    PluginHost(String),
    #[error("failed to serialize surface '{surface}': {source}")]
    Serialize {
        surface: SurfaceName,
        #[source]
        source: serde_json::Error,
    },
    #[error("surface '{0}' missing from profile report")]
    MissingSurface(SurfaceName),
}

#[derive(Debug, Error)]
pub enum RegressionError {
    #[error("baseline parse failed: {0}")]
    BaselineParse(#[from] serde_json::Error),
    #[error("baseline I/O failed: {0}")]
    BaselineIo(#[from] std::io::Error),
    #[error("{0}")]
    Breach(String),
    #[error(transparent)]
    Profile(#[from] ProfileError),
}
