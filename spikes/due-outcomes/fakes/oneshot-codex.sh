#!/bin/bash
# One-shot Codex: `codex exec --json --output-last-message FILE -`, prompt on stdin.
. "$SPIKE_DIR/lib.sh"
prompt=$(cat)
out=
prev=
for a in "$@"; do
  if [ "$prev" = --output-last-message ] || [ "$prev" = -o ]; then out=$a; fi
  prev=$a
done

started() {
  printf '%s\n' '{"type":"thread.started","thread_id":"fake-thread"}' '{"type":"turn.started"}'
}
message() {
  printf '%s\n' '{"type":"item.completed","item":{"id":"item-1","type":"agent_message","text":"ok"}}'
}
completed() {
  printf '%s\n' '{"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1}}'
}

start_descendants
case "$SPIKE_CASE" in
  a) started; message; [ -z "$out" ] || printf 'ok\n' > "$out"; completed ;;
  b) echo "fake codex: fatal error" >&2; exit 3 ;;
  c|d|j)
    started
    while :; do message; sleep 0.2; done ;;
  e) started; crash ;;
  f) hang_forever ;;
  g)
    # exec mode answers an approval request with a denial and goes on.
    started
    printf '%s\n' '{"type":"item.completed","item":{"id":"item-2","type":"command_execution","command":"touch never-created","status":"declined","exit_code":null}}'
    message; [ -z "$out" ] || printf 'ok\n' > "$out"; completed ;;
  h) started; exec 1>&-; hang_forever ;;
  i) started; message; [ -z "$out" ] || printf 'ok\n' > "$out" ;;
esac
exit 0
