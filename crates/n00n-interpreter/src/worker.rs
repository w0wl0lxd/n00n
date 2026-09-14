use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::rc::Rc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::InterpreterError;
use crate::runner::{self, AsyncResolver, PendingCall, ToolFn};

const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;

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

struct Bridge<R, W> {
    reader: RefCell<R>,
    writer: RefCell<W>,
    next_request_id: Cell<u64>,
}

impl<R: BufRead, W: Write> Bridge<R, W> {
    fn read_request(&self) -> Result<WorkerRequest, WorkerError> {
        read_request_frame(&mut *self.reader.borrow_mut(), MAX_REQUEST_BYTES)
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

fn read_request_frame(
    reader: &mut impl BufRead,
    max_bytes: usize,
) -> Result<WorkerRequest, WorkerError> {
    let mut line = String::new();
    let read_limit = u64::try_from(max_bytes.saturating_add(2)).map_err(|error| {
        WorkerError::Protocol(format!("invalid interpreter worker frame limit: {error}"))
    })?;
    let mut limited = std::io::Read::take(reader, read_limit);
    let read = limited.read_line(&mut line)?;
    if read == 0 {
        return Err(WorkerError::Protocol(
            "parent closed the protocol stream".into(),
        ));
    }
    let payload = match line.strip_suffix('\n') {
        Some(payload) => payload,
        None => &line,
    };
    let payload = match payload.strip_suffix('\r') {
        Some(payload) => payload,
        None => payload,
    };
    if payload.len() > max_bytes {
        return Err(WorkerError::Protocol(
            "interpreter worker request exceeded the frame limit".into(),
        ));
    }
    Ok(serde_json::from_str(payload)?)
}

/// Streams interpreter stdout to the parent. The first failed send is kept so
/// it can be returned after the run; later lines are skipped instead of
/// repeating the same closed-channel error and reporting success.
struct OutputStream<'a, R: BufRead, W: Write> {
    bridge: &'a Bridge<R, W>,
    failure: RefCell<Option<WorkerError>>,
}

impl<R: BufRead, W: Write> OutputStream<'_, R, W> {
    fn write_chunk(&self, chunk: &str) {
        if self.failure.borrow().is_some() {
            return;
        }
        for line in chunk.lines() {
            if let Err(error) = self.bridge.send(&WorkerEvent::Output {
                line: line.to_owned(),
            }) {
                *self.failure.borrow_mut() = Some(error);
                return;
            }
        }
    }

