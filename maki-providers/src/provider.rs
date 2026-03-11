use std::future::Future;
use std::pin::Pin;

use flume::Sender;
use serde_json::Value;
use strum::{Display, EnumIter, EnumString, IntoEnumIterator};
use tracing::{debug, warn};

use crate::model::Model;
use crate::providers::zai::{Zai, ZaiPlan};
use crate::{AgentError, Message, ProviderEvent, StreamResponse};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Display, EnumString, EnumIter)]
#[strum(serialize_all = "kebab-case")]
pub enum ProviderKind {
    Anthropic,
    Zai,
    ZaiCodingPlan,
}

impl ProviderKind {
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Anthropic => "Anthropic",
            Self::Zai => "Z.AI",
            Self::ZaiCodingPlan => "Z.AI Coding",
        }
    }

    pub fn create(self) -> Result<Box<dyn Provider>, AgentError> {
        match self {
            Self::Anthropic => Ok(Box::new(crate::providers::anthropic::Anthropic::new()?)),
            Self::Zai => Ok(Box::new(Zai::new(ZaiPlan::Standard)?)),
            Self::ZaiCodingPlan => Ok(Box::new(Zai::new(ZaiPlan::Coding)?)),
        }
    }
}

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait Provider: Send + Sync {
    fn stream_message<'a>(
        &'a self,
        model: &'a Model,
        messages: &'a [Message],
        system: &'a str,
        tools: &'a Value,
        event_tx: &'a Sender<ProviderEvent>,
    ) -> BoxFuture<'a, Result<StreamResponse, AgentError>>;

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<String>, AgentError>>;
}

pub fn from_model(model: &Model) -> Result<Box<dyn Provider>, AgentError> {
    let provider = model.provider.create()?;
    debug!(provider = %model.provider, model = %model.id, "provider created");
    Ok(provider)
}

pub async fn from_model_async(model: &Model) -> Result<Box<dyn Provider>, AgentError> {
    let kind = model.provider;
    let id = model.id.clone();
    let provider = smol::unblock(move || kind.create()).await?;
    debug!(provider = %kind, model = %id, "provider created");
    Ok(provider)
}

pub struct ModelBatch {
    pub models: Vec<String>,
    pub warnings: Vec<String>,
}

pub async fn fetch_all_models(mut on_ready: impl FnMut(ModelBatch)) {
    let (tx, rx) = flume::unbounded();

    for kind in ProviderKind::iter() {
        let Ok(provider) = smol::unblock(move || kind.create()).await else {
            warn!(provider = %kind, "failed to create provider, skipping");
            continue;
        };
        let tx = tx.clone();
        smol::spawn(async move {
            let batch = match provider.list_models().await {
                Ok(ids) => ModelBatch {
                    models: ids.into_iter().map(|id| format!("{kind}/{id}")).collect(),
                    warnings: Vec::new(),
                },
                Err(e) => {
                    warn!(provider = %kind, error = %e, "failed to list models");
                    ModelBatch {
                        models: Vec::new(),
                        warnings: vec![format!("{}: {e}", kind.display_name())],
                    }
                }
            };
            let _ = tx.send_async(batch).await;
        })
        .detach();
    }
    drop(tx);

    while let Ok(batch) = rx.recv_async().await {
        on_ready(batch);
    }
}
