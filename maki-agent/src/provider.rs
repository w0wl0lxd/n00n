use std::sync::mpsc::Sender;
use std::thread;

use serde_json::Value;
use strum::{Display, EnumIter, EnumString, IntoEnumIterator};

use crate::model::Model;
use crate::{AgentError, AgentEvent, Message, StreamResponse};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Display, EnumString, EnumIter)]
#[strum(serialize_all = "lowercase")]
pub enum ProviderKind {
    Anthropic,
    Zai,
}

impl ProviderKind {
    fn create(self) -> Result<Box<dyn Provider>, AgentError> {
        match self {
            Self::Anthropic => Ok(Box::new(crate::anthropic::Anthropic::new()?)),
            Self::Zai => Ok(Box::new(crate::zai::Zai::new()?)),
        }
    }
}

pub trait Provider: Send + Sync {
    fn stream_message(
        &self,
        model: &Model,
        messages: &[Message],
        system: &str,
        tools: &Value,
        event_tx: &Sender<AgentEvent>,
    ) -> Result<StreamResponse, AgentError>;

    fn list_models(&self) -> Result<Vec<String>, AgentError>;
}

pub fn from_model(model: &Model) -> Result<Box<dyn Provider>, AgentError> {
    model.provider.create()
}

pub fn fetch_all_models(mut on_ready: impl FnMut(Vec<String>)) {
    let (tx, rx) = std::sync::mpsc::channel();

    for kind in ProviderKind::iter() {
        let Ok(provider) = kind.create() else {
            continue;
        };
        let tx = tx.clone();
        thread::spawn(move || {
            let models = match provider.list_models() {
                Ok(ids) => ids.into_iter().map(|id| format!("{kind}/{id}")).collect(),
                Err(e) => {
                    eprintln!("warning: {kind}: {e}");
                    Vec::new()
                }
            };
            let _ = tx.send(models);
        });
    }
    drop(tx);

    while let Ok(models) = rx.recv() {
        on_ready(models);
    }
}
