#!/bin/sh
# A stand-in for the Claude Code CLI, used only by tests. It imitates the
# stream-json output of `claude -p`. The first line of standard input picks the
# behaviour. It never reads real configuration.
#
# It honours its arguments only as far as the tests assume Claude Code does:
# --tools names the tools that exist, --allowedTools lists allow rules, and a
# Bash call is checked against the exact `Bash(command)` rules and then by
# running the hook command from --settings with the documented hook input. This
# proves delega's plumbing against that assumed behaviour, not the real CLI.
#
# Deny rules: the fake reads `permissions.deny` from the --settings JSON and
# denies a matching tool call even when an allow rule or the mode would permit
# it. The matching is deliberately simple and models assumed CLI behaviour, not
# the real rule engine: `Read` rules cover Read, Grep and Glob, `Edit` rules
# cover Edit, Write and NotebookEdit, a bare tool name denies every call, a
# `Bash(cmd)` rule matches the exact command and `Bash(prefix:*)` a prefix, and a
# path rule matches as a glob (`//abs/**`, `//abs/**/name`) or, when relative,
# from the working directory at any depth. Nothing else is evaluated.
#
# FAKE_SPLIT=naive makes it read the rule list the way a CLI that splits on
# commas and all whitespace, ignoring parentheses, would.
#
# Environment (set by the test, never inherited from a real session):
#   FAKE_RECORD_DIR  where to write argv, stdin, env names and pids
#   FAKE_VERSION     the text printed for --version
#   FAKE_SESSION_ID  the session id reported when the run is not resuming one
#   FAKE_RESUMED_SESSION_ID  the session id reported when the run is resuming
#                    one, instead of the id it was asked to resume
#   FAKE_USAGE_DIR   where to count each session's turns (see `turn` below)
#   FAKE_RUN_USAGE   the shape of model_usage_since_last_result on a
#                    successful result. Unset, it has one model entry with the
#                    run's figures; `none` leaves it out, as an older Claude
#                    Code would; `empty` makes it {}; `partial` adds a second
#                    model entry with no costUSD
#
# A successful result carries usage the way Claude Code 2.1.329 does: `usage`
# is the run's own tokens (12 in, 4 cached, 7 out), model_usage_since_last_result
# the run's own figures per model (cost 0.001), and total_cost_usd and
# modelUsage the session's totals through this run, which are the run's figures
# times the turn.
#
# Modes about progress:
#   odd-kinds        a successful run that first writes an event whose type is
#                    "leak=" and an sk-ant- key, and one whose type is 500 "k"
#   odd-denial       a successful run with one denial whose tool name is
#                    "Tool", an sk-ant- key and 150 "x", whose path is
#                    /tmp/denied-path, and whose tool result gives a reason
#                    holding the key
#
# Modes about sessions:
#   unknown-session  what Claude Code 2.1.329 does when --resume names a
#                    session that does not exist: one `result` event with
#                    subtype error_during_execution, is_error true, the error
#                    "No conversation found with session ID: <id>" and the
#                    requested id as session_id, then exit 1
#   unknown-session-404  the unknown-session result, also carrying
#                    api_error_status 404 and api_error_type not_found_error
#                    (not recorded; a shape that would read as a missing model)
#   no-session-id    a successful run whose events carry no session_id
#   two-sessions     a successful run whose first event carries
#                    fake-claude-session-2 and whose later events, ending the
#                    stream, carry the usual id
#   many-sessions    a successful run that reports ten distinct session ids
#                    (fake-claude-many-1 to -10) before the usual id
#   stray-after-result  a successful run followed, after its result, by an
#                    event carrying fake-claude-stray
#   subagent-session  a successful run with a sub-agent message (one with
#                    parent_tool_use_id) carrying fake-claude-subagent
#
# Modes about provider account, capacity and model failures. Each writes a
# root assistant message with is_api_error_message true and then a `result`
# with subtype success, is_error true, its reason in `result` and no `errors`,
# then exits 1, unless it says otherwise:
#   model-not-found  Claude Code 2.1.329 given an unknown --model: assistant
#                    error model_not_found, api_error_status 404,
#                    api_error_type not_found_error
#   invalid-api-key  Claude Code 2.1.329 with an invalid API key: assistant
#                    error authentication_failed, api_error_status 401,
#                    api_error_type authentication_error
#   account-on-hold  assistant error account_on_hold and no status or type
#                    (a value the 2.1.329 executable lists; not recorded)
#   billing-error    assistant error billing_error and api_error_type
#                    billing_error (a value the executable lists; not recorded)
#   usage-limit      Claude Code 2.1.314 at a usage limit, as an earlier runner
#                    recorded it: assistant error rate_limit, api_error_status
#                    429, api_error usage_limit_reached with rate_limit_info,
#                    and a result with api_error usage_limit_reached
#   usage-limit-event  a root rate_limit_event with status rejected and
#                    overage rejected, then a failed result with status 429
#                    and the text "API Error: 429 ..." and no assistant error
#   overage-rate-limit-event  the same rejected rate_limit_event but with
#                    isUsingOverage true, then a failed result with status 529
#                    and the text "API Error: 529 overloaded"
#   rate-limit-event-then-success  the rejected rate_limit_event, then a
#                    successful run (exit 0)
#   overloaded       assistant error overloaded, then a result with
#                    api_error_status 529 and "API Error: 529 overloaded"
#   prompt-too-long  no assistant error message; a result with the text
#                    "Prompt is too long" and no errors
#   result-text-error  no assistant error message; a result with the text
#                    "Something unexpected happened" and no errors or API
#                    fields
#   Standard error within the capture limit (exit 3; TOKEN is
#   abcdefghijklmnop123). The "window" is the last 8 KiB, where the excerpt
#   is cut:
#   stderr-key-at-window  an sk-ant- key whose last 10 bytes start the window,
#                    then newlines and "end"
#   stderr-bearer-at-window  "Authorization: Bearer TOKEN" with the window
#                    starting at the space before TOKEN, then newlines and the
#                    line "complete line survives"
#   stderr-bearer-line-before-window  "Authorization: Bearer", a newline, the
#                    line "TOKEN" starting exactly 8 KiB from the end, newlines
#                    and "last"
#   stderr-nbsp-at-window  "Authorization: Bearer", then 8 KiB from the end a
#                    non-breaking space, a space, TOKEN, spaces and "end"
#   stderr-key-shape-after-cut  "xsk-ant-" and 24 "a", 8158 spaces and "end":
#                    the window drops the "x", leaving a key-shaped word that
#                    the first redaction pass did not see
#   stderr-key-shape-after-300-cut  one line, "start xsk-ant-" and 24 "a", a
#                    space, 264 "b" and " end": the 300-character cut drops the
#                    "x", leaving a key-shaped word right after the "... "
#   stderr-marker-secret-after-300-cut  one line, "start xhiddenvalue42", a
#                    space, 282 "b" and " end": the 300-character cut drops the
#                    "x", leaving "hiddenvalue42" at the start of the excerpt,
#                    the end of a configured secret "...hiddenvalue42"
#   Standard error past the 64 KiB capture limit (exit 3):
#   stderr-bearer-at-8k-of-capture  a line that the capture cuts off, then in
#                    the capture: newlines, "Authorization: Bearer" ending
#                    exactly 8 KiB into the capture, the line TOKEN, a private
#                    key block (see key_block) that redaction shrinks to one
#                    marker so TOKEN would reach the excerpt, and "end"
#   stderr-first-line-kept  the lines "first line" and "second line"
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
#   denied-result-text-error  a failed result with a Bash denial, no errors
#                    and the result text "Connection reset"
#   denied-refused-secret  the denied-refused result, its error naming Bash and
#                    then, after 480 "a", an sk-ant- key across the
#                    500-character clip point
#   secret-at-clip   a failed result whose text is 485 "a", a space, then an
#                    sk-ant- key that runs across the 500-character clip point

