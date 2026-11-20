#!/bin/sh
# A stand-in for the Codex CLI, used only by tests. It imitates `codex exec
# --json` and `codex mcp list --json`. The first line of standard input picks
# the behaviour. It never reads real configuration.
#
# It honours the sandbox flags as far as the tests assume Codex does: it reads
# `-c sandbox_mode=...` and lets a write through only where that sandbox would.
# This proves delega's plumbing against that assumed behaviour, not the real CLI.
#
# Environment (set by the test, never inherited from a real session):
#   FAKE_RECORD_DIR  where to write argv, stdin, env names and pids
#   FAKE_VERSION     the text printed for --version
#   FAKE_MCP         ok (default), bad-json, failure, odd-transport, hang
#   FAKE_SESSION_ID  the thread id reported when the run is not resuming one
#   FAKE_RESUMED_SESSION_ID  the thread id reported when the run is resuming
#                    one, instead of the id it was asked to resume
#
# Modes about sessions:
#   unknown-session  what Codex 0.170.0 does when `resume` names a thread that
#                    does not exist: nothing on standard output, the line
#                    "Error: thread/resume: thread/resume failed: no rollout
#                    found for thread id <id> (code -32600)" on standard error,
#                    then exit 1
#   unknown-session-exit-3        the same standard error, but exit 3
#   unknown-session-after-thread  a thread.started event, then the same
#                    standard error and exit 1
#   unknown-session-long-stderr  the unknown-session standard error line
#                    followed by 9 KiB of other standard error, then exit 1
#   unknown-session-error-event  a JSON error event on standard output, then
#                    the unknown-session standard error and exit 1
#   unknown-session-large-stdout  about 120 KiB of turn.started events on
#                    standard output, then the unknown-session standard error
#                    and exit 1
#   stray-after-completed  a successful run followed, after turn.completed, by
#                    a thread.started event for fake-codex-stray
#   no-session-id    a successful run with no thread.started event
#
# Modes about provider account, capacity and model failures. Each exits 1:
#   model-unsupported  Codex 0.170.0 given an unknown --model with a ChatGPT
#                    login: an error event and a turn.failed with "The
#                    'no-such-model' model is not supported when using Codex
#                    with a ChatGPT account."
#   invalid-api-key  Codex 0.170.0 with an invalid API key: ten error events
#                    "Reconnecting... n/5 (unexpected status 401 Unauthorized:
#                    Incorrect API key provided: ...)" and a turn.failed with
#                    "unexpected status 401 Unauthorized: Incorrect API key
#                    provided: ..."
#   usage-limit      the usage limit text an earlier runner recorded, on an
#                    error event and a turn.failed, both with codex_error_info
#                    usage_limit_exceeded
#   usage-limit-error-only  the usage limit error event with codex_error_info
#                    usage_limit_exceeded and no terminal event
#   usage-limit-info-only  a turn.failed whose message names nothing, with
#                    codex_error_info usage_limit_exceeded
#   context-window   a turn.failed saying the model's context window ran out
#   capacity         a turn.failed with "unexpected status 503 Service
#                    Unavailable: the server is at capacity" (not recorded)

for arg in "$@"; do
  case "$arg" in
    --version)
      if [ -n "$FAKE_VERSION_SLEEP" ]; then
        [ -n "$FAKE_RECORD_DIR" ] && : > "$FAKE_RECORD_DIR/probe-started"
        sleep "$FAKE_VERSION_SLEEP"
      fi
      printf '%s\n' "${FAKE_VERSION:-codex-cli 0.99.0}"
      exit 0
      ;;
  esac
done

is_mcp=""
prev=""
for arg in "$@"; do
  [ "$prev" = "mcp" ] && [ "$arg" = "list" ] && is_mcp=yes
  prev="$arg"
done
if [ -n "$is_mcp" ]; then
  [ -n "$FAKE_RECORD_DIR" ] && printf '%s\0' "$@" > "$FAKE_RECORD_DIR/mcp-argv"
  case "${FAKE_MCP:-ok}" in
    ok)
      printf '%s' '[{"name":"ambient","transport":{"type":"stdio"}},{"name":"server.with.dots","transport":{"type":"streamable_http"}}]'
      ;;
    bad-json) printf 'invalid' ;;
    odd-transport) printf '%s' '[{"name":"x","transport":{"type":"carrier-pigeon"}}]' ;;
    failure)
      printf 'Configuration discovery failed\n' >&2
      exit 1
      ;;
    hang)
      trap '' TERM
      while :; do sleep 1; done
      ;;
  esac
  exit 0
fi

