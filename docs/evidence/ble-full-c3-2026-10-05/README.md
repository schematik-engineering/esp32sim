# EX210: execute the unmodified C3 BLE controller

Base: upstream esp32sim `29dd4623307853d8c036b65c5bb3249fa131758c`.
Arduino-ESP32 3.3.11, ESP-IDF 5.5.5, C3 rev3 ROM, Rust 1.99.0.

Milestone A is not reached. With no `--ble`, baseline execution stalls in
`r_rwip_time_get`. Diagnostic register pokes let controller initialization and
advertising setup finish, but the controller's first advertising alarm has no
hardware event/interrupt implementation. No radio PDU was emitted or decoded.
The application success message does not establish advertising transmission.

EX200 substitutes the controller at HCI/VHCI. EX210 instead executes that
controller and investigates its MMIO. No earlier EX200 result is superseded.
No emulator source, hot path, JIT, interrupt routing or device table changed.
The prototype is `replay.py`, using existing CLI script/debug facilities.
It is a diagnostic experiment, not a proposed production hardware model.

## Reproduce

Install Arduino-ESP32 3.3.11 and build its official Server example. Supply
`ARDUINO` as the installed core directory, and worktree-local `BUILD`/`OUTPUT`:

```sh
arduino-cli compile --fqbn esp32:esp32:esp32c3 \
  --build-path "$BUILD" "$ARDUINO/libraries/BLE/examples/Server"
cargo +1.99.0 build --release --bins
python3 docs/evidence/ble-full-c3-2026-10-05/replay.py \
  --emulator target/release/esp32sim-c3 --build "$BUILD" \
  --rom web/wasm/fw/esp32c3_rev3_rom.elf --output "$OUTPUT"
```

On the measured ARM macOS host, installed ctags was an incompatible Intel
executable. The final build additionally passed
`--build-property "runtime.tools.ctags.path=$CTAGS"`. `CTAGS` contained a native
build of <https://github.com/arduino/ctags/tree/5.8-arduino11>:
run `./configure`, rename `__unused__` to `CTAGS_UNUSED` throughout its C/H
sources to avoid the SDK macro collision, then `make -j4`. The obsolete
configure script created its Makefile/config.h but returned 2 after its probes;
make was run separately. No Arduino source, configuration or blob was edited.
A prior identical-sketch-in-CPP attempt also failed on ctags and was abandoned.

The replay runs six stages for one modeled second each, with fixed expected
profile symbols and a check that only the last stage reports application
success. It preserves the assertion failure in stage 1. It does not observe
radio packets, compare speed, or establish timing fidelity.

For the ordered traces, use this common command, without `--ble` or stubs:

```sh
target/release/esp32sim-c3 --boot rom \
  --rom web/wasm/fw/esp32c3_rev3_rom.elf --flash-mb 4 \
  --bootloader "$BUILD/Server.ino.bootloader.bin" \
  --ptable "$BUILD/Server.ino.partitions.bin" \
  --app "$BUILD/Server.ino.bin" --elf "$BUILD/Server.ino.elf" \
  --max-seconds 0.04 --no-dump --debug mmio \
  --trace-fn esp_bt_controller --trace-fn btdm_controller
```

For `enable-mmio`, change the duration to `0.3`, add
`--script "$OUTPUT/stage-5.script"`, and add `--trace-fn` for each of
`r_lld_adv`, `r_sch`, `esp_phy`, `coex`. Redirect stdout and stderr separately
into the caller's output directory. Default execution engine; no `--no-jit`.

## Findings

- ROM `r_rwip_time_get`, PC `0x4002ee78`, polls bit 31 of `0x6003101c`.
  Baseline trace: 778,414 reads there in 0.04 modeled seconds. The separate
  one-second profile assigns 48.33% each to load and branch.
- Stage 1 supplies coarse snapshots at `0x6003101c/20`. It reaches
  `BLE assert lld.c 324, param 00000000 09001b00`.
- `r_lld_core_init` requires `0x60031004 == 0x09001b00` for this specimen.
- Stages 2–4 expose repeated bit-31 completion polls at `0x60031000`, in
  `r_rwip_driver_init` and `r_rwble_hw_disable`.
- Stage 5 clears the selected completions and reaches
  `r_lld_adv_start_schedule_asap` at 0.2590 modeled seconds.