if [ "$1" = "--version" ]; then
  # FAKE_VERSION_SLEEP makes the version probe slow, so a test can signal the
  # caller while the probe runs.
  if [ -n "$FAKE_VERSION_SLEEP" ]; then
    [ -n "$FAKE_RECORD_DIR" ] && : > "$FAKE_RECORD_DIR/probe-started"
    sleep "$FAKE_VERSION_SLEEP"
  fi
  printf '%s\n' "${FAKE_VERSION:-2.1.400 (Claude Code)}"
  exit 0
fi

set -f
nl='
'
split_ifs=","
[ "$FAKE_SPLIT" = "naive" ] && split_ifs=", 	$nl"
resume=""
tools=""
allowed=""
settings=""
input_format=""
permission_mode=""
prompt_tool=""
prev=""
for arg in "$@"; do
  case "$prev" in
    --resume) resume="$arg" ;;
    --tools) tools="$arg" ;;
    --allowedTools) allowed="$arg" ;;
    --settings) settings="$arg" ;;
    --input-format) input_format="$arg" ;;
    --permission-mode) permission_mode="$arg" ;;
    --permission-prompt-tool) prompt_tool="$arg" ;;
  esac
  prev="$arg"
done
sid="${FAKE_SESSION_ID:-fake-claude-session-1}"
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

