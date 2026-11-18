#!/bin/bash
# Periodic provider output for the cases where the provider "keeps producing
# output". Exits when its parent (the fake provider) is gone.
printf '%s %s\n' ticker $$ >> "$SPIKE_DIR/pids"
while kill -0 "$1" 2>/dev/null; do
  [ ! -f "$SPIKE_DIR/active" ] || cat "$SPIKE_DIR/tick.txt"
  sleep 0.5
done
