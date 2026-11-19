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
# FAKE_SPLIT=naive makes it read the rule list the way a CLI that splits on
# commas and all whitespace, ignoring parentheses, would.
#
# Environment (set by the test, never inherited from a real session):
#   FAKE_RECORD_DIR  where to write argv, stdin, env names and pids
#   FAKE_VERSION     the text printed for --version

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
sid="${resume:-fake-claude-session-1}"

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

# path_allowed TOOL PATH: an allow rule TOOL(//dir/**) covers PATH.
path_allowed() {
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

# bash_allowed COMMAND: the Bash tool exists, an exact rule (or bare Bash)
# allows it, and the hook, when settings install one, exits 0.
bash_allowed() {
  has_tool Bash || return 1
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

case "$mode" in
  success)
    init
    success '"fake final answer"' '[]'
    ;;
  structured)
    init
    success '"{\"answer\":42}"' '[]'
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
