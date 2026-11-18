#!/bin/bash
# Resident Codex: JSON-RPC over stdio, `codex app-server`. Envelopes follow
# due's tests/fixtures/session-agent.sh.
. "$SPIKE_DIR/lib.sh"

native="fake-codex-$$"
turn=turn-1
emit() { printf '%s\n' "$1"; }
result() { emit '{"id":'"$1"',"result":'"$2"'}'; }
notify() { emit '{"method":"'"$1"'","params":'"$2"'}'; }
id_re='"id":(-?[0-9]+)'
method_re='"method":"([^"]+)"'
active=0
asked=0

agent_message() {
  notify item/completed '{"threadId":"'"$native"'","turnId":"'"$turn"'","item":{"type":"agentMessage","id":"answer-1","text":"'"$1"'","phase":"'"$2"'"}}'
}
completed() { # status text
  active=0; active_off
  [ -z "$2" ] || agent_message "$2" final_answer
  notify turn/completed '{"threadId":"'"$native"'","turn":{"id":"'"$turn"'","items":[],"status":"'"$1"'","error":null,"durationMs":1}}'
}

begin() {
  active=1; active_on
  result "$id" '{"turn":{"id":"'"$turn"'","items":[],"status":"inProgress","error":null}}'
  notify turn/started '{"threadId":"'"$native"'","turn":{"id":"'"$turn"'","items":[],"status":"inProgress","error":null}}'
  start_descendants
  case "$SPIKE_CASE" in
    a) completed completed ok ;;
    b) echo "fake codex: fatal error" >&2; exit 3 ;;
    e) crash ;;
    f) hang_forever ;;
    h) exec 1>&-; hang_forever ;;
    g)
      asked=1
      emit '{"method":"item/commandExecution/requestApproval","id":17,"params":{"threadId":"'"$native"'","turnId":"'"$turn"'","itemId":"exec-1","command":"touch never-created","availableDecisions":["accept","cancel"]}}' ;;
    i) agent_message working commentary ;;
    c|d|j) agent_message working commentary ;;
  esac
}

case "$SPIKE_CASE" in
  c|d|j) start_ticker '{"method":"item/completed","params":{"threadId":"'"$native"'","turnId":"'"$turn"'","item":{"type":"agentMessage","id":"answer-1","text":"working","phase":"commentary"}}}' ;;
esac
while IFS= read -r line; do
  if [[ "$line" =~ $method_re ]]; then
    method="${BASH_REMATCH[1]}"
    if [[ "$line" =~ $id_re ]]; then id="${BASH_REMATCH[1]}"; else continue; fi
    case "$method" in
      initialize) result "$id" '{"userAgent":"fixture/0.149.0","codexHome":"/tmp/fixture","platformFamily":"unix","platformOs":"macos"}' ;;
      thread/start|thread/resume) result "$id" '{"thread":{"id":"'"$native"'","turns":[]}}' ;;
      turn/start) begin ;;
      turn/interrupt)
        if [ "$SPIKE_VARIANT" != stubborn ]; then
          result "$id" '{}'
          [ "$active" = 0 ] || completed interrupted ''
        fi ;;
    esac
  elif [ "$asked" = 1 ]; then
    asked=0
    notify serverRequest/resolved '{"threadId":"'"$native"'","requestId":17}'
    completed completed "permission answered"
  fi
done
[ "$SPIKE_VARIANT" != stubborn ] || hang_forever
exit 0
