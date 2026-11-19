//! The seven tools. Each is one call into the supervisor and returns what the
//! supervisor returns, as structured content and as the same JSON in a text
//! block. The adapter keeps no state of its own beyond the supervisor handle.

use std::future::Future;
use std::sync::Arc;

use delega_core::contract::partial::PartialRequest;
use delega_core::supervisor::{
    CloseResponse, Detail, Error, FollowUpResponse, InterruptResponse, ListResponse, ReadResponse,
    StartResponse, Supervisor, Target, WaitResponse, WaitTargets,
};
use serde::Serialize;
use serde_json::Value;
use tower_mcp::extract::{RawArgs, State};
use tower_mcp::{CallToolResult, Content, Tool, ToolBuilder};

use crate::args::{CloseArgs, FollowUpArgs, InterruptArgs, ListArgs, ReadArgs, WaitArgs};
use crate::start;

/// The tool names. The server lists them sorted by name.
pub const TOOL_NAMES: [&str; 7] = [
    "start",
    "follow_up",
    "wait",
    "read",
    "list",
    "interrupt",
    "close",
];

const START: &str = "Start a worker on a new assignment and return at once with the worker id, \
    the run id and the state (queued or running). Give the request fields (provider, prompt and an \
    absolute cwd are required), or profile plus overrides; a profile may supply cwd. A start \
    with no cwd, or a relative one, is refused as cwd_required. Next, call wait with the run id and a short \
    timeout, passing the cursor back, then read the finished run. A request that cannot go \
    ahead is not an error: it returns a run whose result is refused, and read gives the reason.";

const FOLLOW_UP: &str = "Give an existing worker another assignment that continues its provider \
    session. Returns at once with the new run id; it queues behind a running run. Next, wait and \
    read as for start. Refused as a run, not an error, when the worker is closed, its queue is \
    full, it has no session to continue or the prompt is invalid.";

const WAIT: &str = "Wait until any named run finishes, or the current run of any named worker \
    does, or the time passes (default 30 s unless the server's configuration file sets another, \
    capped by the server; list reports both). A timeout is not an error: \
    timed_out is true and the states are current. Pass the cursor back as after to be told only \
    of what finishes later, instead of waiting long. Next, read a run that finished.";

const READ: &str = "Read a run, or a worker's current run: the result if it has finished, \
    otherwise the state, the partial text and recent activity. Returns at once. Compact by \
    default; detail full adds the activity history, diagnostics and every field of the result. \
    A long answer is cut: page it with text_offset, using the run id and not the worker id, \
    because a worker's latest run can change between pages. The window says where it sits and \
    whether more follows.";

const LIST: &str = "List the workers on this connection with their state, queue length and \
    session id, and the limits in force and in use. Returns at once. Check it before starting \
    many workers.";

const INTERRUPT: &str = "Stop a run, or a worker's current run. Returns when the run has \
    stopped and cleanup was attempted, after 30 s at most; settled false means the stop has \
    not finished and the run should be read later. The result is interrupted. A finished run \
    is returned unchanged.";

const CLOSE: &str = "Stop a worker, discard its queued runs and free its slot. Returns when its \
    running run has stopped, after 30 s at most. Read the results you need first: they stay \
    readable only for a bounded time after close. Closing a closed worker answers the same way.";

/// A tool's name and the schemas of its arguments and result, as the server
/// lists them.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSchemas {
    /// The tool name.
    pub name: String,
    /// The JSON Schema of the arguments.
    pub input: Value,
    /// The JSON Schema of the structured result.
    pub output: Value,
}

/// The schemas of the seven tools, built from the same definitions the server
/// lists, without a supervisor or a runtime.
pub fn tool_schemas() -> Vec<ToolSchemas> {
    tools(None, PartialRequest::default())
        .iter()
        .map(|tool| {
            let definition = tool.definition();
            ToolSchemas {
                name: definition.name,
                input: definition.input_schema,
                output: definition.output_schema.unwrap_or(Value::Null),
            }
        })
        .collect()
}

/// The text a tool returns, as structured content and as the same JSON in a
/// text block.
fn success<T: Serialize>(value: &T) -> CallToolResult {
    match delega_core::canonical::to_sorted_value(value) {
        Ok(json) => {
            let mut result = CallToolResult::json(json.clone());
            result.content = vec![Content::text(json.to_string())];
            result
        }
        Err(err) => CallToolResult::error(format!("the response could not be encoded: {err}")),
    }
}

/// A supervisor answer, or its error as a tool error with the supervisor's own
/// message.
fn answer<T: Serialize>(result: Result<T, Error>) -> CallToolResult {
    match result {
        Ok(value) => success(&value),
        Err(error) => CallToolResult::error(error.to_string()),
    }
}

/// Holds the supervisor, if there is one: [`tool_schemas`] builds the tools
/// without.
#[derive(Clone)]
struct Adapter {
    supervisor: Option<Supervisor>,
    /// The request defaults every `start` begins from.
    defaults: PartialRequest,
}