# The strings of the `permissions.deny` array of the settings JSON, one per
# line. The scan follows JSON strings, so a comma, bracket or escaped quote in a
# rule does not end the list.
deny_rules=$(printf '%s' "$settings" | awk '
{
  text = text $0
}
END {
  at = index(text, "\"deny\":[")
  if (at == 0) exit
  rest = substr(text, at + 8)
  out = ""; instr = 0; i = 1; n = length(rest)
  while (i <= n) {
    c = substr(rest, i, 1)
    if (instr) {
      if (c == "\\") { i++; out = out substr(rest, i, 1) }
      else if (c == "\"") { instr = 0; print out; out = "" }
      else out = out c
    } else {
      if (c == "\"") instr = 1
      else if (c == "]") break
    }
    i++
  }
}')

IFS= read -r mode || true
if [ "$input_format" = stream-json ]; then
  [ "$permission_mode" = manual ] && [ "$prompt_tool" = stdio ] || exit 71
  case "$mode" in *'"type":"user"'*) ;; *) exit 72 ;; esac
  case "$mode" in *'"role":"user"'*) ;; *) exit 72 ;; esac
  case ",$tools," in *,Write,*) ;; *) exit 73 ;; esac
  case "$mode" in
    *ask-invalid-json*) printf '%s\n' 'not JSON' ;;
    *ask-stream-too-large*|*ask-no-read*|*ask-oversized*)
      case "$mode" in
        *ask-stream-too-large*) size=4194305 ;;
        *ask-oversized*) size=270000 ;;
        *) size=240000 ;;
      esac
      awk -v size="$size" 'BEGIN {
        printf "{\"type\":\"control_request\",\"request_id\":\"large\",\"request\":{\"subtype\":\"can_use_tool\",\"tool_name\":\"Write\",\"input\":{\"file_path\":\"large.txt\",\"content\":\"%*s\"}}}\n", size, ""
      }'
      case "$mode" in
        *ask-stream-too-large*) exit 0 ;;
        *ask-no-read*) sleep 60; exit 78 ;;
      esac
      IFS= read -r response || exit 74
      case "$response" in
        *'"behavior":"deny"'*'"request_id":"large"'*|*'"request_id":"large"'*'"behavior":"deny"'*) ;;
        *) exit 76 ;;
      esac
      ;;
  esac
  printf '%s\n' '{"type":"control_request","request_id":"permission-1","request":{"subtype":"can_use_tool","tool_name":"Write","tool_use_id":"tool-1","description":"Write the fixture file","display_name":"Write file","permission_suggestions":[],"input":{"file_path":"example.txt","content":"fixture-private-content"}}}'
  IFS= read -r response || exit 74
  case "$response" in *'"type":"control_response"'*'"request_id":"permission-1"'*|*'"request_id":"permission-1"'*'"type":"control_response"'*) ;; *) exit 75 ;; esac
  case "$response" in
    *'"behavior":"allow"'*)
      case "$response" in
        *'"updatedInput":{"content":"fixture-private-content","file_path":"example.txt"}'*|*'"updatedInput":{"file_path":"example.txt","content":"fixture-private-content"}'*) ;;
        *) exit 76 ;;
      esac
      answer=allowed
      denials='[]'
      ;;
    *'"behavior":"deny"'*'"message":'*) answer=denied; denials='[{"tool_name":"Write","tool_use_id":"tool-1","tool_input":{"file_path":"example.txt"}}]' ;;
    *) exit 76 ;;
  esac
  printf '{"type":"result","subtype":"success","is_error":false,"session_id":"%s","result":"%s","permission_denials":%s}\n' "$sid" "$answer" "$denials"
  printf '%s\n' '{"type":"control_request","request_id":"late","request":{"subtype":"can_use_tool","tool_name":"Write","input":{}}}'
  if IFS= read -r extra; then exit 77; fi
  exit 0
fi
rest=$(cat)

if [ -n "$FAKE_RECORD_DIR" ]; then
  printf '%s\0' "$@" > "$FAKE_RECORD_DIR/argv"
  { printf '%s\n' "$mode"; printf '%s' "$rest"; } > "$FAKE_RECORD_DIR/stdin"
  env | sed -n 's/^\([A-Za-z_][A-Za-z0-9_]*\)=.*/\1/p' | sort > "$FAKE_RECORD_DIR/env"
  printf '%s\n' "$$" > "$FAKE_RECORD_DIR/pid"
  pwd > "$FAKE_RECORD_DIR/cwd"
fi

has_tool() {
  case ",$tools," in *",$1,"*) return 0 ;; esac
  return 1
}

# deny_class TOOL: the rule tool that governs TOOL.
deny_class() {
  case "$1" in
    Read|Grep|Glob) printf Read ;;
    Edit|Write|NotebookEdit) printf Edit ;;
    *) printf '%s' "$1" ;;
  esac
}

# denied TOOL TARGET: a deny rule from the settings matches the call. TARGET is
# an absolute path, or the command for Bash.
denied() {
  want=$(deny_class "$1")
  here=$(pwd -P)
  while IFS= read -r rule; do
    [ -n "$rule" ] || continue
    case "$rule" in
      *"("*) tool=${rule%%"("*}; spec=${rule#*"("}; spec=${spec%")"} ;;
      *) tool=$rule; spec="" ;;
    esac
    [ "$tool" = "$want" ] || [ "$tool" = "$1" ] || continue
    [ "$tool" = "$rule" ] && return 0
    if [ "$want" = "Bash" ]; then
      case "$spec" in
        *":*") prefix=${spec%":*"}; case "$2" in "$prefix"*) return 0 ;; esac ;;
        *) [ "$2" = "$spec" ] && return 0 ;;
      esac
      continue
    fi
    case "$spec" in
      "//"*) path="/${spec#//}" ;;
      "~/"*) continue ;;
      "/"*) path="$here/${spec#/}" ;;
      # A relative rule resolves against the working directory and matches at
      # any depth, as a live probe of the real CLI showed.
      *)
        spec=${spec#./}
        spec=${spec#"**/"}
        path="$here/**/$spec"
        ;;
    esac
    zero=$(printf '%s' "$path" | sed 's|\*\*/||g')
    some=$(printf '%s' "$path" | sed 's|\*\*|*|g')
    case "$2" in $zero | $some) return 0 ;; esac
  done <<EOF
$deny_rules
EOF
  return 1
}

