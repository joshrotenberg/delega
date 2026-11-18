# Shared helpers for the fake providers. Sourced, never run directly.
#
# Environment (set by the runner for one case):
#   SPIKE_DIR      case directory; every process the fake starts has a script
#                  path under it, which is the marker the harness searches for
#   SPIKE_CASE     outcome case letter
#   SPIKE_VARIANT  plain | child | setsid | ignore | stubborn | pipe | orphan | orphan-late

: "${SPIKE_DIR:?}" "${SPIKE_CASE:?}" "${SPIKE_VARIANT:?}"

# Append "role pid" to the case's pid ledger.
record() { printf '%s %s\n' "$1" "$2" >> "$SPIKE_DIR/pids"; }

record provider $$

# stubborn: the provider itself ignores SIGINT and SIGTERM. Otherwise a signal
# ends it, like a real CLI with default handlers.
if [ "$SPIKE_VARIANT" = stubborn ]; then
  trap '' INT TERM
else
  trap 'exit 130' INT TERM
fi

# Start a descendant with default SIGINT handling. A bash background job in a
# script inherits SIGINT=ignored, which would make every child look stubborn.
spawn_default() {
  perl -e '$SIG{INT}="DEFAULT"; $SIG{QUIT}="DEFAULT"; $SIG{TERM}="DEFAULT"; exec @ARGV' "$@" &
}

# Cleanup variants. Called once when the provider starts working.
#   child   one child in the provider's process group, stdio detached
#   orphan  a child that starts a setsid grandchild and exits at once, so the
#           grandchild is reparented before any process listing can see the link
#   orphan-late  as orphan, but the child lives 1.3 s first
#   pipe    as child, but it keeps the provider's stdin, stdout and stderr
#   setsid  a child in the group that starts a grandchild in its own session
#   ignore  one child in the group that ignores SIGTERM and SIGINT
start_descendants() {
  case "$SPIKE_VARIANT" in
    child)  spawn_default bash "$SPIKE_DIR/child.sh" plain </dev/null >/dev/null 2>&1 ;;
    ignore) spawn_default bash "$SPIKE_DIR/child.sh" ignore </dev/null >/dev/null 2>&1 ;;
    setsid) spawn_default bash "$SPIKE_DIR/child.sh" mid </dev/null >/dev/null 2>&1 ;;
    pipe)   spawn_default bash "$SPIKE_DIR/child.sh" plain ;;
    orphan) spawn_default bash "$SPIKE_DIR/child.sh" orphan </dev/null >/dev/null 2>&1 ;;
    orphan-late) spawn_default bash "$SPIKE_DIR/child.sh" orphan-late </dev/null >/dev/null 2>&1 ;;
  esac
}

# Sleep forever without reading input. A trapped signal ends it promptly.
hang_forever() { while :; do sleep 1; done; }

# SIGKILL ourselves after a short delay: a crash, not an exit.
crash() { sleep 0.5; kill -9 $$; }

# Mark the provider as working. A ticker (see below) emits output while the
# marker exists. bash 3.2 cannot tell a read timeout from EOF, so periodic
# output cannot come from the input loop.
active_on() { : > "$SPIKE_DIR/active"; }
active_off() { rm -f "$SPIKE_DIR/active"; }

# Print the line "$1" every 0.5 s while the active marker exists, until the
# provider process is gone. The ticker is a separate script so that it is
# recorded under its own role.
start_ticker() {
  printf '%s\n' "$1" > "$SPIKE_DIR/tick.txt"
  spawn_default bash "$SPIKE_DIR/ticker.sh" $$
}
