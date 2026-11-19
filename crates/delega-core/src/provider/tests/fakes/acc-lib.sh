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
#   crash     SIGKILL itself after 0.5 s
#   hang      no output, never finishes
#   closeout  close standard output, then never finish
#   nofinal   output ends without a final answer, exit 0
#   flood     output at full speed, never finishes
#   slow      finish normally after 1.5 s
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
#   relay        a fork relay (see acc-child.sh)

ACC_DIR=$(dirname "$0")

acc_record() { printf '%s %s\n' "$1" "$2" >> "$ACC_DIR/pids"; }

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
  esac
}

# Wait (at most 3 s) until the descendants of a variant exist, so a provider
# that finishes at once does so with them running. The run then measures
# delega's cleanup, not whether the descendants had started yet.
acc_wait_ready() {
  acc_n=0
  while [ $acc_n -lt 60 ]; do
    case "$1" in
      child | pipe) grep -q '^child ' "$ACC_DIR/pids" 2>/dev/null && return ;;
      ignore) grep -q '^child-ignore ' "$ACC_DIR/pids" 2>/dev/null && return ;;
      setsid | orphan | orphan-late) grep -q '^leaf-setsid ' "$ACC_DIR/pids" 2>/dev/null && return ;;
      relay) [ -e "$ACC_DIR/beat" ] && return ;;
      *) return ;;
    esac
    sleep 0.05
    acc_n=$((acc_n + 1))
  done
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
  acc_start_variant "$acc_variant"
  acc_wait_ready "$acc_variant"
  case "$acc_behaviour" in
    success) init; acc_success ;;
    exit3)
      printf 'fake provider crashed\n' >&2
      exit 3
      ;;
    busy)
      init
      while :; do acc_chatter; sleep 0.2; done
      ;;
    crash)
      init
      sleep 0.5
      kill -9 $$
      ;;
    hang)
      init
      while :; do sleep 1; done
      ;;
    closeout)
      init
      exec 1>&-
      while :; do sleep 1; done
      ;;
    nofinal) init ;;
    flood)
      init
      while :; do acc_chatter; done
      ;;
    slow)
      init
      sleep 1.5
      acc_success
      ;;
  esac
}
