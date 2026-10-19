#!/bin/bash
# Offline reproduction. No device access or synthetic ROM contents.
set -euo pipefail
cd "$(dirname "$0")/.."
: "${S31_ROM:?set S31_ROM to the pinned public ELF}"
: "${S31_ROM_DATA:?set S31_ROM_DATA to the M1c ROM dump}"
: "${CARGO_TARGET_DIR:?set CARGO_TARGET_DIR outside the control checkout}"
OUT="$CARGO_TARGET_DIR/s31-m1d-reproduction"
mkdir -p "$OUT"
python3 - "$S31_ROM" "$S31_ROM_DATA" <<'PY'
import hashlib
from pathlib import Path
import sys
for path, expected in zip(sys.argv[1:], [
    'a52ad7513deb656a910a5740125f1cce2c7941f11ce57213b7b43aea93d5ab87',
    '45376d77d7c7d0e9c3359a0d6eb2aa1e369aecb181fee32abc16f93b431c002a',
]):
    assert hashlib.sha256(Path(path).read_bytes()).hexdigest() == expected, 'wrong input revision'
PY
nice -n 10 cargo +1.99.0 build --release -p esp32s31
nice -n 10 cargo +1.99.0 test --release -p esp32s31
nice -n 10 cargo +1.99.0 test --release -p esp32s31 external_ -- --ignored
for n in 1 2; do
    "$CARGO_TARGET_DIR/release/esp32sim-s31" --chip esp32s31 --rom "$S31_ROM" --max-instructions 1000000 --rom-data "$S31_ROM_DATA" > "$OUT/stop-$n.txt"
done
cmp "$OUT/stop-1.txt" "$OUT/stop-2.txt"
printf '%s\n' '[s31] chip=esp32s31 pc=0x2f83e844 opcode=0xbcc49073 retired=6334 stop=Csr(3020)' > "$OUT/expected.txt"
cmp "$OUT/expected.txt" "$OUT/stop-1.txt"
printf 'Two identical runs: '; printf '%s\n' "$(< "$OUT/stop-1.txt")"
python3 -m unittest discover -s s31 -p 'test_*.py' -v
