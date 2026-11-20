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
#
# Modes about sessions:
#   unknown-session  what Claude Code 2.1.329 does when --resume names a
#                    session that does not exist: one `result` event with
#                    subtype error_during_execution, is_error true, the error
#                    "No conversation found with session ID: <id>" and the
#                    requested id as session_id, then exit 1
#   no-session-id    a successful run whose events carry no session_id

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
prev=""
for arg in "$@"; do
  case "$prev" in
    --resume) resume="$arg" ;;
    --tools) tools="$arg" ;;
    --allowedTools) allowed="$arg" ;;
    --settings) settings="$arg" ;;
  esac
  prev="$arg"
done
sid="${FAKE_SESSION_ID:-fake-claude-session-1}"
[ -n "$resume" ] && sid="${FAKE_RESUMED_SESSION_ID:-$resume}"

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

# path_allowed TOOL PATH: an allow rule TOOL(//dir/**) covers PATH.
path_allowed() {
  denied "$1" "$2" && return 1
  hit=1
  old=$IFS
  IFS=$split_ifs
  for rule in $allowed; do
    case "$rule" in
      "$1") hit=0 ;;
      "$1(//**)") hit=0 ;;
      "$1(//"*"/**)")
        inner=${rule#"$1(//"}
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
  emit "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"session_id\":\"$sid\",\"result\":$1,\"total_cost_usd\":0.001,\"usage\":{\"input_tokens\":12,\"cache_read_input_tokens\":4,\"output_tokens\":7},\"permission_denials\":$2}"
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
  unknown-session)
    emit "{\"type\":\"result\",\"subtype\":\"error_during_execution\",\"is_error\":true,\"session_id\":\"$resume\",\"errors\":[\"No conversation found with session ID: $resume\"],\"permission_denials\":[]}"
    exit 1
    ;;
  no-session-id)
    emit '{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"working"}]}}'
    emit '{"type":"result","subtype":"success","is_error":false,"result":"fake final answer","permission_denials":[]}'
    ;;
  success-then-exit-nonzero)
    init
    success '"fake final answer"' '[]'
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
  hang)
    init
    trap '' TERM
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