# path_allowed TOOL PATH: a bare tool or a scoped allow rule covers PATH.
# A scoped Edit(path) rule covers Edit, Write and NotebookEdit, as Claude Code
# applies it; a bare tool name covers only that tool.
path_allowed() {
  denied "$1" "$2" && return 1
  case "$1" in
    Edit|Write|NotebookEdit) rule_tool=Edit ;;
    *) rule_tool=$1 ;;
  esac
  hit=1
  old=$IFS
  IFS=$split_ifs
  for rule in $allowed; do
    case "$rule" in
      "$1") hit=0 ;;
      "$rule_tool(//**)") hit=0 ;;
      "$rule_tool(//"*"/**)")
        inner=${rule#"$rule_tool(//"}
        inner=${inner%"/**)"}
        case "$2" in "/$inner/"*) hit=0 ;; esac
        ;;
    esac
  done
  IFS=$old
  return $hit
}

# search_allowed TOOL PATH: Glob or Grep exists and is allowed on PATH, either
# because the tool is listed as an allow rule (bare, or scoped to a directory
# covering PATH) or because a scoped Read rule covers PATH. Claude Code applies
# the Read rules to its read-only search tools, so Grep and Glob need no allow
# rule of their own.
search_allowed() {
  has_tool "$1" || return 1
  denied "$1" "$2" && return 1
  path_allowed "$1" "$2" || path_allowed Read "$2"
}

# bash_allowed COMMAND: the Bash tool exists, an exact rule (or bare Bash)
# allows it, and the hook, when settings install one, exits 0.
bash_allowed() {
  has_tool Bash || return 1
  denied Bash "$1" && return 1
  exact=1
  old=$IFS
  IFS=$split_ifs
  for rule in $allowed; do
    [ "$rule" = "Bash" ] && exact=0
    [ "$rule" = "Bash($1)" ] && exact=0
  done
  IFS=$old
  [ $exact -eq 0 ] || return 1
  case "$settings" in
    *'"hooks"'*)
      hook=$(printf '%s' "$settings" | sed -n 's/.*"command":"\([^"]*\)".*/\1/p')
      hook_args=$(printf '%s' "$settings" | sed -n 's/.*"args":\[\(.*\)\],"command".*/\1/p' | tr ',' ' ')
      eval "set -- $hook_args"
      printf '{"tool_name":"Bash","tool_input":{"command":"%s"}}' "$cmd_for_hook" | "$hook" "$@" > /dev/null 2>&1
      status=$?
      [ -n "$FAKE_RECORD_DIR" ] && printf '%s\n' "$status" > "$FAKE_RECORD_DIR/hook"
      return $status
      ;;
  esac
  return 0
}

emit() { printf '%s\n' "$1"; }
init() {
  emit "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"$sid\"}"
  emit "{\"type\":\"assistant\",\"session_id\":\"$sid\",\"message\":{\"id\":\"m1\",\"content\":[{\"type\":\"text\",\"text\":\"working\"}]}}"
}
success() {
  total=$(awk -v t="$turn" 'BEGIN { printf "%.3f", 0.001 * t }')
  case "$FAKE_RUN_USAGE" in
    none) since="" ;;
    empty) since="\"model_usage_since_last_result\":{}," ;;
    partial) since="\"model_usage_since_last_result\":{\"fake-model\":{\"inputTokens\":12,\"outputTokens\":7,\"cacheReadInputTokens\":4,\"cacheCreationInputTokens\":0,\"costUSD\":0.001},\"fake-other\":{\"inputTokens\":0,\"outputTokens\":0}}," ;;
    *) since="\"model_usage_since_last_result\":{\"fake-model\":{\"inputTokens\":12,\"outputTokens\":7,\"cacheReadInputTokens\":4,\"cacheCreationInputTokens\":0,\"costUSD\":0.001}}," ;;
  esac
  models="\"modelUsage\":{\"fake-model\":{\"inputTokens\":$((12 * turn)),\"outputTokens\":$((7 * turn)),\"cacheReadInputTokens\":$((4 * turn)),\"cacheCreationInputTokens\":0,\"costUSD\":$total}},"
  emit "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"session_id\":\"$sid\",\"result\":$1,\"total_cost_usd\":$total,\"usage\":{\"input_tokens\":12,\"cache_read_input_tokens\":4,\"output_tokens\":7},$models$since\"permission_denials\":$2}"
}

# key_block N: a private key block with N bytes of body and no newline, which
# the redactor replaces with one marker, so text far before the 8 KiB excerpt
# reaches it.
key_block() {
  printf '%s' '-----BEGIN RSA PRIVATE KEY-----'
  head -c "$1" /dev/zero | tr '\0' A
  printf '%s' '-----END RSA PRIVATE KEY-----'
}

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

