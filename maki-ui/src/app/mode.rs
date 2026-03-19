use std::borrow::Cow;
use std::path::{Path, PathBuf};

use crate::theme;
use maki_agent::{AgentInput, AgentMode};
use maki_storage::DataDir;
use maki_storage::plans;
use ratatui::style::{Color, Modifier, Style};

use super::App;
use super::queue::QueuedMessage;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Build,
    Plan,
    BuildPlan,
}

impl Mode {
    pub(crate) fn color(&self) -> Color {
        match self {
            Self::Build => theme::current().mode_build,
            Self::Plan => theme::current().mode_plan,
            Self::BuildPlan => theme::current().mode_build_plan,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlanState {
    path: Option<PathBuf>,
    written: bool,
}

impl PlanState {
    pub(crate) fn new() -> Self {
        Self {
            path: None,
            written: false,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_path(path: PathBuf, written: bool) -> Self {
        Self {
            path: Some(path),
            written,
        }
    }

    pub(crate) fn ensure_path(&mut self, storage: &DataDir) {
        self.path.get_or_insert_with(|| {
            plans::new_plan_path(storage).unwrap_or_else(|_| PathBuf::from("plans/plan.md"))
        });
    }

    pub(crate) fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub(crate) fn mark_written(&mut self) {
        self.written = true;
    }

    #[cfg(test)]
    pub(crate) fn is_written(&self) -> bool {
        self.written
    }

    pub(crate) fn pending_plan(&self) -> Option<&Path> {
        if self.written { self.path() } else { None }
    }
}

impl App {
    pub(super) fn reset_plan(&mut self) {
        self.mode = Mode::Build;
        self.plan = PlanState::new();
    }

    pub(super) fn toggle_mode(&mut self) -> Vec<super::Action> {
        self.mode = match self.mode {
            Mode::Build => {
                self.plan.ensure_path(&self.storage);
                Mode::Plan
            }
            Mode::Plan => {
                if self.plan.written {
                    Mode::BuildPlan
                } else {
                    Mode::Build
                }
            }
            Mode::BuildPlan => Mode::Build,
        };
        vec![]
    }

    pub(super) fn agent_mode(&self) -> AgentMode {
        match self.mode {
            Mode::Plan => match self.plan.path() {
                Some(p) => AgentMode::Plan(p.to_path_buf()),
                None => AgentMode::Build,
            },
            Mode::Build | Mode::BuildPlan => AgentMode::Build,
        }
    }

    pub(super) fn pending_plan(&self) -> Option<&Path> {
        match self.mode {
            Mode::BuildPlan => self.plan.pending_plan(),
            _ => None,
        }
    }

    pub(crate) fn build_agent_input(&self, msg: &QueuedMessage) -> AgentInput {
        AgentInput {
            message: msg.text.clone(),
            mode: self.agent_mode(),
            pending_plan: self.pending_plan().map(Path::to_path_buf),
            images: msg.images.clone(),
            ..Default::default()
        }
    }

    pub(super) fn mode_label(&self) -> (Cow<'static, str>, Style) {
        let label: Cow<'static, str> = match self.mode {
            Mode::Build => "[BUILD]".into(),
            Mode::Plan => "[PLAN]".into(),
            Mode::BuildPlan => {
                let name = self
                    .plan
                    .path()
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
