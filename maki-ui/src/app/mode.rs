use std::borrow::Cow;
use std::mem;
use std::path::{Path, PathBuf};

use crate::theme;
use maki_agent::{AgentInput, AgentMode};
use maki_storage::plans;
use ratatui::style::{Color, Modifier, Style};

use super::App;
use super::queue::QueuedMessage;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Mode {
    Build,
    Plan { path: PathBuf, written: bool },
    BuildPlan,
}

impl Mode {
    pub(crate) fn color(&self) -> Color {
        match self {
            Self::Build => theme::current().mode_build,
            Self::Plan { .. } => theme::current().mode_plan,
            Self::BuildPlan => theme::current().mode_build_plan,
        }
    }

    pub(super) fn plan_path(&self) -> Option<&Path> {
        match self {
            Self::Plan { path, .. } => Some(path),
            _ => None,
        }
    }

    pub(super) fn mark_plan_written(&mut self) {
        if let Self::Plan { written, .. } = self {
            *written = true;
        }
    }
}

impl App {
    pub(super) fn toggle_mode(&mut self) -> Vec<super::Action> {
        self.mode = match mem::replace(&mut self.mode, Mode::Build) {
            Mode::BuildPlan => Mode::Build,
            Mode::Build => Mode::Plan {
                path: plans::new_plan_path(&self.storage)
                    .unwrap_or_else(|_| PathBuf::from("plans/plan.md")),
                written: false,
            },
            Mode::Plan { path, written } => {
                if written {
                    self.ready_plan = Some(path);
                }
                if self.ready_plan.is_some() {
                    Mode::BuildPlan
                } else {
                    Mode::Build
                }
            }
        };
        vec![]
    }

    pub(super) fn agent_mode(&self) -> AgentMode {
        match &self.mode {
            Mode::Plan { path, .. } => AgentMode::Plan(path.clone()),
            Mode::Build | Mode::BuildPlan => AgentMode::Build,
        }
    }

    pub(super) fn pending_plan(&self) -> Option<&Path> {
        match &self.mode {
            Mode::BuildPlan => self.ready_plan.as_deref(),
            _ => None,
        }
    }

    pub(crate) fn build_agent_input(&self, msg: &QueuedMessage) -> AgentInput {
        AgentInput {
            message: msg.text.clone(),
            mode: self.agent_mode(),
            pending_plan: self.pending_plan().map(Path::to_path_buf),
            images: msg.images.clone(),
        }
    }

    pub(super) fn mode_label(&self) -> (Cow<'static, str>, Style) {
        let label: Cow<'static, str> = match &self.mode {
            Mode::Build => "[BUILD]".into(),
            Mode::Plan { .. } => "[PLAN]".into(),
            Mode::BuildPlan => {
                let name = self
                    .ready_plan
                    .as_deref()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .unwrap_or("PLAN");
                format!("[BUILD {name}]").into()
            }
        };
        let style = Style::new()
            .fg(self.mode.color())
            .add_modifier(Modifier::BOLD);
        (label, style)
    }
}
