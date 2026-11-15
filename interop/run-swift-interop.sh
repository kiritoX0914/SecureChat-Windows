#!/bin/bash
# Live interop: Swift ChatManager (role S) vs two Rust sc-peer processes (R1, R2).
# Usage: interop/run-swift-interop.sh <path to SecureChat-macOS checkout>
set -uo pipefail
cd "$(dirname "$0")/.."
SWIFT_SRC=${1:?path to SecureChat-macOS checkout}/SecureChat
WORK=$(mktemp -d)
mkdir -p "$WORK/src"
cp "$SWIFT_SRC"/Models/Models.swift "$SWIFT_SRC"/Crypto/*.swift "$SWIFT_SRC"/Network/*.swift \
   "$SWIFT_SRC"/Core/PersistenceManager.swift "$WORK/src/"
sed 's/^import UIKit$//' "$SWIFT_SRC"/Core/ChatManager.swift > "$WORK/src/ChatManager.swift"
cp interop/UIKitShim.swift interop/InteropMain.swift "$WORK/src/"
mv "$WORK/src/InteropMain.swift" "$WORK/src/main.swift"
xcrun swiftc -swift-version 5 -O -target "$(uname -m)-apple-macos14.0" -o "$WORK/swift-peer" "$WORK"/src/*.swift || exit 1

BIN=${SC_PEER:-target/release/sc-peer}
head -c 3500000 /dev/urandom > "$WORK/testfile.bin"
export SC_TEST_FILE="$WORK/testfile.bin"
export SC_TEST_SHA=$(shasum -a 256 "$WORK/testfile.bin" | cut -d' ' -f1)
mkdir -p "$WORK/home-S/Documents" "$WORK/home-S/Library/Preferences" "$WORK/home-S/Library/Application Support"

( HOME="$WORK/home-S" CFFIXED_USER_HOME="$WORK/home-S" "$WORK/swift-peer" > "$WORK/S.log" 2>&1; echo $? > "$WORK/S.exit" ) &
sleep 0.5
( SC_LOG=1 "$BIN" R1 "$WORK" > "$WORK/R1.log" 2>&1; echo $? > "$WORK/R1.exit" ) &
sleep 0.3
( SC_LOG=1 "$BIN" R2 "$WORK" > "$WORK/R2.log" 2>&1; echo $? > "$WORK/R2.exit" ) &
wait
FAIL=0
for r in S R1 R2; do
  echo "================ $r (exit $(cat $WORK/$r.exit)) ================"
  grep -E "PASS|FAIL|RESULT" "$WORK/$r.log"
  [ "$(cat $WORK/$r.exit)" = "0" ] || FAIL=1
done
mkdir -p interop-logs && cp "$WORK"/*.log interop-logs/ 2>/dev/null
if [ $FAIL != 0 ]; then
  for r in S R1 R2; do echo "######## $r full"; grep -v "^\s*$" "$WORK/$r.log" | tail -120; done
fi
exit $FAIL