impl Adapter {
    async fn with<T, F, Fut>(&self, call: F) -> CallToolResult
    where
        T: Serialize,
        F: FnOnce(Supervisor) -> Fut,
        Fut: Future<Output = Result<T, Error>>,
    {
        match self.supervisor.clone() {
            Some(supervisor) => answer(call(supervisor).await),
            None => CallToolResult::error("the server has no supervisor"),
        }
    }
}

/// The tools over `supervisor`, or the tools alone, for their schemas.
pub(crate) fn tools(supervisor: Option<Supervisor>, defaults: PartialRequest) -> Vec<Tool> {
    let adapter = Arc::new(Adapter {
        supervisor,
        defaults,
    });
    vec![
        start_tool(&adapter),
        follow_up_tool(&adapter),
        wait_tool(&adapter),
        read_tool(&adapter),
        list_tool(&adapter),
        interrupt_tool(&adapter),
        close_tool(&adapter),
    ]
}

fn start_tool(adapter: &Arc<Adapter>) -> Tool {
    ToolBuilder::new("start")
        .description(START)
        .input_schema(start::input_schema())
        .output_schema_for::<StartResponse>()
        .extractor_handler(
            adapter.clone(),
            |State(adapter): State<Arc<Adapter>>, RawArgs(args): RawArgs| async move {
                // A profile is read from disk, so the work is off the runtime's
                // threads: a stuck filesystem must not hold up other calls.
                let defaults = adapter.defaults.clone();
                let built =
                    tokio::task::spawn_blocking(move || start::build_request(defaults, args)).await;
                Ok(match built {
                    Ok(Ok(request)) => {
                        adapter
                            .with(|s| async move { s.start_requiring_cwd(request).await })
                            .await
                    }
                    Ok(Err(message)) => CallToolResult::error(message),
                    Err(_) => CallToolResult::error("reading the request failed unexpectedly"),
                })
            },
        )
        .build()
}

fn follow_up_tool(adapter: &Arc<Adapter>) -> Tool {
    let adapter = adapter.clone();
    ToolBuilder::new("follow_up")
        .description(FOLLOW_UP)
        .output_schema_for::<FollowUpResponse>()
        .handler(move |args: FollowUpArgs| {
            let adapter = adapter.clone();
            async move {
                Ok(adapter
                    .with(|s| async move {
                        s.follow_up(&args.worker, args.prompt, args.timeout_secs)
                            .await
                    })
                    .await)
            }
        })
        .build()
}

fn wait_tool(adapter: &Arc<Adapter>) -> Tool {
    let adapter = adapter.clone();
    ToolBuilder::new("wait")
        .description(WAIT)
        .output_schema_for::<WaitResponse>()
        .read_only()
        .handler(move |args: WaitArgs| {
            let adapter = adapter.clone();
            async move {
                let targets = WaitTargets {
                    runs: args.runs,
                    workers: args.workers,
                };
                Ok(adapter
                    .with(|s| async move { s.wait(targets, args.timeout_secs, args.after).await })
                    .await)
            }
        })
        .build()
}

fn read_tool(adapter: &Arc<Adapter>) -> Tool {
    let adapter = adapter.clone();
    ToolBuilder::new("read")
        .description(READ)
        .output_schema_for::<ReadResponse>()
        .read_only_safe()
        .handler(move |args: ReadArgs| {
            let adapter = adapter.clone();
            async move {
                Ok(match Target::from_args(args.run, args.worker) {
                    Ok(target) => {
                        let detail = args.detail.unwrap_or(Detail::Compact);
                        adapter
                            .with(|s| async move {
                                s.read(target, detail, args.after, args.text_offset).await
                            })
                            .await
                    }
                    Err(error) => answer::<ReadResponse>(Err(error)),
                })
            }
        })
        .build()
}

fn list_tool(adapter: &Arc<Adapter>) -> Tool {
    let adapter = adapter.clone();
    ToolBuilder::new("list")
        .description(LIST)
        .output_schema_for::<ListResponse>()
        .read_only_safe()
        .handler(move |_args: ListArgs| {
            let adapter = adapter.clone();
            async move { Ok(adapter.with(|s| async move { s.list().await }).await) }
        })
        .build()
}

fn interrupt_tool(adapter: &Arc<Adapter>) -> Tool {
    let adapter = adapter.clone();
    ToolBuilder::new("interrupt")
        .description(INTERRUPT)
        .output_schema_for::<InterruptResponse>()
        .handler(move |args: InterruptArgs| {
            let adapter = adapter.clone();
            async move {
                Ok(match Target::from_args(args.run, args.worker) {
                    Ok(target) => {
                        adapter
                            .with(|s| async move { s.interrupt(target).await })
                            .await
                    }
                    Err(error) => answer::<InterruptResponse>(Err(error)),
                })
            }
        })
        .build()
}

fn close_tool(adapter: &Arc<Adapter>) -> Tool {
    let adapter = adapter.clone();
    ToolBuilder::new("close")
        .description(CLOSE)
        .output_schema_for::<CloseResponse>()
        .destructive()
        .handler(move |args: CloseArgs| {
            let adapter = adapter.clone();
            async move {
                Ok(adapter
                    .with(|s| async move { s.close(&args.worker).await })
                    .await)
            }
        })
        .build()
}
