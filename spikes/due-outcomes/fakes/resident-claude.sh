#!/bin/bash
# Resident Claude: stream-json duplex on stdin and stdout. Envelopes follow
# due's tests/fixtures/session-agent.sh.
. "$SPIKE_DIR/lib.sh"

sess="fake-claude-$$"
emit() { printf '%s\n' "$1"; }
request_re='"request_id":"([^"]+)"'
asked=0
active=0

assistant() {
  emit '{"type":"assistant","session_id":"'"$sess"'","message":{"role":"assistant","content":[{"type":"text","text":"working"}]}}'
}
result() { # subtype is_error text
  active=0; active_off
  emit '{"type":"result","subtype":"'"$1"'","is_error":'"$2"',"result":"'"$3"'","session_id":"'"$sess"'","total_cost_usd":0.01,"usage":{"input_tokens":1,"output_tokens":1}}'
}

begin() {
  active=1; active_on
  emit '{"type":"system","subtype":"init","session_id":"'"$sess"'"}'
  start_descendants
  case "$SPIKE_CASE" in
    a) assistant; result success false ok ;;
    b) echo "fake claude: fatal error" >&2; exit 3 ;;
    e) crash ;;
    f) hang_forever ;;
    h) exec 1>&-; hang_forever ;;
    g)
      asked=1
      emit '{"type":"control_request","request_id":"permission-1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"touch never-created"}}}' ;;
    i) assistant ;;
    c|d|j) assistant ;;
  esac
}

case "$SPIKE_CASE" in
  c|d|j) start_ticker '{"type":"assistant","session_id":"'"$sess"'","message":{"role":"assistant","content":[{"type":"text","text":"working"}]}}' ;;
esac
while IFS= read -r line; do
  case "$line" in
    *'"type":"control_request"'*)
      if [ "$SPIKE_VARIANT" != stubborn ]; then
        [[ "$line" =~ $request_re ]]
        emit '{"type":"control_response","response":{"subtype":"success","request_id":"'"${BASH_REMATCH[1]}"'","response":{}}}'
        [ "$active" = 0 ] || result error_during_execution true interrupted
      fi ;;
    *'"type":"control_response"'*)
      [ "$asked" = 0 ] || { asked=0; result success false "permission answered"; } ;;
    *'"type":"user"'*) begin ;;
  esac
done
# stdin closed: a normal provider exits, a stubborn one does not.
[ "$SPIKE_VARIANT" != stubborn ] || hang_forever
exit 0
