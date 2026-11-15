#!/bin/bash
# Run the 3-role scenario with all roles played by the Rust core (sanity check).
set -uo pipefail
cd "$(dirname "$0")/.."
BIN=${SC_PEER:-target/release/sc-peer}
WORK=$(mktemp -d)
head -c 3500000 /dev/urandom > "$WORK/testfile.bin"
export SC_TEST_FILE="$WORK/testfile.bin"
export SC_TEST_SHA=$( (sha256sum "$WORK/testfile.bin" 2>/dev/null || shasum -a 256 "$WORK/testfile.bin") | cut -d' ' -f1)
run() { "$BIN" "$1" "$WORK" > "$WORK/$1.log" 2>&1; echo $? > "$WORK/$1.exit"; }
run S & sleep 0.3; run R1 & sleep 0.3; run R2 &
wait
FAIL=0
for r in S R1 R2; do
  echo "================ $r (exit $(cat $WORK/$r.exit)) ================"
  grep -E "PASS|FAIL|RESULT" "$WORK/$r.log"
  [ "$(cat $WORK/$r.exit)" = "0" ] || FAIL=1
done
[ $FAIL = 0 ] || { for r in S R1 R2; do echo "#### $r"; tail -60 "$WORK/$r.log"; done; }
exit $FAIL
