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
#   FAKE_USAGE_DIR   where to count each thread's turns (see `turn` below)
#
# turn.completed carries usage the way Codex 0.170.0 does: the thread's totals
# through this turn, here 100 in, 10 cached, 7 out and 3 reasoning per turn,
# times the turn.
#
# Modes about progress:
#   odd-kinds        a successful run that first writes an event whose type is
#                    "leak=" and an sk-ant- key, and one whose type is 500 "k"
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
#   secret-at-clip   a turn.failed whose message is 485 "a", a space, then an
#                    sk-ant- key that runs across the 500-character clip point
#   secret-at-clip-error-only  the same message on an error event, with no
#                    terminal event
#   usage-limit-info-no-message  an error event with codex_error_info
#                    usage_limit_exceeded and no message, and no terminal event
#   denied-refused-secret  a declined command, then a turn.failed whose
#                    message is "Bash", 480 "a" and an sk-ant- key across the
#                    500-character clip point
#   capacity         a turn.failed with "unexpected status 503 Service
#                    Unavailable: the server is at capacity" (not recorded)
#   content-policy   codex-cli 0.171.0 on 2026-11-24 refusing an assignment
#                    under its content policy: an error event and a turn.failed
#                    with "This content was flagged for possible cybersecurity
#                    risk. ...", the text as recorded in issue #219, where part
#                    of it was elided as "..."
#   content-policy-quoted  the same events with a message that only quotes the
#                    content-policy phrase after other text
#
# Modes about the standard error excerpt:
#   Standard error past the 64 KiB capture limit whose capture starts inside
#   a secret (exit 3). "|" marks where the capture starts; after the fragment
#   come newlines and 20 long sk-ant- keys (see secret_fill) up to exactly
#   64 KiB, which redact to 20 markers:
#   stderr-cut-key   "sk-ant-api03-abcdefghijkl|mnopqrstuvwxyz0123456789"
#   stderr-cut-bearer  "Authorization: Bearer", a newline, "|abcdefghijklmnop123"
#   stderr-cut-private-key  a private key BEGIN line, "MIIEbody|cutbodyline0123456789"
#                    and the END line
#   stderr-cut-char  "caf" and the two bytes of an e with acute accent, with
#                    the capture starting at the second byte
#   stderr-cut-value  the value of MY_SERVICE_API_TOKEN and a newline, then 20
#                    keys of 2400 bytes: the capture starts inside the value
#                    when it is longer than 17536 bytes
#   stderr-cut-then-plain  the stderr-cut-key fragment, then 4679 lines
#                    "ordinary line" and "end", so text is left after the guard
#   stderr-full-not-cut  "first words", newlines and the 20 keys, exactly
#                    64 KiB in all, so nothing is cut

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

# The turn of the session this run is: 1 for a run that starts a session, 2
# for a resumed one. With FAKE_USAGE_DIR set, a counter per session is kept
# there instead, so each resumed run is the turn after the last one recorded.
turn=1
[ -n "$resume" ] && turn=2
if [ -n "$FAKE_USAGE_DIR" ]; then
  counter="$FAKE_USAGE_DIR/turns-$(printf '%s' "$sid" | tr -c 'A-Za-z0-9._-' '_')"
  if [ -n "$resume" ]; then
    last=$(cat "$counter" 2>/dev/null || echo 1)
    turn=$((last + 1))
  fi
  printf '%s\n' "$turn" > "$counter"
fi

IFS= read -r mode || true
rest=$(cat)

if [ -n "$FAKE_RECORD_DIR" ]; then
  printf '%s\0' "$@" > "$FAKE_RECORD_DIR/argv"
  { printf '%s\n' "$mode"; printf '%s' "$rest"; } > "$FAKE_RECORD_DIR/stdin"
  env | sed -n 's/^\([A-Za-z_][A-Za-z0-9_]*\)=.*/\1/p' | sort > "$FAKE_RECORD_DIR/env"
  printf '%s\n' "$$" > "$FAKE_RECORD_DIR/pid"
  pwd > "$FAKE_RECORD_DIR/cwd"
fi

