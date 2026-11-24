#!/bin/sh
# A descendant of a fake provider in the acceptance harness's `acc:` modes.
# Publishes its pid in `pids/` beside this script, then idles. See acc-lib.sh.
ACC_DIR=$(dirname "$0")
record() { : > "$ACC_DIR/pids/$1.$2"; }
idle() { while :; do sleep 1; done; }
detach='use POSIX; POSIX::setsid(); $SIG{INT}="DEFAULT"; exec @ARGV'
case "$1" in
  plain) record child $$; idle ;;
  ignore) trap '' TERM INT; record child-ignore $$; idle ;;
  mid)
    record mid $$
    # Move the grandchild into its own session, out of the provider's group.
    perl -e "$detach" sh "$ACC_DIR/acc-child.sh" leaf &
    wait
    ;;
  orphan | orphan-late)
    record mid $$
    perl -e "$detach" sh "$ACC_DIR/acc-child.sh" leaf &
    [ "$1" = orphan ] || sleep 1.3
    exit 0
    ;;
  leaf) record leaf-setsid $$; idle ;;
  relay)
    # Each generation ignores TERM, leads a group of its own, lives a few
    # milliseconds, touches the heartbeat file `beat`, forks a successor and
    # exits, so no process stays alive for longer than one process listing. It
    # stops by itself after 1500 generations, or as soon as the file `stop`
    # exists (the harness creates it). The first process stays as a plain
    # child of the provider. Every generation publishes `relay-heartbeat.PID` in
    # the ledger as its first act, so the harness attributes it to its role
    # instead of finding it unrecorded.
    record relay $$
    exec perl -e 'use POSIX; my ($beat, $stop, $pids) = @ARGV;
      my $p = fork();
      if ($p == 0) {
        open(my $record, q(>), "$pids/relay-heartbeat.$$") or exit 1; close($record);
        $SIG{TERM} = q(IGNORE);
        for (1..1500) { last if -e $stop; setpgid(0,0);
          open H, q(>), $beat; print H $$; close H;
          select(undef, undef, undef, 0.005);
          my $q = fork();
          if ($q) { exit 0 }
          open(my $record, q(>), "$pids/relay-heartbeat.$$") or exit 1; close($record); }
        exit 0 }
      sleep 60' "$ACC_DIR/beat" "$ACC_DIR/stop" "$ACC_DIR/pids"
    ;;
esac
