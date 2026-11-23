//! The eight tools. Each is one call into the supervisor and returns what the
//! supervisor returns, as structured content and as the same JSON in a text
//! block. The adapter keeps no state of its own beyond the supervisor handle.

use std::future::Future;
use std::sync::Arc;

use delega_core::contract::applied::AppliedConfig;
use delega_core::contract::partial::PartialRequest;
use delega_core::supervisor::{
    CloseResponse, Detail, Error, FollowUpResponse, InterruptResponse, ListResponse, ReadResponse,
    StartResponse, Supervisor, Target, WaitResponse, WaitTargets,
};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;
use tower_mcp::extract::{RawArgs, State};
use tower_mcp::{CallToolResult, Content, Tool, ToolBuilder};

use crate::args::{CloseArgs, FollowUpArgs, InterruptArgs, ListArgs, ReadArgs, WaitArgs};
use crate::stale::{StaleServer, Staleness};
use crate::start;

/// The tool names. The server lists them sorted by name.
pub const TOOL_NAMES: [&str; 8] = [
    "start",
    "follow_up",
    "wait",
    "read",
    "list",
    "interrupt",
    "close",
    "respond",
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

const WAIT: &str = "Wait for a named run or worker to finish a run or need a permission decision, \
    or for the time to pass (default 30 s unless the server's configuration file sets another, \
    capped by the server; list reports both). A timeout is not an error: \
    timed_out is true and the states are current. A wait that the server's shutdown released \
    answers released_by_shutdown true, and a run it names can still be running. The server lets \
    a limited number of waits block at once (list reports max_waits_in_flight and \
    waits_in_flight); a wait beyond it returns at once with limit_reached \
    \"max_waits_in_flight\", timeout_secs 0 and the current states: let an open wait return, \
    or call again later with the cursor; a wait with timeout_secs 0 only checks and is never \
    turned away. Pass the cursor back as after to be told only \
    of later completions and permission changes. Use respond for a pending_interactions entry.";

const READ: &str = "Read a run, or a worker's current run: the result if it has finished, \
    otherwise the state, pending interactions, partial text and recent activity. Returns at once. Compact by \
    default; detail full adds the activity history, diagnostics and every field of the result. \
    A long answer is cut: page it with text_offset, using the run id and not the worker id, \
    because a worker's latest run can change between pages. The window says where it sits and \
    whether more follows.";

const LIST: &str = "List the workers on this connection with their state, queue length and \
    session id, and the limits in force and in use. It starts nothing and waits on no run; its \
    check of the server's executable waits for the filesystem for about 2 s at most. Check it \
    before starting many workers. stale_server is present when that check finds the executable \
    this server started from replaced or removed: tell the user to reconnect the server or \
    start a new session. A check that did not finish reports nothing.";

const INTERRUPT: &str = "Stop a run, or a worker's current run. Returns when the run has \
    stopped and cleanup was attempted, after 30 s at most; settled false means the stop has \
    not finished and the run should be read later. The result is interrupted. A finished run \
    is returned unchanged.";

const CLOSE: &str = "Stop a worker, discard its queued runs and free its slot. Returns when its \
    running run has stopped, after 30 s at most. Read the results you need first: they stay \
    readable only for a bounded time after close. Closing a closed worker answers the same way.";

/// The answer to `list`: the supervisor's, and `stale_server` while the
/// executable this server was started from is no longer the one on disk.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[schemars(title = "ListResponse")]
struct ListAnswer {
    #[serde(flatten)]
    list: ListResponse,
    /// Present only when the executable this server was started from has been
    /// replaced or removed since, for example by `delega plugin install`: the
    /// host is still running the old build.
    #[serde(skip_serializing_if = "Option::is_none")]
    stale_server: Option<StaleServer>,
}

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

/// The schemas of the eight tools, built from the same definitions the server
/// lists, without a supervisor or a runtime.
pub fn tool_schemas() -> Vec<ToolSchemas> {
    tools(
        None,
        PartialRequest::default(),
        None,
        Staleness::new(None, ""),
    )
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
    /// The configuration file those defaults came from, for `applied.config`.
    config: Option<AppliedConfig>,
    /// Whether the executable the server runs from is still the one on disk.
    staleness: Staleness,
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
pub(crate) fn tools(
    supervisor: Option<Supervisor>,
    defaults: PartialRequest,
    config: Option<AppliedConfig>,
    staleness: Staleness,
) -> Vec<Tool> {
    let adapter = Arc::new(Adapter {
        supervisor,
        defaults,
        config,
        staleness,
    });
    vec![
        start_tool(&adapter),
        follow_up_tool(&adapter),
        wait_tool(&adapter),
        read_tool(&adapter),
        list_tool(&adapter),
        interrupt_tool(&adapter),
        close_tool(&adapter),
        respond_tool(&adapter),
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
                let config = adapter.config.clone();
                let built = tokio::task::spawn_blocking(move || {
                    start::build_request(defaults, config, args)
                })
                .await;
                Ok(match built {
                    Ok(Ok((request, origin))) => {
                        adapter
                            .with(|s| async move {
                                s.start_requiring_cwd_with_origin(request, origin).await
                            })
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
        .output_schema_for::<ListAnswer>()
        .read_only_safe()
        .handler(move |_args: ListArgs| {
            let adapter = adapter.clone();
            async move {
                // Asked on every call: the executable can be replaced at any time.
                let stale_server = adapter.staleness.check().await;
                Ok(adapter
                    .with(|s| async move {
                        s.list().await.map(|list| ListAnswer { list, stale_server })
                    })
                    .await)
            }
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

#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RespondArgs {
    interaction: String,
    decision: delega_core::contract::result::PermissionDecision,
    #[schemars(length(max = 256))]
    note: Option<String>,
}

fn respond_tool(adapter: &Arc<Adapter>) -> Tool {
    let adapter = adapter.clone();
    ToolBuilder::new("respond")
        .description("Answer one pending permission interaction with allow or deny.")
        .output_schema_for::<delega_core::supervisor::RespondResponse>()
        .handler(move |args: RespondArgs| {
            let adapter = adapter.clone();
            async move {
                if args.note.is_some_and(|note| note.len() > 256) {
                    return Ok(CallToolResult::error(
                        "note must be at most 256 UTF-8 bytes",
                    ));
                }
                Ok(adapter
                    .with(|s| async move { s.respond(args.interaction, args.decision).await })
                    .await)
            }
        })
        .build()
}