# secret_fill COUNT BYTES: COUNT lines of BYTES bytes each (newline included),
# every one an sk-ant- key, which the redactor replaces with one marker. A few
# long ones fill a capture and redact to less than one excerpt, so the
# capture's start reaches the excerpt unless it is guarded.
secret_fill() {
  fill_i=0
  while [ "$fill_i" -lt "$1" ]; do
    printf 'sk-ant-'
    head -c $(($2 - 8)) /dev/zero | tr '\0' a
    printf '\n'
    fill_i=$((fill_i + 1))
  done
}

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
  emit "{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":$((100 * turn)),\"cached_input_tokens\":$((10 * turn)),\"output_tokens\":$((7 * turn)),\"reasoning_output_tokens\":$((3 * turn))}}"
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
  odd-kinds)
    init
    emit '{"type":"leak=sk-ant-abcdefghijklmnopqrstuvwx"}'
    emit "{\"type\":\"$(head -c 500 /dev/zero | tr '\0' k)\"}"
    final '"fake final answer"'
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
  content-policy)
    init
    msg="This content was flagged for possible cybersecurity risk. If this seems wrong, try rephrasing your request. If you're doing authorized security work that requires more cyber permissive safeguards, apply for ... access via https://platform.openai.com/settings/organization/status-and-access before retrying."
    emit "{\"type\":\"error\",\"message\":\"$msg\"}"
    emit "{\"type\":\"turn.failed\",\"error\":{\"message\":\"$msg\"}}"
    exit 1
    ;;
  content-policy-quoted)
    init
    msg="Stream disconnected while reading the reply to: This content was flagged for possible review"
    emit "{\"type\":\"error\",\"message\":\"$msg\"}"
    emit "{\"type\":\"turn.failed\",\"error\":{\"message\":\"$msg\"}}"
    exit 1
    ;;
  context-window)
    init
    emit '{"type":"turn.failed","error":{"message":"Codex ran out of room in the model'"'"'s context window. Start a new thread or clear earlier history before retrying."}}'
    exit 1
    ;;
  secret-at-clip)
    init
    pad=$(head -c 485 /dev/zero | tr '\0' a)
    emit "{\"type\":\"turn.failed\",\"error\":{\"message\":\"$pad sk-ant-abcdefghijklmnopqrstuvwx tail\"}}"
    exit 1
    ;;
  secret-at-clip-error-only)
    init
    pad=$(head -c 485 /dev/zero | tr '\0' a)
    emit "{\"type\":\"error\",\"message\":\"$pad sk-ant-abcdefghijklmnopqrstuvwx tail\"}"
    exit 1
    ;;
  usage-limit-info-no-message)
    init
    emit '{"type":"error","codex_error_info":"usage_limit_exceeded"}'
    exit 1
    ;;
  denied-refused-secret)
    init
    pad=$(head -c 480 /dev/zero | tr '\0' a)
    emit '{"type":"item.completed","item":{"id":"c1","type":"command_execution","command":"make install","status":"declined"}}'
    emit "{\"type\":\"turn.failed\",\"error\":{\"message\":\"Bash $pad sk-ant-abcdefghijklmnopqrstuvwx tail\"}}"
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
    echo 'fake provider failed after success' >&2
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
  stderr-cut-key)
    init
    {
      printf 'sk-ant-api03-abcdefghijkl'
      printf 'mnopqrstuvwxyz0123456789\n'
      head -c 1511 /dev/zero | tr '\0' '\n'
      secret_fill 20 3200
    } >&2
    exit 3
    ;;
  stderr-cut-bearer)
    init
    {
      printf 'Authorization: Bearer\n'
      printf 'abcdefghijklmnop123\n'
      head -c 1516 /dev/zero | tr '\0' '\n'
      secret_fill 20 3200
    } >&2
    exit 3
    ;;
  stderr-cut-private-key)
    init
    {
      printf '%s\nMIIEbody' '-----BEGIN RSA PRIVATE KEY-----'
      printf 'cutbodyline0123456789\n%s\n' '-----END RSA PRIVATE KEY-----'
      head -c 1484 /dev/zero | tr '\0' '\n'
      secret_fill 20 3200
    } >&2
    exit 3
    ;;
  stderr-cut-char)
    init
    {
      printf 'caf\303'
      printf '\251\n'
      head -c 1534 /dev/zero | tr '\0' '\n'
      secret_fill 20 3200
    } >&2
    exit 3
    ;;
  stderr-cut-value)
    init
    {
      printf '%s\n' "$MY_SERVICE_API_TOKEN"
      secret_fill 20 2400
    } >&2
    exit 3
    ;;
  stderr-cut-then-plain)
    init
    {
      printf 'sk-ant-api03-abcdefghijkl'
      printf 'mnopqrstuvwxyz0123456789\n\n'
      yes 'ordinary line' | head -n 4679
      printf 'end\n'
    } >&2
    exit 3
    ;;
  stderr-full-not-cut)
    init
    {
      printf 'first words\n'
      head -c 1524 /dev/zero | tr '\0' '\n'
      secret_fill 20 3200
    } >&2
    exit 3
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
  # A final answer and turn.completed with usage, then no exit until stopped.
  success-then-hang)
    # TERM is ignored before the first event, so a stop that lands once output
    # has begun cannot end the fake before it writes its records.
    trap '' TERM
    init
    final '"fake final answer"'
    done_ok
    # Written once the result is out, so a test can tell it arrived.
    [ -n "$FAKE_RECORD_DIR" ] && : > "$FAKE_RECORD_DIR/result-written"
    [ -n "$FAKE_RECORD_DIR" ] && : > "$FAKE_RECORD_DIR/started"
    while :; do sleep 1; done
    ;;
  hang)
    # TERM is ignored before the first event, so a stop that lands once output
    # has begun cannot end the fake before it writes `started`.
    trap '' TERM
    init
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
