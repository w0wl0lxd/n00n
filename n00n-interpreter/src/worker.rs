use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::rc::Rc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::InterpreterError;
use crate::runner::{self, AsyncResolver, PendingCall, ToolFn};

#[derive(Debug, Serialize, Deserialize)]
pub struct StartRequest {
    pub code: String,
    pub tool_names: Vec<String>,
    pub timeout_millis: u64,
    pub max_memory_bytes: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WireCall {
    pub call_id: u32,
    pub name: String,
    pub args: Vec<Value>,
    pub kwargs: Vec<(String, Value)>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WireCallResult {
    pub call_id: u32,
    pub value: Result<Value, String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkerRequest {
    Start(StartRequest),
    CallResults {
        request_id: u64,
        results: Vec<WireCallResult>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkerEvent {
    Started,
    Output {
        line: String,
    },
    ToolCalls {
        request_id: u64,
        calls: Vec<WireCall>,
    },
    Complete {
        output: Option<Value>,
        stdout: String,
    },
    Failed {
        error: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    #[error("interpreter worker I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("interpreter worker protocol failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("interpreter worker protocol violation: {0}")]
    Protocol(String),
}

struct Bridge {
    reader: RefCell<BufReader<std::io::Stdin>>,
    writer: RefCell<BufWriter<std::io::Stdout>>,
    next_request_id: Cell<u64>,
}

impl Bridge {
    fn read_request(&self) -> Result<WorkerRequest, WorkerError> {
        let mut line = String::new();
        let read = self.reader.borrow_mut().read_line(&mut line)?;
        if read == 0 {
            return Err(WorkerError::Protocol(
                "parent closed the protocol stream".into(),
            ));
        }
        Ok(serde_json::from_str(&line)?)
    }

    fn send(&self, event: &WorkerEvent) -> Result<(), WorkerError> {
        let mut writer = self.writer.borrow_mut();
        serde_json::to_writer(&mut *writer, event)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
        Ok(())
    }

    fn forward_calls(
        &self,
        calls: Vec<PendingCall>,
    ) -> Result<Vec<(u32, Result<Value, String>)>, InterpreterError> {
        let request_id = self.next_request_id.get();
        self.next_request_id.set(request_id.saturating_add(1));
        let calls = calls
            .into_iter()
            .map(|call| WireCall {
                call_id: call.call_id,
                name: call.name,
                args: call.args,
                kwargs: call.kwargs,
            })
            .collect();
        self.send(&WorkerEvent::ToolCalls { request_id, calls })
            .map_err(|error| InterpreterError::Runtime(error.to_string()))?;
        match self
            .read_request()
            .map_err(|error| InterpreterError::Runtime(error.to_string()))?
        {
            WorkerRequest::CallResults {
                request_id: response_id,
                results,
            } if response_id == request_id => Ok(results
                .into_iter()
                .map(|result| (result.call_id, result.value))
                .collect()),
            WorkerRequest::CallResults {
                request_id: response_id,
                ..
            } => Err(InterpreterError::Runtime(format!(
                "interpreter worker response {response_id} did not match request {request_id}"
            ))),
            WorkerRequest::Start(_) => Err(InterpreterError::Runtime(
                "interpreter worker received a duplicate start request".into(),
            )),
        }
    }
}

/// Runs the framed interpreter worker protocol over standard input and output.
///
/// # Errors
///
/// Returns an error when protocol input is invalid or standard I/O fails.
pub fn run_stdio() -> Result<(), WorkerError> {
    let bridge = Rc::new(Bridge {
        reader: RefCell::new(BufReader::new(std::io::stdin())),
        writer: RefCell::new(BufWriter::new(std::io::stdout())),
        next_request_id: Cell::new(1),
    });
    let WorkerRequest::Start(start) = bridge.read_request()? else {
        return Err(WorkerError::Protocol(
            "first interpreter worker request must be start".into(),
        ));
    };

    let tools = start
        .tool_names
        .into_iter()
        .map(|name| {
            let bridge = Rc::clone(&bridge);
            let tool: ToolFn = Box::new(move |fn_name, args, kwargs| {
                let call = PendingCall {
                    call_id: 0,
                    name: fn_name.to_owned(),
                    args,
                    kwargs,
                };
                bridge
                    .forward_calls(vec![call])
                    .map_err(|error| error.to_string())?
                    .pop()
                    .map_or_else(
                        || Err("interpreter worker received an empty tool response".into()),
                        |(_, result)| result,
                    )
            });
            (name, tool)
        })
        .collect::<HashMap<_, _>>();
    let resolver: AsyncResolver = {
        let bridge = Rc::clone(&bridge);
        Box::new(move |calls| bridge.forward_calls(calls))
    };
    let limits = runner::limits(
        Duration::from_millis(start.timeout_millis),
        start.max_memory_bytes,
    );

    bridge.send(&WorkerEvent::Started)?;
    let result =
        runner::run_streaming(&start.code, &tools, Some(&resolver), limits, &mut |chunk| {
            for line in chunk.lines() {
                let _ = bridge.send(&WorkerEvent::Output {
                    line: line.to_owned(),
                });
            }
        });
    match result {
        Ok(result) => bridge.send(&WorkerEvent::Complete {
            output: result.output,
            stdout: result.stdout,
        }),
        Err(error) => bridge.send(&WorkerEvent::Failed {
            error: error.to_string(),
        }),
    }
}
