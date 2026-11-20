# Process behaviours for the acceptance harness (acceptance/). Sourced by
# fake-claude.sh and fake-codex.sh in their `acc:<behaviour>:<variant>` modes,
# never run directly and never used by the unit tests.
#
# The caller (the harness) copies the fakes and this file into one directory
# whose name is unique to a case. Every process this file starts runs a script
# from that directory, so the directory name is in its command line and the
# harness finds it there, whatever delega reported. Each process also appends
# `role pid` to `pids` in the directory.
#
# The fake that sources this file defines `init`, `acc_success` (the stream a
# successful run ends with) and `acc_chatter` (one line of ordinary output).
#
# Behaviours (what the provider does itself):
#   success   finish normally
#   exit3     print to standard error and exit 3
#   busy      one line of output every 0.2 s, never finishes
#   crash     SIGKILL itself after 0.5 s (1.5 s with the bare variant, see
#             below)
#   hang      no output, never finishes
#   closeout  close standard output, then never finish
#   nofinal   output ends without a final answer, exit 0
#   flood     output at full speed, never finishes
#   slow      finish normally after 1.5 s
#
# Every behaviour but exit3 creates `ready` once its opening output is written.
#
# Variants (what it leaves running beside itself):
#   plain        nothing
#   child        one child in the provider's group, stdio detached
#   setsid       a child in the group that starts a grandchild in a session of
#                its own
#   ignore       a child in the group that ignores TERM and INT
#   stubborn     nothing, but the provider ignores TERM and INT itself
#   pipe         a child that keeps the provider's stdin, stdout and stderr
#   orphan       a child that starts a setsid grandchild and exits at once
#   orphan-late  as orphan, but the child lives 1.3 s first
#   relay        a fork relay (see acc-child.sh); every generation records
#                itself as `relay-heartbeat` before it does anything else
#   bare         a `sleep 300` in a session of its own whose command line does
#                not carry the case directory; only the ledger (and its
#                command name) identifies it, so a harness that looks for the
#                directory name in the process table does not find it

ACC_DIR=$(dirname "$0")

acc_record() { printf '%s %s\n' "$1" "$2" >> "$ACC_DIR/pids"; }

# The stream's opening lines, then a `ready` file: after it the provider has
# written to its standard output, so a caller that ends the reader (SIGKILL of
# delega) cannot make that write fail with SIGPIPE.
acc_init() { init; : > "$ACC_DIR/ready"; }

# Run a command in the background with default handling of INT, QUIT and TERM.
# A background job of a non-interactive shell inherits SIGINT ignored, which
# would make every child look stubborn.
acc_spawn() {
  perl -e '$SIG{INT}="DEFAULT"; $SIG{QUIT}="DEFAULT"; $SIG{TERM}="DEFAULT"; exec @ARGV' "$@" &
}

acc_start_variant() {
  case "$1" in
    child) acc_spawn sh "$ACC_DIR/acc-child.sh" plain </dev/null >/dev/null 2>&1 ;;
    ignore) acc_spawn sh "$ACC_DIR/acc-child.sh" ignore </dev/null >/dev/null 2>&1 ;;
    setsid) acc_spawn sh "$ACC_DIR/acc-child.sh" mid </dev/null >/dev/null 2>&1 ;;
    pipe) acc_spawn sh "$ACC_DIR/acc-child.sh" plain ;;
    orphan) acc_spawn sh "$ACC_DIR/acc-child.sh" orphan </dev/null >/dev/null 2>&1 ;;
    orphan-late) acc_spawn sh "$ACC_DIR/acc-child.sh" orphan-late </dev/null >/dev/null 2>&1 ;;
    relay) acc_spawn sh "$ACC_DIR/acc-child.sh" relay </dev/null >/dev/null 2>&1 ;;
    bare)
      # The path of the marker file travels in the environment, not in the
      # command line, so the process carries no case directory name before or
      # after it becomes `sleep 300`. It writes the file once it has left the
      # session, which is the moment the variant is about.
      ACC_DETACHED="$ACC_DIR/detached" perl -MPOSIX -e 'POSIX::setsid(); $SIG{INT}="DEFAULT"; open(F, ">", $ENV{ACC_DETACHED}); close(F); exec @ARGV' sleep 300 </dev/null >/dev/null 2>&1 &
      acc_record bare $!
      ;;
  esac
}