resume=""
sandbox=""
prev=""
for arg in "$@"; do
  [ "$prev" = "resume" ] && resume="$arg"
  if [ "$prev" = "-c" ]; then
    case "$arg" in
      sandbox_mode=*) sandbox=$(printf '%s' "${arg#sandbox_mode=}" | tr -d '"') ;;
      # Accepted, and nothing else: the fake starts no shell profile.
      allow_login_shell=*) ;;
    esac
  fi
  prev="$arg"
done
sid="${FAKE_SESSION_ID:-fake-codex-thread-1}"
[ -n "$resume" ] && sid="${FAKE_RESUMED_SESSION_ID:-$resume}"

IFS= read -r mode || true
rest=$(cat)

if [ -n "$FAKE_RECORD_DIR" ]; then
  printf '%s\0' "$@" > "$FAKE_RECORD_DIR/argv"
  { printf '%s\n' "$mode"; printf '%s' "$rest"; } > "$FAKE_RECORD_DIR/stdin"
  env | sed -n 's/^\([A-Za-z_][A-Za-z0-9_]*\)=.*/\1/p' | sort > "$FAKE_RECORD_DIR/env"
  printf '%s\n' "$$" > "$FAKE_RECORD_DIR/pid"
  pwd > "$FAKE_RECORD_DIR/cwd"
fi

emit() { printf '%s\n' "$1"; }
init() {
  emit "{\"type\":\"thread.started\",\"thread_id\":\"$sid\"}"
  emit '{"type":"turn.started"}'
  emit '{"type":"item.completed","item":{"id":"i0","type":"agent_message","phase":"commentary","text":"working"}}'
}
final() {
  emit "{\"type\":\"item.completed\",\"item\":{\"id\":\"i1\",\"type\":\"agent_message\",\"phase\":\"final_answer\",\"text\":$1}}"
}
done_ok() {
  emit '{"type":"turn.completed","usage":{"input_tokens":100,"cached_input_tokens":10,"output_tokens":7,"reasoning_output_tokens":3}}'
}

# The acceptance harness's process behaviours (`acc:<behaviour>:<variant>`, see
# acc-lib.sh). No unit test uses them.
acc_success() { final '"fake final answer"'; done_ok; }
acc_chatter() { emit '{"type":"item.completed","item":{"id":"i","type":"agent_message","phase":"commentary","text":"x"}}'; }

