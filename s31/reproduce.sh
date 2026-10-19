#!/bin/bash
# Offline only: no serial, USB, flash, simulator stubs or application entry bypass.
set -euo pipefail
cd "$(dirname "$0")/.."
: "${S31_ROM:?set S31_ROM to esp32s31_rev0_rom.elf}"
: "${S31_REF_DIR:?set S31_REF_DIR to the accepted M0 s31 directory}"
: "${RISCV_BIN:?set RISCV_BIN to the pinned riscv32-esp-elf/bin directory}"
: "${CARGO_TARGET_DIR:?set CARGO_TARGET_DIR to a scratch build directory}"
OUT="$CARGO_TARGET_DIR/s31-reproduction"
mkdir -p "$OUT"
nice -n 10 cargo +1.99.0 build --release -p esp32s31 --bin esp32sim-s31
nice -n 10 cargo +1.99.0 test --release -p esp32s31
nice -n 10 cargo +1.99.0 test --release -p esp32s31 external_pinned_rom_first_stop -- --ignored
for n in 1 2; do
    "$CARGO_TARGET_DIR/release/esp32sim-s31" --chip esp32s31 --rom "$S31_ROM" --max-instructions 1000 > "$OUT/stop-$n.txt"
done
cmp "$OUT/stop-1.txt" "$OUT/stop-2.txt"
EXPECTED='[s31] chip=esp32s31 pc=0x2f801724 opcode=0x00032303 retired=6 stop=access address=0x20701030 width=4 write=false fault=Unmapped'
printf '%s\n' "$EXPECTED" > "$OUT/expected.txt"
cmp "$OUT/expected.txt" "$OUT/stop-1.txt"
printf '%s\n' "$EXPECTED"
python3 -m unittest discover -s s31 -p 'test_*.py' -v
python3 s31/audit_isa.py --rom "$S31_ROM" \
    --app "$S31_REF_DIR/reference/hello_world/build/hello_world.elf" \
    --bootloader "$S31_REF_DIR/reference/hello_world/build/bootloader/bootloader.bin" \
    --objdump "$RISCV_BIN/riscv32-esp-elf-objdump" --objcopy "$RISCV_BIN/riscv32-esp-elf-objcopy" \
    --probe "$CARGO_TARGET_DIR/release/esp32sim-s31" --work "$OUT/audit" --out s31