# api_error_message FIELDS TEXT: a root assistant message marked
# is_api_error_message, with the extra JSON members FIELDS and the text TEXT.
api_error_message() {
  emit "{\"type\":\"assistant\",\"parent_tool_use_id\":null,\"session_id\":\"$sid\",\"message\":{\"model\":\"<synthetic>\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"$2\"}]},\"is_api_error_message\":true,$1}"
}
# api_error_result FIELDS TEXT: a failed result with subtype success, the
# extra JSON members FIELDS, TEXT as `result` and no `errors`.
api_error_result() {
  emit "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":true,\"session_id\":\"$sid\",$1,\"result\":\"$2\",\"permission_denials\":[]}"
}

# The acceptance harness's process behaviours (`acc:<behaviour>:<variant>`, see
# acc-lib.sh). No unit test uses them.
acc_success() { success '"fake final answer"' '[]'; }
acc_chatter() { emit "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"x\"}]}}"; }

case "$mode" in
  acc:*)
    . "$(dirname "$0")/acc-lib.sh"
    acc_run "${mode#acc:}"
    ;;
  success)
    init
    success '"fake final answer"' '[]'
    ;;
  echo:*)
    # Answers with its own mode and the session it ran in, so a test can tell
    # which run produced the answer and whether the session was resumed.
    init
    success "\"answered $mode sid=$sid\"" '[]'
    ;;
  trace:*)
    # Records how many `trace:` runs with the same directory are alive when it
    # starts, in `DIR/peak.PID`, so a test can measure concurrency from outside
    # the code under test. The directory is the text up to the first `#`.
    init
    trace_rest=${mode#trace:}
    trace_dir=${trace_rest%%#*}
    : > "$trace_dir/run.$$"
    printf '%s\n' "$(ls "$trace_dir" | grep -c '^run\.')" > "$trace_dir/peak.$$"
    sleep 0.3
    rm -f "$trace_dir/run.$$"
    success "\"answered $mode sid=$sid\"" '[]'
    ;;
  odd-denial)
    init
    key=sk-ant-abcdefghijklmnopqrstuvwx
    tool="Tool $key $(head -c 150 /dev/zero | tr '\0' x)"
    emit "{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"t1\",\"is_error\":true,\"content\":\"Denied with $key here. More detail.\"}]}}"
    success '"done"' "[{\"tool_name\":\"$tool\",\"tool_use_id\":\"t1\",\"tool_input\":{\"file_path\":\"/tmp/denied-path\"}}]"
    ;;
  odd-kinds)
    init
    emit '{"type":"leak=sk-ant-abcdefghijklmnopqrstuvwx"}'
    emit "{\"type\":\"$(head -c 500 /dev/zero | tr '\0' k)\"}"
    success '"fake final answer"' '[]'
    ;;
  secret-message)
    # Agent messages that hold a secret and an oversize text.
    init
    big=$(head -c 5000 /dev/zero | tr '\0' 'x')
    emit "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"token=$MY_SERVICE_API_TOKEN key sk-ant-abcdefghijklmnopqrstuvwx\"},{\"type\":\"text\",\"text\":\"$big\"}]}}"
    success '"done"' '[]'
    ;;
  gate:*)
    # Runs until the file named after `gate:` exists, then answers as `echo:`.
    # Writes `<file>.started` when it begins, so a test can tell it is running.
    init
    gate=${mode#gate:}
    : > "$gate.started"
    while [ ! -e "$gate" ]; do sleep 0.05; done
    success "\"answered $mode sid=$sid\"" '[]'
    ;;
  structured)
    init
    success '"{\"answer\":42}"' '[]'
    ;;
  structured-large)
    # A structured answer well over the compact bound.
    init
    big=$(head -c 20000 /dev/zero | tr '\0' 'x')
    success "\"{\\\"answer\\\":\\\"$big\\\"}\"" '[]'
    ;;
  structured-fenced)
    init
    success '"```json\n{\"answer\":42}\n```"' '[]'
    ;;
  structured-invalid)
    init
    success '"{\"answer\":\"no\"}"' '[]'
    ;;
  structured-text)
    init
    success '"this is prose, not JSON"' '[]'
    ;;
  empty-success)
    init
    success '""' '[]'
    ;;
  provider-error)
    init
    emit "{\"type\":\"result\",\"subtype\":\"error_during_execution\",\"is_error\":true,\"session_id\":\"$sid\",\"errors\":[\"Provider rejected the turn\"],\"permission_denials\":[]}"
    ;;
  exit-nonzero)
    init
    printf 'fake provider crashed\n' >&2
    exit 3
    ;;
  model-not-found)
    init
    text="There's an issue with the selected model (no-such-model). It may not exist or you may not have access to it. Run --model to pick a different model."
    api_error_message "\"error\":\"model_not_found\",\"api_error_status\":404,\"api_error_type\":\"not_found_error\"" "$text"
    api_error_result "\"api_error_status\":404,\"api_error_type\":\"not_found_error\",\"terminal_reason\":\"api_error\"" "$text"
    exit 1
    ;;
  invalid-api-key)
    init
    text="Invalid API key · Fix external API key"
    api_error_message "\"error\":\"authentication_failed\",\"api_error_status\":401,\"api_error_type\":\"authentication_error\",\"api_error_origin\":\"first_party\"" "$text"
    api_error_result "\"api_error_status\":401,\"api_error_type\":\"authentication_error\",\"terminal_reason\":\"api_error\"" "$text"
    exit 1
    ;;
  account-on-hold)
    init
    text="Your account is on hold"
    api_error_message "\"error\":\"account_on_hold\"" "$text"
    api_error_result "\"terminal_reason\":\"api_error\"" "$text"
    exit 1
    ;;
  billing-error)
    init
    text="Your credit balance is too low"
    api_error_message "\"error\":\"billing_error\",\"api_error_type\":\"billing_error\"" "$text"
    api_error_result "\"api_error_type\":\"billing_error\"" "$text"
    exit 1
    ;;
  usage-limit)
    init
    text="You've hit your session limit · resets 1pm (America/Los_Angeles)"
    emit "{\"type\":\"assistant\",\"parent_tool_use_id\":null,\"session_id\":\"$sid\",\"message\":{\"model\":\"<synthetic>\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"$text\"}]},\"error\":\"rate_limit\",\"is_api_error_message\":true,\"api_error_status\":429,\"api_error\":\"usage_limit_reached\",\"api_error_params\":{\"rate_limit_info\":{\"status\":\"rejected\",\"rateLimitType\":\"five_hour\",\"overageStatus\":\"rejected\",\"overageDisabledReason\":\"org_level_disabled\",\"isUsingOverage\":false}}}"
    api_error_result "\"api_error_status\":429,\"api_error\":\"usage_limit_reached\"" "$text"
    exit 1
    ;;
  usage-limit-event)
    init
    emit "{\"type\":\"rate_limit_event\",\"session_id\":\"$sid\",\"rate_limit_info\":{\"status\":\"rejected\",\"rateLimitType\":\"five_hour\",\"overageStatus\":\"rejected\",\"isUsingOverage\":false}}"
    api_error_result "\"api_error_status\":429" "API Error: 429 rate limited"
    exit 1
    ;;
  overage-rate-limit-event)
    init
    emit "{\"type\":\"rate_limit_event\",\"session_id\":\"$sid\",\"rate_limit_info\":{\"status\":\"rejected\",\"rateLimitType\":\"five_hour\",\"overageStatus\":\"allowed\",\"isUsingOverage\":true}}"
    api_error_result "\"api_error_status\":529" "API Error: 529 overloaded"
    exit 1
    ;;
  rate-limit-event-then-success)
    init
    emit "{\"type\":\"rate_limit_event\",\"session_id\":\"$sid\",\"rate_limit_info\":{\"status\":\"rejected\",\"rateLimitType\":\"five_hour\",\"overageStatus\":\"rejected\",\"isUsingOverage\":false}}"
    success '"fake final answer"' '[]'
    ;;
  overloaded)
    init
    api_error_message "\"error\":\"overloaded\",\"api_error_status\":529" "API Error: 529 overloaded"
    api_error_result "\"api_error_status\":529" "API Error: 529 overloaded"
    exit 1
    ;;
  prompt-too-long)
    init
    api_error_result "\"terminal_reason\":\"api_error\"" "Prompt is too long"
    exit 1
    ;;
  result-text-error)
    init
    api_error_result "\"terminal_reason\":\"other\"" "Something unexpected happened"
    exit 1
    ;;
  unknown-session)
    emit "{\"type\":\"result\",\"subtype\":\"error_during_execution\",\"is_error\":true,\"session_id\":\"$resume\",\"errors\":[\"No conversation found with session ID: $resume\"],\"permission_denials\":[]}"
    exit 1
    ;;
  unknown-session-404)
    emit "{\"type\":\"result\",\"subtype\":\"error_during_execution\",\"is_error\":true,\"session_id\":\"$resume\",\"api_error_status\":404,\"api_error_type\":\"not_found_error\",\"errors\":[\"No conversation found with session ID: $resume\"],\"permission_denials\":[]}"
    exit 1
    ;;
  stderr-key-at-window)
    init
    { printf 'sk-ant-abcdefghijklmnopqrstuvwx'; head -c 8178 /dev/zero | tr '\0' '\n'; printf 'end\n'; } >&2
    exit 3
    ;;
  stderr-bearer-at-window)
    init
    { printf 'Authorization: Bearer abcdefghijklmnop123\n'; head -c 8148 /dev/zero | tr '\0' '\n'; printf 'complete line survives\n'; } >&2
    exit 3
    ;;
  stderr-key-shape-after-cut)
    init
    { printf 'xsk-ant-'; head -c 24 /dev/zero | tr '\0' a; head -c 8158 /dev/zero | tr '\0' ' '; printf 'end'; } >&2
    exit 3
    ;;
  stderr-key-shape-after-300-cut)
    init
    { printf 'start xsk-ant-'; head -c 24 /dev/zero | tr '\0' a; printf ' '; head -c 264 /dev/zero | tr '\0' b; printf ' end'; } >&2
    exit 3
    ;;
  stderr-marker-secret-after-300-cut)
    init
    { printf 'start xhiddenvalue42 '; head -c 282 /dev/zero | tr '\0' b; printf ' end'; } >&2
    exit 3
    ;;
  stderr-bearer-at-8k-of-capture)
    init
    {
      printf 'output before the capture\n'
      head -c 8171 /dev/zero | tr '\0' '\n'
      printf 'Authorization: Bearer\nabcdefghijklmnop123\n'
      key_block 57258
      printf '\nend\n'
    } >&2
    exit 3
    ;;
  stderr-bearer-line-before-window)
    init
    { printf 'Authorization: Bearer\nabcdefghijklmnop123\n'; head -c 8167 /dev/zero | tr '\0' '\n'; printf 'last\n'; } >&2
    exit 3
    ;;
  stderr-nbsp-at-window)
    init
    { printf 'Authorization: Bearer\302\240 abcdefghijklmnop123'; head -c 8167 /dev/zero | tr '\0' ' '; printf 'end'; } >&2
    exit 3
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
  stderr-first-line-kept)
    init
    { printf 'first line\nsecond line\n'; } >&2
    exit 3
    ;;
  denied-result-text-error)
    init
    emit "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":true,\"session_id\":\"$sid\",\"result\":\"Connection reset\",\"permission_denials\":[{\"tool_name\":\"Bash\",\"tool_use_id\":\"t1\",\"tool_input\":{\"command\":\"make install\"}}]}"
    ;;
  denied-refused-secret)
    init
    pad=$(head -c 480 /dev/zero | tr '\0' a)
    emit "{\"type\":\"result\",\"subtype\":\"error_during_execution\",\"is_error\":true,\"session_id\":\"$sid\",\"errors\":[\"Bash $pad sk-ant-abcdefghijklmnopqrstuvwx tail\"],\"permission_denials\":[{\"tool_name\":\"Bash\",\"tool_use_id\":\"t1\",\"tool_input\":{\"command\":\"make install\"}}]}"
    ;;
  secret-at-clip)
    init
    pad=$(head -c 485 /dev/zero | tr '\0' a)
    api_error_result "\"terminal_reason\":\"other\"" "$pad sk-ant-abcdefghijklmnopqrstuvwx tail"
    exit 1
    ;;
  two-sessions)
    emit '{"type":"system","subtype":"status","session_id":"fake-claude-session-2"}'
    init
    success '"fake final answer"' '[]'
    ;;
  many-sessions)
    n=1
    while [ $n -le 10 ]; do
      emit "{\"type\":\"system\",\"subtype\":\"status\",\"session_id\":\"fake-claude-many-$n\"}"
      n=$((n+1))
    done
    init
    success '"fake final answer"' '[]'
    ;;
  stray-after-result)
    init
    success '"fake final answer"' '[]'
    emit '{"type":"system","subtype":"status","session_id":"fake-claude-stray"}'
    ;;
  subagent-session)
    init
    emit '{"type":"assistant","parent_tool_use_id":"t1","session_id":"fake-claude-subagent","message":{"id":"m2","content":[{"type":"text","text":"sub-agent text"}]}}'
    success '"fake final answer"' '[]'
    ;;
  no-session-id)
    emit '{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"working"}]}}'
    emit '{"type":"result","subtype":"success","is_error":false,"result":"fake final answer","permission_denials":[]}'
    ;;
  success-then-exit-nonzero)
    init
    success '"fake final answer"' '[]'
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
    emit "{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"t1\",\"is_error\":true,\"content\":\"Permission to use Bash has been denied. More detail.\"}]}}"
    success '"done without the command"' '[{"tool_name":"Bash","tool_use_id":"t1","tool_input":{"command":"rm -rf /tmp/never"}}]'
    ;;
  denied-refused)
    init
    emit "{\"type\":\"result\",\"subtype\":\"error_during_execution\",\"is_error\":true,\"session_id\":\"$sid\",\"errors\":[\"Cannot continue without Bash\"],\"permission_denials\":[{\"tool_name\":\"Bash\",\"tool_use_id\":\"t1\",\"tool_input\":{\"command\":\"make install\"}}]}"
    ;;
  tool-bash:*)
    init
    cmd=${mode#tool-bash:}
    cmd_for_hook=$cmd
    if bash_allowed "$cmd"; then
      success "\"ran: $cmd\"" '[]'
    else
      success '"could not run it"' "[{\"tool_name\":\"Bash\",\"tool_use_id\":\"t1\",\"tool_input\":{\"command\":\"$cmd\"}}]"
    fi
    ;;
  tool-write:*)
    init
    target=${mode#tool-write:}
    if { has_tool Write || has_tool Edit; } && path_allowed Write "$target"; then
      : > "$target"
      success '"wrote it"' '[]'
    else
      success '"could not write it"' "[{\"tool_name\":\"Write\",\"tool_use_id\":\"t1\",\"tool_input\":{\"file_path\":\"$target\"}}]"
    fi
    ;;
  tool-read:*)
    init
    target=${mode#tool-read:}
    if has_tool Read && path_allowed Read "$target"; then
      success '"read it"' '[]'
    else
      success '"could not read it"' "[{\"tool_name\":\"Read\",\"tool_use_id\":\"t1\",\"tool_input\":{\"file_path\":\"$target\"}}]"
    fi
    ;;
  tool-grep:*|tool-glob:*)
    init
    case "$mode" in
      tool-grep:*) search_tool=Grep; target=${mode#tool-grep:}; ;;
      *) search_tool=Glob; target=${mode#tool-glob:} ;;
    esac
    if search_allowed "$search_tool" "$target"; then
      success '"searched it"' '[]'
    else
      success '"could not search it"' "[{\"tool_name\":\"$search_tool\",\"tool_use_id\":\"t1\",\"tool_input\":{\"path\":\"$target\"}}]"
    fi
    ;;
  denial-then-work)
    init
    emit "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"Bash\"}]}}"
    emit "{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"t1\",\"is_error\":true,\"content\":\"Permission denied.\"}]}}"
    emit "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"I will try something else\"}]}}"
    emit "{\"type\":\"result\",\"subtype\":\"error_during_execution\",\"is_error\":true,\"session_id\":\"$sid\",\"errors\":[\"Cannot continue without Bash\"],\"permission_denials\":[{\"tool_name\":\"Bash\",\"tool_use_id\":\"t1\",\"tool_input\":{\"command\":\"make\"}}]}"
    ;;
  denial-api-error)
    init
    emit "{\"type\":\"result\",\"subtype\":\"error_during_execution\",\"is_error\":true,\"session_id\":\"$sid\",\"errors\":[\"API connection lost\"],\"permission_denials\":[{\"tool_name\":\"Bash\",\"tool_use_id\":\"t1\",\"tool_input\":{\"command\":\"make\"}}]}"
    ;;
  error-max-turns-with-denial)
    init
    emit "{\"type\":\"result\",\"subtype\":\"error_max_turns\",\"is_error\":true,\"session_id\":\"$sid\",\"errors\":[\"Turn limit reached\"],\"permission_denials\":[{\"tool_name\":\"Bash\",\"tool_use_id\":\"t1\",\"tool_input\":{\"command\":\"make\"}}]}"
    ;;
  denied-with-connection-error)
    init
    emit "{\"type\":\"system\",\"subtype\":\"api_retry\",\"session_id\":\"$sid\"}"
    emit "{\"type\":\"result\",\"subtype\":\"error_during_execution\",\"is_error\":true,\"session_id\":\"$sid\",\"errors\":[\"Connection reset\"],\"permission_denials\":[{\"tool_name\":\"Bash\",\"tool_use_id\":\"t1\",\"tool_input\":{\"command\":\"make\"}}]}"
    ;;
  failed-then-success)
    init
    emit "{\"type\":\"result\",\"subtype\":\"error_during_execution\",\"is_error\":true,\"session_id\":\"$sid\",\"errors\":[\"First failure\"],\"permission_denials\":[]}"
    success '"a later answer"' '[]'
    ;;
  invalid-utf8)
    init
    printf '{"type":"result","subtype":"success","is_error":false,"result":"bad \377 byte"}\n'
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
      emit "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"x\"}]}}"
      i=$((i+1))
    done
    success '"fake final answer"' '[]'
    ;;
  chatty-hang)
    init
    i=0
    while [ $i -lt 10000 ]; do
      emit "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"x\"}]}}"
      i=$((i+1))
    done
    trap '' TERM
    [ -n "$FAKE_RECORD_DIR" ] && : > "$FAKE_RECORD_DIR/started"
    while :; do sleep 1; done
    ;;
  # A successful result with usage, then no exit until stopped.
  success-then-hang)
    # TERM is ignored before the first event, so a stop that lands once output
    # has begun cannot end the fake before it writes its records.
    trap '' TERM
    init
    success '"fake final answer"' '[]'
    # Written once the result is out, so a test can tell it arrived.
    [ -n "$FAKE_RECORD_DIR" ] && : > "$FAKE_RECORD_DIR/result-written"
    [ -n "$FAKE_RECORD_DIR" ] && : > "$FAKE_RECORD_DIR/started"
    while :; do sleep 1; done
    ;;
  # Ignores the polite stop and runs until the file named after `hang-until:`
  # exists, then exits with no result. Writes `<file>.started` once it ignores
  # the stop, so a test decides when the run can end.
  hang-until:*)
    init
    release=${mode#hang-until:}
    trap '' TERM
    : > "$release.started"
    while [ ! -e "$release" ]; do sleep 0.05; done
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
    success "\"$big\"" '[]'
    ;;
  large-utf8)
    # 24576 characters of a two-, a three- and a four-byte character in turn,
    # so a window of 20000 characters ends between characters of different
    # widths.
    init
    big='é€😀'
    n=0
    while [ $n -lt 13 ]; do big="$big$big"; n=$((n+1)); done
    success "\"$big\"" '[]'
    ;;
  descendant)
    init
    sleep 300 > /dev/null 2>&1 &
    [ -n "$FAKE_RECORD_DIR" ] && printf '%s\n' "$!" > "$FAKE_RECORD_DIR/child.pid"
    success '"fake final answer"' '[]'
    ;;
  *)
    init
    success '"fake final answer"' '[]'
    ;;
esac