# Wait until the descendants of a variant exist and have reached the state the
# variant is about, so a provider that finishes at once does so with them
# running. The run then measures delega's cleanup, not whether the descendants
# had started yet. It waits on a recorded event (a ledger entry or a marker
# file), never a fixed sleep, with a deadline of 60 s that only a broken
# machine reaches.
acc_wait_ready() {
  acc_n=0
  while [ $acc_n -lt 1200 ]; do
    case "$1" in
      child | pipe) grep -q '^child ' "$ACC_DIR/pids" 2>/dev/null && return ;;
      ignore) grep -q '^child-ignore ' "$ACC_DIR/pids" 2>/dev/null && return ;;
      setsid | orphan-late) grep -q '^leaf-setsid ' "$ACC_DIR/pids" 2>/dev/null && return ;;
      orphan) acc_orphaned && return ;;
      relay) [ -e "$ACC_DIR/beat" ] && return ;;
      bare) [ -e "$ACC_DIR/detached" ] && return ;;
      *) return ;;
    esac
    sleep 0.05
    acc_n=$((acc_n + 1))
  done
}

# Whether the `orphan` variant's grandchild has started its own session and its
# parent, the short-lived `mid`, is no longer its parent. Only then is the
# situation the gap describes in place: a descendant in a session of its own
# whose parent is gone, which no listing can tie to the worker.
acc_orphaned() {
  acc_leaf=$(sed -n 's/^leaf-setsid //p' "$ACC_DIR/pids" 2>/dev/null | head -n 1)
  [ -n "$acc_leaf" ] || return 1
  acc_mid=$(sed -n 's/^mid //p' "$ACC_DIR/pids" 2>/dev/null | head -n 1)
  acc_parent=$(ps -o ppid= -p "$acc_leaf" 2>/dev/null | tr -d ' ')
  [ "$acc_parent" != "$acc_mid" ]
}

# acc_run BEHAVIOUR:VARIANT
acc_run() {
  acc_behaviour=${1%%:*}
  acc_variant=${1#*:}
  acc_record provider $$
  if [ "$acc_variant" = stubborn ]; then
    trap '' INT TERM
  else
    trap 'exit 130' INT TERM
  fi
  # A provider that detaches a child and finishes at once is seen by the early
  # listings delega takes in the first 200 ms of a run (CAPTURE_RAMP), so the
  # gap the bare variant documents would not show. Starting it after the ramp
  # puts the child between that listing and the first periodic one.
  if [ "$acc_variant" = bare ] && [ "$acc_behaviour" = success ]; then sleep 0.3; fi
  acc_start_variant "$acc_variant"
  acc_wait_ready "$acc_variant"
  case "$acc_behaviour" in
    success) acc_init; acc_success ;;
    exit3)
      printf 'fake provider crashed\n' >&2
      exit 3
      ;;
    busy)
      acc_init
      while :; do acc_chatter; sleep 0.2; done
      ;;
    crash)
      acc_init
      # The bare variant is a direct child of the provider that left the
      # session, which delega adopts only if a listing sees it while the
      # provider is alive. 1.5 s leaves room for the early listings and
      # several periodic ones, so a machine that delays one listing does not
      # turn the row into the gap it does not mean to test.
      if [ "$acc_variant" = bare ]; then sleep 1.5; else sleep 0.5; fi
      kill -9 $$
      ;;
    hang)
      acc_init
      while :; do sleep 1; done
      ;;
    closeout)
      acc_init
      exec 1>&-
      while :; do sleep 1; done
      ;;
    nofinal) acc_init ;;
    flood)
      acc_init
      while :; do acc_chatter; done
      ;;
    slow)
      acc_init
      sleep 1.5
      acc_success
      ;;
  esac
}
