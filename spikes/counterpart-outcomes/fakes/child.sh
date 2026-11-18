#!/bin/bash
# A descendant of the fake provider. Records its pid, then idles.
record() { printf '%s %s\n' "$1" "$2" >> "$SPIKE_DIR/pids"; }
idle() { while :; do sleep 1; done; }
case "$1" in
  plain)  record child $$; idle ;;
  ignore) trap '' TERM INT; record child-ignore $$; idle ;;
  mid)
    record mid $$
    # Move the grandchild into its own session, out of the provider's group.
    perl -MPOSIX -e 'POSIX::setsid(); $SIG{INT}="DEFAULT"; exec @ARGV' bash "$SPIKE_DIR/child.sh" leaf &
    wait
    ;;
  orphan|orphan-late)
    record mid $$
    perl -MPOSIX -e 'POSIX::setsid(); $SIG{INT}="DEFAULT"; exec @ARGV' bash "$SPIKE_DIR/child.sh" leaf &
    [ "$1" = orphan ] || sleep 1.3
    exit 0
    ;;
  leaf)   record leaf-setsid $$; idle ;;
esac
