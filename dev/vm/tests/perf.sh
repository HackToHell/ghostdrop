#!/bin/bash
# Usage (inside VM): perf.sh sway|x11   — prints latency/throughput/CPU numbers.
source ~/vmenv.sh
if [ "$1" = x11 ]; then x11_env; log=/tmp/gd-x11.log; else sway_env; log=/tmp/gd-sway.log; fi
gd() { ~/bin/ghostdrop "$@"; }
gd ctl quit force >/dev/null 2>&1; sleep 0.6
GHOSTDROP_TRACE=1 setsid -f ~/bin/ghostdrop --daemon > $log 2>&1 </dev/null; sleep 1.5
pid=$(pgrep -n -f "ghostdrop --daemon")
[ -f /tmp/big.txt ] || python3 -c "
import random; random.seed(1); w=['alpha','beta','gamma','delta','\x1b[31mred\x1b[0m','\x1b[1;32mbold\x1b[0m','日本語','─│┼']
open('/tmp/big.txt','w').write('\n'.join(' '.join(random.choice(w) for _ in range(14)) for _ in range(330000)))"
for i in 1 2 3 4 5; do gd ctl show >/dev/null; sleep 0.4; gd ctl hide >/dev/null; sleep 0.3; done
echo "show->first frame (ms):" $(grep -a -o "first frame [0-9.]* ms" $log | awk '{print $3}' | tr '\n' ' ')
s=$(date +%s%N); for i in $(seq 10); do ~/ghostdrop/linux/target/release/ghostdrop ctl version >/dev/null 2>&1 || GHOSTDROP_SOCKET= true; done; e=$(date +%s%N)
echo "client round-trip: $(( (e-s)/10000000 )).$(( (e-s)/100000 % 100 )) ms avg (10 runs, fork+exec+ipc)"
gd ctl show >/dev/null; sleep 0.5; rm -f /tmp/t.txt
gd ctl binding 'text:clear; s=$(date +%s%N); cat /tmp/big.txt; e=$(date +%s%N); echo $(( (e-s)/1000000 )) > /tmp/t.txt\r' >/dev/null
for i in $(seq 120); do [ -s /tmp/t.txt ] && break; sleep 0.25; done
sz=$(stat -c %s /tmp/big.txt); ms=$(cat /tmp/t.txt)
echo "cat $((sz/1048576)) MiB: ${ms} ms -> $(( sz / 1048576 * 1000 / (ms>0?ms:1) )) MiB/s"
cpu() { awk '{print $14+$15}' /proc/$pid/stat; }
sleep 1; a=$(cpu); sleep 5; b=$(cpu); echo "visible idle CPU: $(( (b-a) * 100 / 500 ))% (5s)"
gd ctl hide >/dev/null; sleep 1; a=$(cpu); sleep 5; b=$(cpu); echo "hidden CPU: $(( (b-a) * 100 / 500 ))% (5s)"
echo "RSS: $(( $(awk '/VmRSS/{print $2}' /proc/$pid/status) / 1024 )) MiB"