case "$mode" in
  acc:*)
    . "$(dirname "$0")/acc-lib.sh"
    acc_run "${mode#acc:}"
    ;;
  success)
    init
    final '"fake final answer"'
    done_ok
    ;;
  echo:*)
    init
    final "\"answered $mode sid=$sid\""
    done_ok
    ;;
  trace:*)
    init
    trace_rest=${mode#trace:}
    trace_dir=${trace_rest%%#*}
    : > "$trace_dir/run.$$"
    printf '%s\n' "$(ls "$trace_dir" | grep -c '^run\.')" > "$trace_dir/peak.$$"
    sleep 0.3
    rm -f "$trace_dir/run.$$"
    final "\"answered $mode sid=$sid\""
    done_ok
    ;;
  secret-message)
    init
    big=$(head -c 5000 /dev/zero | tr '\0' 'x')
    emit "{\"type\":\"item.completed\",\"item\":{\"id\":\"s1\",\"type\":\"agent_message\",\"phase\":\"commentary\",\"text\":\"token=$MY_SERVICE_API_TOKEN key sk-ant-abcdefghijklmnopqrstuvwx\"}}"
    emit "{\"type\":\"item.completed\",\"item\":{\"id\":\"s2\",\"type\":\"agent_message\",\"phase\":\"commentary\",\"text\":\"$big\"}}"
    final '"done"'
    done_ok
    ;;
  gate:*)
    init
    gate=${mode#gate:}
    : > "$gate.started"
    while [ ! -e "$gate" ]; do sleep 0.05; done
    final "\"answered $mode sid=$sid\""
    done_ok
    ;;
  structured)
    init
    final '"{\"answer\":42}"'
    done_ok
    ;;
  structured-large)
    init
    big=$(head -c 20000 /dev/zero | tr '\0' 'x')
    final "\"{\\\"answer\\\":\\\"$big\\\"}\""
    done_ok
    ;;
  structured-fenced)
    init
    final '"```json\n{\"answer\":42}\n```"'
    done_ok
    ;;
  structured-invalid)
    init
    final '"{\"answer\":\"no\"}"'
    done_ok
    ;;
  structured-text)
    init
    final '"this is prose, not JSON"'
    done_ok
    ;;
  empty-success)
    init
    done_ok
    ;;
  provider-error)
    init
    emit '{"type":"turn.failed","error":{"message":"Provider rejected the turn"}}'
    ;;
  error-event)
    init
    emit '{"type":"error","message":"Transport unavailable"}'
    ;;
  recoverable-error)
    init
    emit '{"type":"error","message":"Retrying connection"}'
    final '"fake final answer"'
    done_ok
    ;;
  exit-nonzero)
    init
    printf 'fake provider crashed\n' >&2
    exit 3
    ;;
  model-unsupported)
    init
    msg="The 'no-such-model' model is not supported when using Codex with a ChatGPT account."
    emit "{\"type\":\"error\",\"message\":\"$msg\"}"
    emit "{\"type\":\"turn.failed\",\"error\":{\"message\":\"$msg\"}}"
    exit 1
    ;;
  invalid-api-key)
    init
    detail="unexpected status 401 Unauthorized: Incorrect API key provided: sk-fake-****test. You can find your API key at https://platform.openai.com/account/api-keys."
    n=1
    while [ $n -le 10 ]; do
      emit "{\"type\":\"error\",\"message\":\"Reconnecting... $(( (n + 1) / 2 ))/5 ($detail)\"}"
      n=$((n+1))
    done
    emit "{\"type\":\"turn.failed\",\"error\":{\"message\":\"$detail\"}}"
    exit 1
    ;;
  usage-limit)
    init
    msg="You've hit your usage limit. Visit https://chatgpt.com/codex/settings/usage to purchase more credits or try again at Oct 3rd, 2026 11:04 AM."
    emit "{\"type\":\"error\",\"message\":\"$msg\",\"codex_error_info\":\"usage_limit_exceeded\"}"
    emit "{\"type\":\"turn.failed\",\"error\":{\"message\":\"$msg\",\"codex_error_info\":\"usage_limit_exceeded\"}}"
    exit 1
    ;;
  usage-limit-error-only)
    init
    msg="You've hit your usage limit. Visit https://chatgpt.com/codex/settings/usage to purchase more credits or try again at Oct 3rd, 2026 11:04 AM."
    emit "{\"type\":\"error\",\"message\":\"$msg\",\"codex_error_info\":\"usage_limit_exceeded\"}"
    exit 1
    ;;
  usage-limit-info-only)
    init
    emit '{"type":"turn.failed","codex_error_info":"usage_limit_exceeded","error":{"message":"The turn could not continue"}}'
    exit 1
    ;;
  context-window)
    init
    emit '{"type":"turn.failed","error":{"message":"Codex ran out of room in the model'"'"'s context window. Start a new thread or clear earlier history before retrying."}}'
    exit 1
    ;;
  capacity)
    init
    emit '{"type":"turn.failed","error":{"message":"unexpected status 503 Service Unavailable: the server is at capacity"}}'
    exit 1
    ;;
  unknown-session)
    printf 'Error: thread/resume: thread/resume failed: no rollout found for thread id %s (code -32600)\n' "$resume" >&2
    exit 1
    ;;
  unknown-session-exit-3)
    printf 'Error: thread/resume: thread/resume failed: no rollout found for thread id %s (code -32600)\n' "$resume" >&2
    exit 3
    ;;
  unknown-session-after-thread)
    emit "{\"type\":\"thread.started\",\"thread_id\":\"$sid\"}"
    printf 'Error: thread/resume: thread/resume failed: no rollout found for thread id %s (code -32600)\n' "$resume" >&2
    exit 1
    ;;
  unknown-session-long-stderr)
    printf 'Error: thread/resume: thread/resume failed: no rollout found for thread id %s (code -32600)\n' "$resume" >&2
    head -c 9216 /dev/zero | tr '\0' 'x' >&2
    printf '\n' >&2
    exit 1
    ;;
  unknown-session-error-event)
    emit '{"type":"error","message":"thread/resume failed"}'
    printf 'Error: thread/resume: thread/resume failed: no rollout found for thread id %s (code -32600)\n' "$resume" >&2
    exit 1
    ;;
  unknown-session-large-stdout)
    i=0
    while [ $i -lt 5000 ]; do
      emit '{"type":"turn.started"}'
      i=$((i+1))
    done
    printf 'Error: thread/resume: thread/resume failed: no rollout found for thread id %s (code -32600)\n' "$resume" >&2
    exit 1
    ;;
  stray-after-completed)
    init
    final '"fake final answer"'
    done_ok
    emit '{"type":"thread.started","thread_id":"fake-codex-stray"}'
    ;;
  no-session-id)
    emit '{"type":"turn.started"}'
    final '"fake final answer"'
    done_ok
    ;;
  success-then-exit-nonzero)
    init
    final '"fake final answer"'
    done_ok
    exit 5
    ;;
  malformed)
    printf 'not json\n'
    ;;
  no-final)
    init
    ;;
  denied-completed)
    init
    emit '{"type":"item.started","item":{"id":"c1","type":"command_execution","command":"rm -rf /tmp/never","status":"in_progress"}}'
    emit '{"type":"item.completed","item":{"id":"c1","type":"command_execution","command":"rm -rf /tmp/never","status":"declined"}}'
    final '"done without the command"'
    done_ok
    ;;
  denied-refused)
    init
    emit '{"type":"item.completed","item":{"id":"c1","type":"command_execution","command":"make install","status":"declined"}}'
    emit '{"type":"turn.failed","error":{"message":"Cannot continue without the command"}}'
    ;;
  tool-write:*)
    init
    target=${mode#tool-write:}
    here=$(pwd)
    allow=1
    case "$sandbox" in
      danger-full-access) allow=0 ;;
      workspace-write) case "$target" in "$here"/*) allow=0 ;; esac ;;
    esac
    if [ $allow -eq 0 ]; then
      : > "$target"
      emit "{\"type\":\"item.completed\",\"item\":{\"id\":\"c1\",\"type\":\"command_execution\",\"command\":\"touch $target\",\"status\":\"completed\",\"exit_code\":0}}"
      final '"wrote it"'
    else
      emit "{\"type\":\"item.completed\",\"item\":{\"id\":\"c1\",\"type\":\"command_execution\",\"command\":\"touch $target\",\"status\":\"failed\",\"exit_code\":1}}"
      final '"could not write it"'
    fi
    done_ok
    ;;
  tool-command:*)
    init
    cmd=${mode#tool-command:}
    emit "{\"type\":\"item.completed\",\"item\":{\"id\":\"c1\",\"type\":\"command_execution\",\"command\":\"$cmd\",\"status\":\"completed\",\"exit_code\":0}}"
    final "\"ran: $cmd\""
    done_ok
    ;;
  failed-then-completed)
    init
    emit '{"type":"turn.failed","error":{"message":"First failure"}}'
    final '"a later answer"'
    done_ok
    ;;
  denied-then-file-change)
    init
    emit '{"type":"item.completed","item":{"id":"c1","type":"command_execution","command":"make","status":"declined"}}'
    emit '{"type":"item.completed","item":{"id":"f1","type":"file_change","status":"completed"}}'
    emit '{"type":"turn.failed","error":{"message":"Cannot continue"}}'
    ;;
  denied-then-usage-limit)
    init
    emit '{"type":"item.completed","item":{"id":"c1","type":"command_execution","command":"make","status":"declined"}}'
    emit '{"type":"turn.failed","error":{"message":"You have hit your usage limit. Try again later."}}'
    ;;
  denied-then-error)
    init
    emit '{"type":"item.completed","item":{"id":"c1","type":"command_execution","command":"make","status":"declined"}}'
    emit '{"type":"error","message":"Connection reset"}'
    emit '{"type":"turn.failed","error":{"message":"Stream disconnected"}}'
    ;;
  invalid-utf8)
    init
    printf '{"type":"item.completed","item":{"type":"agent_message","phase":"final_answer","text":"bad \377 byte"}}\n'
    done_ok
    ;;
  stderr-secret)
    init
    printf 'boom token=%s and sk-ant-abcdefghijklmnopqrstuvwx and Bearer abcdefghijklmnop123\n' "$MY_SERVICE_API_TOKEN" >&2
    exit 3
    ;;
  chatty)
    init
    i=0
    while [ $i -lt 20000 ]; do
      emit "{\"type\":\"item.completed\",\"item\":{\"id\":\"i\",\"type\":\"agent_message\",\"phase\":\"commentary\",\"text\":\"x\"}}"
      i=$((i+1))
    done
    final '"fake final answer"'
    done_ok
    ;;
  chatty-hang)
    init
    i=0
    while [ $i -lt 10000 ]; do
      emit "{\"type\":\"item.completed\",\"item\":{\"id\":\"i\",\"type\":\"agent_message\",\"phase\":\"commentary\",\"text\":\"x\"}}"
      i=$((i+1))
    done
    trap '' TERM
    [ -n "$FAKE_RECORD_DIR" ] && : > "$FAKE_RECORD_DIR/started"
    while :; do sleep 1; done
    ;;
  hang)
    init
    trap '' TERM
    [ -n "$FAKE_RECORD_DIR" ] && : > "$FAKE_RECORD_DIR/started"
    while :; do sleep 1; done
    ;;
  large)
    init
    big=$(head -c 3000000 /dev/zero | tr '\0' 'x')
    final "\"$big\""
    done_ok
    ;;
  descendant)
    init
    sleep 300 > /dev/null 2>&1 &
    [ -n "$FAKE_RECORD_DIR" ] && printf '%s\n' "$!" > "$FAKE_RECORD_DIR/child.pid"
    final '"fake final answer"'
    done_ok
    ;;
  *)
    init
    final '"fake final answer"'
    done_ok
    ;;
esac
