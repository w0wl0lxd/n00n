use std::mem;

use crate::theme;
use maki_agent::AgentMode;
use maki_storage::plans;
use ratatui::style::{Color, Modifier, Style};

use super::App;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Mode {
    Build,
    Plan { path: String, written: bool },
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

    pub(super) fn plan_path(&self) -> Option<&str> {
        match self {
            Self::Plan { path, .. } => Some(path),
            _ => None,
        }
    }

    pub(super) fn mark_plan_written(&mut self, written_path: &str) {
        if let Self::Plan { path, written } = self
            && (written_path == path.as_str() || std::path::Path::new(path).ends_with(written_path))
        {
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
                    .unwrap_or_else(|_| "plans/plan.md".into()),
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

    pub(super) fn pending_plan(&self) -> Option<&str> {
        match &self.mode {
            Mode::BuildPlan => self.ready_plan.as_deref(),
            _ => None,
        }
    }

    pub(super) fn mode_label(&self) -> (&'static str, Style) {
        let label = match &self.mode {
            Mode::Build => "[BUILD]",
            Mode::Plan { .. } => "[PLAN]",
            Mode::BuildPlan => "[BUILD PLAN]",
        };
        let style = Style::new()
            .fg(self.mode.color())
            .add_modifier(Modifier::BOLD);
        (label, style)
    }
}