    fn into_error(self) -> Option<WorkerError> {
        self.failure.into_inner()
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
    let output = OutputStream {
        bridge: bridge.as_ref(),
        failure: RefCell::new(None),
    };
    let result =
        runner::run_streaming(&start.code, &tools, Some(&resolver), limits, &mut |chunk| {
            output.write_chunk(chunk);
        });
    if let Some(error) = output.into_error() {
        return Err(error);
    }
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

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::io::{BufReader, Cursor};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::{Value, json};
    use test_case::test_case;

    use super::{
        Bridge, OutputStream, StartRequest, WorkerError, WorkerEvent, WorkerRequest,
        read_request_frame,
    };
    use crate::runner::PendingCall;

    const TEST_FRAME_LIMIT: usize = 128;

    struct FailingWriter {
        attempts: Arc<AtomicUsize>,
    }

    impl std::io::Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            self.attempts.fetch_add(1, Ordering::Relaxed);
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "parent closed the pipe",
            ))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn output_stream_records_the_first_failed_send() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let bridge = Bridge {
            reader: RefCell::new(BufReader::new(Cursor::new(Vec::new()))),
            writer: RefCell::new(FailingWriter {
                attempts: Arc::clone(&attempts),
            }),
            next_request_id: Cell::new(1),
        };
        let output = OutputStream {
            bridge: &bridge,
            failure: RefCell::new(None),
        };

        output.write_chunk("first\n");
        let after_first = attempts.load(Ordering::Relaxed);
        assert!(after_first > 0, "the first line must reach the writer");

        output.write_chunk("second\n");
        assert_eq!(
            attempts.load(Ordering::Relaxed),
            after_first,
            "a recorded send failure must stop further writes"
        );

        let error = output
            .into_error()
            .expect("a failed output send must not be discarded");
        assert!(
            matches!(error, WorkerError::Io(_) | WorkerError::Json(_)),
            "{error}"
        );
    }

    fn framed_request(request: &WorkerRequest) -> Vec<u8> {
        let mut frame = serde_json::to_vec(request).unwrap();
        frame.push(b'\n');
        frame
    }

    fn test_bridge(request: &WorkerRequest) -> Bridge<BufReader<Cursor<Vec<u8>>>, Vec<u8>> {
        Bridge {
            reader: RefCell::new(BufReader::new(Cursor::new(framed_request(request)))),
            writer: RefCell::new(Vec::new()),
            next_request_id: Cell::new(1),
        }
    }

    #[test_case(
        &json!({"type": "start", "code": "pass", "tool_names": [], "timeout_millis": 1, "max_memory_bytes": 1}),
        "start";
        "start"
    )]
    #[test_case(
        &json!({"type": "call_results", "request_id": 4, "results": []}),
        "call_results";
        "call_results"
    )]
    fn worker_request_round_trips_with_type_tag(expected: &Value, expected_type: &str) {
        let mut frame = serde_json::to_vec(&expected).unwrap();
        frame.push(b'\n');
        let request = read_request_frame(&mut Cursor::new(frame), TEST_FRAME_LIMIT).unwrap();
        let encoded = serde_json::to_value(request).unwrap();
        assert_eq!(encoded["type"], expected_type);
        assert_eq!(&encoded, expected);
    }

    #[test_case(&WorkerEvent::Started, "started" ; "started")]
    #[test_case(&WorkerEvent::Output { line: "hello".into() }, "output" ; "output")]
    #[test_case(&WorkerEvent::Complete { output: None, stdout: "done".into() }, "complete" ; "complete")]
    fn worker_event_round_trips_with_type_tag(event: &WorkerEvent, expected_type: &str) {
        let mut frame = serde_json::to_vec(&event).unwrap();
        frame.push(b'\n');
        assert_eq!(frame.last(), Some(&b'\n'));
        let decoded: WorkerEvent = serde_json::from_slice(&frame).unwrap();
        let encoded = serde_json::to_value(decoded).unwrap();
        assert_eq!(encoded["type"], expected_type);
    }

    #[test_case(TEST_FRAME_LIMIT, false ; "maximum_size")]
    #[test_case(TEST_FRAME_LIMIT + 1, true ; "oversized")]
    fn request_frame_limit_is_enforced(payload_bytes: usize, expected_error: bool) {
        let request = json!({"type": "call_results", "request_id": 1, "results": []}).to_string();
        let padding = payload_bytes.checked_sub(request.len()).unwrap();
        let mut frame = format!("{request}{}", " ".repeat(padding)).into_bytes();
        frame.push(b'\n');
        let result = read_request_frame(&mut Cursor::new(frame), TEST_FRAME_LIMIT);
        assert_eq!(result.is_err(), expected_error);
    }

    #[test]
    fn forward_calls_rejects_mismatched_request_id() {
        let bridge = test_bridge(&WorkerRequest::CallResults {
            request_id: 2,
            results: Vec::new(),
        });
        let error = bridge
            .forward_calls(vec![PendingCall {
                call_id: 7,
                name: "read".into(),
                args: Vec::new(),
                kwargs: Vec::new(),
            }])
            .unwrap_err();
        assert!(error.to_string().contains("did not match request"));
        assert!(
            String::from_utf8(bridge.writer.into_inner())
                .unwrap()
                .contains("tool_calls")
        );
    }

    #[test]
    fn forward_calls_rejects_duplicate_start() {
        let bridge = test_bridge(&WorkerRequest::Start(StartRequest {
            code: "pass".into(),
            tool_names: Vec::new(),
            timeout_millis: 1,
            max_memory_bytes: 1,
        }));
        let error = bridge.forward_calls(Vec::new()).unwrap_err();
        assert!(error.to_string().contains("duplicate start request"));
    }
}
