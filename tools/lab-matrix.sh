#!/usr/bin/env bash
# Stage 1 lab run: start seihai-gserver on 127.0.0.1 in a throwaway world,
# then log every Seihai client profile in with the real seihai-mae binary
# (headless, under xvfb-run) and keep each connected for STAY seconds.
#
#   tools/lab-matrix.sh PATH/TO/seihai-mae [STAY]
#
# Loopback only. Accounts are made on first login with a lab password.
set -u
MAE=${1:?path to seihai-mae}
STAY=${2:-60}
HERE=$(cd "$(dirname "$0")/.." && pwd)
GS=${GSERVER:-$HERE/target/release/seihai-gserver}
PORT=${PORT:-14802}
LAB=$(mktemp -d)
mkdir -p "$LAB/world/config" "$LAB/out"
printf 'create_accounts = true\nport = %s\n' "$PORT" > "$LAB/world/config/server.txt"
echo '*' > "$LAB/world/config/allowedversions.txt"
"$GS" --world "$LAB/world" > "$LAB/gserver.log" 2>&1 &
GSPID=$!
trap 'kill $GSPID 2>/dev/null; wait $GSPID 2>/dev/null' EXIT
for _ in $(seq 50); do grep -q listening "$LAB/gserver.log" && break; sleep 0.1; done

LABELS=("1.21" "1.28 beta 2" "1.3 test" "1.31" "1.32" "1.33" "1.34" "1.35" "1.36" "1.37"
  "1.38" "1.39" "1.41" "1.42" "2.00" "2.01" "2.02" "2.02 rev2" "2.03" "2.04" "2.05" "2.1"
  "2.12" "2.13" "2.14" "2.15" "2.16" "2.17" "2.22" "2.3" "2.31" "3.04" "3.153" "4.04r2"
  "4.20r6" "4.20r8" "5.00r7" "5.12" "5.24r4" "5.30r3" "5.31r4" "6.00r2" "6.015" "6.034"
  "6.03r7" "6.03r7 Linux")
export GRAAL_PW=labpw
i=0
for v in "${LABELS[@]}"; do
  i=$((i + 1))
  (
    mkdir -p "$LAB/home$i"
    # Own HOME each: no saved settings can replace --version.
    HOME="$LAB/home$i" timeout "$((STAY + 5))" xvfb-run -a "$MAE" --online "127.0.0.1:$PORT" \
      --account "lab$i" --password-env GRAAL_PW --version "$v" --no-sound --no-save-downloads \
      > "$LAB/out/$i.log" 2>&1
  ) &
  sleep 0.2
done
wait $(jobs -p | grep -v "^$GSPID$") 2>/dev/null
sleep 1
fail=0
i=0
for v in "${LABELS[@]}"; do
  i=$((i + 1))
  log="$LAB/out/$i.log"
  ok=yes
  grep -q "logged in to 127.0.0.1:$PORT" "$log" || ok="no login"
  [ "$ok" = yes ] && { grep -q "level from the server's board" "$log" || ok="no level"; }
  [ "$ok" = yes ] && { grep -qiE "panic|connection closed|disconnected" "$log" && ok="dropped"; }
  # The server saw the account leave only when the client was stopped.
  left=$(grep " lab$i left" "$LAB/gserver.log" | sed 's/^\[ *\([0-9.]*\)\].*/\1/')
  joined=$(grep " lab$i \[" "$LAB/gserver.log" | sed 's/^\[ *\([0-9.]*\)\].*/\1/')
  if [ "$ok" = yes ] && [ -n "$left" ] && [ -n "$joined" ]; then
    awk -v a="$joined" -v b="$left" -v s="$STAY" 'BEGIN { exit !(b - a >= s) }' || ok="left after $(awk -v a="$joined" -v b="$left" 'BEGIN{print b-a}') s"
  fi
  if [ "$ok" = yes ]; then
    printf 'ok    %-14s %s, %s s connected\n' "$v" "$(grep -o " lab$i \[[^]]*\]" "$LAB/gserver.log" | head -1 | sed 's/^ lab[0-9]* //')" "$(awk -v a="$joined" -v b="$left" 'BEGIN{printf "%.0f", b-a}')"
  else
    printf 'FAIL  %-14s %s (log %s)\n' "$v" "$ok" "$log"
    fail=1
  fi
done
echo "server log: $LAB/gserver.log"
exit $fail
