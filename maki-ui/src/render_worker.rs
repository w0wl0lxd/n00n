use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;

use maki_agent::{ToolInput, ToolOutput};
use ratatui::text::Line;

struct RenderJob {
    id: u64,
    tool_input: Option<ToolInput>,
    tool_output: Option<ToolOutput>,
}

pub struct RenderResult {
    pub id: u64,
    pub lines: Vec<Line<'static>>,
}

static NEXT_JOB_ID: AtomicU64 = AtomicU64::new(0);

pub struct RenderWorker {
    tx: mpsc::Sender<RenderJob>,
    rx: mpsc::Receiver<RenderResult>,
}

impl RenderWorker {
    pub fn new() -> Self {
        let (req_tx, req_rx) = mpsc::channel::<RenderJob>();
        let (res_tx, res_rx) = mpsc::channel::<RenderResult>();

        thread::Builder::new()
            .name("render".into())
            .spawn(move || {
                use crate::components::code_view;
                while let Ok(job) = req_rx.recv() {
                    let lines = code_view::render_tool_content(
                        job.tool_input.as_ref(),
                        job.tool_output.as_ref(),
                        true,
                    );
                    if res_tx.send(RenderResult { id: job.id, lines }).is_err() {
                        break;
                    }
                }
            })
            .expect("spawn highlight thread");

        Self {
            tx: req_tx,
            rx: res_rx,
        }
    }

    pub fn send(&self, tool_input: Option<ToolInput>, tool_output: Option<ToolOutput>) -> u64 {
        let id = NEXT_JOB_ID.fetch_add(1, Ordering::Relaxed);
        let _ = self.tx.send(RenderJob {
            id,
            tool_input,
            tool_output,
        });
        id
    }

    pub fn try_recv(&self) -> Option<RenderResult> {
        self.rx.try_recv().ok()
    }
}
