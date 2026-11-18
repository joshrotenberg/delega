#!/bin/bash
# One-shot Claude: `claude -p --output-format json|stream-json`, prompt on stdin.
# Version probe: answered before anything else, no ledger entry.
for a in "$@"; do
  if [ "$a" = --version ]; then echo "2.1.325 (Claude Code)"; exit 0; fi
done
. "$SPIKE_DIR/lib.sh"
prompt=$(cat)
stream=0
for a in "$@"; do [ "$a" != stream-json ] || stream=1; done

assistant='{"type":"assistant","session_id":"fake-session","message":{"role":"assistant","content":[{"type":"text","text":"working"}]}}'
result_ok='{"type":"result","subtype":"success","is_error":false,"result":"ok","session_id":"fake-session","total_cost_usd":0.01,"duration_ms":5,"num_turns":1,"usage":{"input_tokens":1,"output_tokens":1}}'
result_denied='{"type":"result","subtype":"success","is_error":false,"result":"could not run the command","session_id":"fake-session","total_cost_usd":0.01,"duration_ms":5,"num_turns":1,"usage":{"input_tokens":1,"output_tokens":1},"permission_denials":[{"tool_name":"Bash","tool_use_id":"tool-1","tool_input":{"command":"touch never-created"}}]}'

# Print a finished transcript in whichever format was requested.
finish() {
  if [ "$stream" = 1 ]; then
    printf '%s\n%s\n' "$assistant" "$1"
  else
    printf '[%s,%s]\n' "$assistant" "$1"
  fi
}

start_descendants
case "$SPIKE_CASE" in
  a) finish "$result_ok" ;;
  b) echo "fake claude: fatal error" >&2; exit 3 ;;
  c|d|j)
    # Keeps producing output; never finishes.
    while :; do
      [ "$stream" = 1 ] && printf '%s\n' "$assistant"
      sleep 0.2
    done ;;
  e) crash ;;
  f) hang_forever ;;
  g) finish "$result_denied" ;;
  h) exec 1>&-; hang_forever ;;
  i)
    # Output ends without a terminal result, then the process exits 0.
    if [ "$stream" = 1 ]; then printf '%s\n' "$assistant"; else printf '[%s]\n' "$assistant"; fi ;;
esac
exit 0
