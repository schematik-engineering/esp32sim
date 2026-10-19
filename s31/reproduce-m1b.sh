#!/bin/bash
# Offline only; caller supplies the pinned public ROM. No device access.
set -euo pipefail
cd "$(dirname "$0")/.."
: "${S31_ROM:?set S31_ROM to esp32s31_rev0_rom.elf}"
: "${CARGO_TARGET_DIR:?set CARGO_TARGET_DIR outside the control checkout}"
OUT="$CARGO_TARGET_DIR/s31-m1b-reproduction"
mkdir -p "$OUT"
python3 - "$S31_ROM" <<'PY'
import hashlib
from pathlib import Path
import sys
assert hashlib.sha256(Path(sys.argv[1]).read_bytes()).hexdigest() == 'a52ad7513deb656a910a5740125f1cce2c7941f11ce57213b7b43aea93d5ab87', 'wrong ROM revision'
PY
nice -n 10 cargo +1.99.0 build --release -p esp32s31 --bin esp32sim-s31
nice -n 10 cargo +1.99.0 test --release -p esp32s31
nice -n 10 cargo +1.99.0 test --release -p esp32s31 external_pinned_rom_first_stop -- --ignored
for n in 1 2; do
    "$CARGO_TARGET_DIR/release/esp32sim-s31" --chip esp32s31 --rom "$S31_ROM" --max-instructions 1000 > "$OUT/stop-$n.txt"
done
cmp "$OUT/stop-1.txt" "$OUT/stop-2.txt"
EXPECTED='[s31] chip=esp32s31 pc=0x2f801812 opcode=0x0007a283 retired=44 stop=access address=0x2f83f700 width=4 write=false fault=Unmapped'
printf '%s\n' "$EXPECTED" > "$OUT/expected.txt"
cmp "$OUT/expected.txt" "$OUT/stop-1.txt"
printf '%s\n' "$EXPECTED"
python3 -m unittest discover -s s31 -p 'test_*.py' -v
