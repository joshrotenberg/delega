#!/bin/sh
# A stand-in for the Claude Code CLI, used only by tests. It imitates the
# stream-json output of `claude -p`. The first line of standard input picks the
# behaviour. It never reads real configuration.
#
# Environment (set by the test, never inherited from a real session):
#   FAKE_RECORD_DIR  where to write argv, stdin, env names and pids
#   FAKE_VERSION     the text printed for --version

if [ "$1" = "--version" ]; then
  printf '%s\n' "${FAKE_VERSION:-2.1.400 (Claude Code)}"
  exit 0
fi

resume=""
prev=""
for arg in "$@"; do
  [ "$prev" = "--resume" ] && resume="$arg"
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
    success '"done without the command"' '[{"tool_name":"Bash","tool_use_id":"t1","tool_input":{"command":"rm -rf /tmp/never"}}]'
    ;;
  denied-refused)
    init
    emit "{\"type\":\"result\",\"subtype\":\"error_during_execution\",\"is_error\":true,\"session_id\":\"$sid\",\"errors\":[\"Cannot continue without Bash\"],\"permission_denials\":[{\"tool_name\":\"Bash\",\"tool_use_id\":\"t1\",\"tool_input\":{\"command\":\"make install\"}}]}"
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
