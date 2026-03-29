use std::future::Future;
use std::pin::Pin;

use flume::Sender;
use serde_json::Value;
use strum::{Display, EnumIter, EnumString, IntoEnumIterator};
use tracing::{debug, warn};

use crate::model::{Model, models_for_provider};
use crate::providers::anthropic::Anthropic;
use crate::providers::dynamic;
use crate::providers::openai::OpenAi;
use crate::providers::synthetic::Synthetic;
use crate::providers::zai::{Zai, ZaiPlan};
use crate::{AgentError, Message, ProviderEvent, StreamResponse, ThinkingConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Display, EnumString, EnumIter)]
#[strum(serialize_all = "kebab-case")]
pub enum ProviderKind {
    Anthropic,
    #[strum(serialize = "openai")]
    OpenAi,
    Zai,
    ZaiCodingPlan,
    Synthetic,
}

impl ProviderKind {
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Anthropic => "Anthropic",
            Self::OpenAi => "OpenAI",
            Self::Zai => "Z.AI",
            Self::ZaiCodingPlan => "Z.AI Coding",
            Self::Synthetic => "Synthetic",
        }
    }

    pub const fn supports_thinking(self) -> bool {
        matches!(self, Self::Anthropic | Self::Synthetic)
    }

    pub fn create(self) -> Result<Box<dyn Provider>, AgentError> {
        match self {
            Self::Anthropic => Ok(Box::new(Anthropic::new()?)),
            Self::OpenAi => Ok(Box::new(OpenAi::new()?)),
            Self::Zai => Ok(Box::new(Zai::new(ZaiPlan::Standard)?)),
            Self::ZaiCodingPlan => Ok(Box::new(Zai::new(ZaiPlan::Coding)?)),
            Self::Synthetic => Ok(Box::new(Synthetic::new()?)),
        }
    }

    pub fn is_available(self) -> bool {
        self.create().is_ok()
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
        thinking: ThinkingConfig,
    ) -> BoxFuture<'a, Result<StreamResponse, AgentError>>;

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<String>, AgentError>>;

    fn refresh_auth(&self) -> BoxFuture<'_, Result<(), AgentError>> {
        Box::pin(async { Ok(()) })
    }
}

pub fn from_model(model: &Model) -> Result<Box<dyn Provider>, AgentError> {
    if let Some(slug) = &model.dynamic_slug {
        let provider = dynamic::create(slug)?;
        debug!(slug, model = %model.id, "dynamic provider created");
        return Ok(provider);
    }
    let provider = model.provider.create()?;
    debug!(provider = %model.provider, model = %model.id, "provider created");
    Ok(provider)
}

pub async fn from_model_async(model: &Model) -> Result<Box<dyn Provider>, AgentError> {
    let slug = model.dynamic_slug.clone();
    let kind = model.provider;
    let id = model.id.clone();
    let provider = smol::unblock(move || {
        if let Some(slug) = &slug {
            dynamic::create(slug)
        } else {
            kind.create()
        }
    })
    .await?;
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
                    warn!(provider = %kind, error = %e, "failed to list models, using static fallback");
                    let fallback: Vec<String> = models_for_provider(kind)
                        .iter()
                        .flat_map(|entry| entry.prefixes.iter())
                        .map(|p| format!("{kind}/{p}"))
                        .collect();
                    ModelBatch {
                        models: fallback,
                        warnings: vec![format!(
                            "{}: {e} (using static fallback)",
                            kind.display_name()
                        )],
                    }
                }
            };
            let _ = tx.send_async(batch).await;
        })
        .detach();
    }

    let dynamic_specs = dynamic::dynamic_model_specs();
    if !dynamic_specs.is_empty() {
        let _ = tx.send(ModelBatch {
            models: dynamic_specs,
            warnings: Vec::new(),
        });
    }

    drop(tx);

    while let Ok(batch) = rx.recv_async().await {
        on_ready(batch);
    }
}