- ROM `r_rwip_timer_hus_set` writes `0x33f`/`0x270` at `0x600310ec/f0`,
  writes `0x800` at `0x60031018`, and sets bit 11 at `0x6003100c`.
  These are inferred alarm, acknowledgement and mask semantics, not a
  published register specification.
- Interrupt matrix `0x600c2020 = 9` routes source 8, RWBLE, to CPU interrupt 9.
  The final run reports no delivery of that interrupt and waits mostly idle.
  No scheduler packet-programming or advertising event-start callback appears
  in the selected trace. Counter/alarm IRQ, event table and TX completion are
  the next hardware work, followed by packet extraction and decoding.

The replay's coarse snapshot formula is deliberately provisional. It supplies
`floor(seconds * 3200)` and subcount 624 every 100 microseconds. ROM time-read
arithmetic suggests a coarse/fine clock, but the script does not implement
atomic latching, valid reset behavior or deadline correctness. Reset writes
are scheduled for this exact firmware. No silicon comparison was performed.

## Files and attribution

- `inputs.json`: built firmware, ROM, libraries and matching IDF header hashes.
- `stages.json`: six outcomes, exact modeled work, failed stages and profiles.
- `mmio-summary.json` / `enable-mmio-summary.json`: selected radio/clock/IRQ
  block counts and first accesses, plus highest-count reads.
- `mmio-registers.tsv`: baseline selected-block summary by address/direction/PC.
- `enable-controller.tsv`, `enable-clocks-irq.tsv`, `enable-phy.tsv`: the final
  trace summarized by hardware category. PHY omits the redundant symbol column;
  PC plus the pinned ELF/ROM resolves it. These are access aggregates, not raw
  event streams. `first_event` preserves first-access order. Repeated accesses
  retain counts and first/last values. Host script writes are excluded from
  counts; event indices include them. The selection omits unrelated UART,
  flash, general timer and GPIO traffic. NRX is grouped by 4 KiB page, not the
  public sub-page base. Nearest-symbol labels outside actual function extents,
  notably bootloader `_iram_text_end+...`, are not function attribution.
- `function-order.txt`: selected function-entry evidence from the final trace.
  Adjacent duplicate names can be ROM entry veneer and body, not two calls.
- `polls-disassembly.txt`: exact instructions establishing the poll/comparison
  and inferred timer semantics. Addresses are for this specimen only.
- `checks.json`: commands/results; goldens unchanged, default toolchain unchanged.
- `captures.json`: original/sanitized log hashes and local compressed filenames.

Public register facts were checked against IDF **v5.5.5**, matching the firmware:
[reg_base.h](https://github.com/espressif/esp-idf/blob/v5.5.5/components/soc/esp32c3/register/soc/reg_base.h),
[system_reg.h](https://github.com/espressif/esp-idf/blob/v5.5.5/components/soc/esp32c3/register/soc/system_reg.h),
[syscon_reg.h](https://github.com/espressif/esp-idf/blob/v5.5.5/components/soc/esp32c3/register/soc/syscon_reg.h),
[interrupts.h](https://github.com/espressif/esp-idf/blob/v5.5.5/components/soc/esp32c3/include/soc/interrupts.h).
The [controller wrapper](https://github.com/espressif/esp-idf/blob/v5.5.5/components/bt/controller/esp32c3/bt.c)
provides lifecycle context. `0x60031xxx` and `0x60011xxx` behavior here is
blob/ROM-derived; no complete public field map was found. No IDF 4.4 or other
chip register equivalence is assumed.

## Retention and limits

Full normalized captures and the firmware specimen remain in the local notes
folder `ble-full-artifacts/`; access was verified before deleting worktree
scratch. They are not public artifacts. No upload or GitHub action was made.
The ELF is retained locally only and may contain build-path debug metadata.
The committed receipt contains no firmware binary or local absolute path.
Home/worktree labels in retained logs are replaced with placeholders;
`captures.json` records both hashes. Redaction changes no numeric sample,
protocol byte, instruction count or event order. The curated evidence can be
challenged using the replay and disassembly without private captures.

Single native runs under uncontrolled host load and heavy diagnostic output
are not CPU benchmarks. No speed or hardware timing claim follows. Since no
emulator code changed, the CONTRIBUTING speed comparison is not applicable.
No JIT code changed. The requested conditional JIT check is not applicable.
