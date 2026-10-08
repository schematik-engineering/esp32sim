# EX221: short cycle slices and quad PSRAM

Base: upstream/main `017af524`. The final source is the commit containing this receipt.
Related scheduling mechanisms: EX047, EX133 and EX177. This change bounds a final
partial round; it does not change the default quantum or price instructions.
The command path reuses SpiMem and its existing dirty-memory reporting.

The ordinary scheduler clips the final round to max_cycles. Virtual and batched
rounds admit only whole quanta that fit. The synthetic spin program `06 ff ff`
runs with one or two busy cores, batching enabled/disabled, and successive
1, 255, 256, 257, 1025 and 1-cycle budgets at quantum 256.

Quad PSRAM returns three manufacturer/KGD/density bytes for the configured
2/4/8 MiB capacity on CS1 command 0x9f. The density match returns the final byte
directly; no extra ID bytes are synthesized.
Absent and unsupported capacities return an invalid ID. Quad read/write aliases
reuse the octal transfer path; octal mode-register and CS0 flash IDs remain unchanged.
No firmware fixture or golden was added or regenerated.

## Public definitions

- ESP-IDF v5.5.5 `components/esp_psram/device/esp_quad_psram_defs_ap.h`,
  lines 21-60: commands, manufacturer 0x0d, KGD 0x5d, density bits.
- ESP-IDF v4.4.8 `components/esp_hw_support/port/esp32s3/spiram_psram.c`,
  lines 46-83: the same commands, KGD and density encoding. Arduino-ESP32 2.x
  uses IDF 4.4; 3.x uses IDF 5.x. No relevant encoding difference.
- ESP-IDF v5.5.5 `components/soc/esp32s3/register/soc/spi_mem_reg.h`,
  lines 110-113, 383-391 and 560-570: USR, MISO/MOSI and CS disable bits.

Revision bits and an absent chip returning 0xff are inferred
model choices, not hardware measurements. Octal density reporting is unchanged.
No speed, RF or hardware-timing claim. No new fields or per-tick callbacks;
the scheduler adds an inline bound at the existing round boundary.

## CPU comparison

PENDING

## Verification

Darwin arm64, cargo 1.99.0, Node v22.23.1. [Commands and exit codes](checks.json)
and [additional CI checks](extra-checks.json) all pass. Fetch inputs with
`tools/fetch-demo-assets.sh --no-linux`; [SHA-256 hashes](inputs.json) identify
ROMs and public demo assets. No private captures or machine identifiers retained.

Workspace checks use an empty HOME, with CARGO_HOME and RUSTUP_HOME pointing to
the installed toolchain/cache. All ESP32SIM_* variables are removed first.
For CI policy only, set `ESP32SIM_ROM_DIR="$PWD/web/wasm/fw"`.
CI-policy tests: 643 passed. Plain tests, no ESP32SIM_* variables: 622 passed,
35 ignored. Both native and WASM Clippy deny warnings. All eight WASM demos pass.
Existing golden files are byte-identical to the base. No JIT code changed.

Focused batching check:
`ESP32SIM_VQ_NATIVE=1 cargo +1.99.0 test --release -p esp32s3 --test machine busy_runs_honor_cycle_ceiling_with_virtual_and_batched_rounds`.
The test asserts that the virtual/batched paths actually ran when enabled.

## Mutation table

Each mutation was applied alone, compiled successfully and failed the named test.
Restore the rule between runs. Scheduler mutations use the focused command above;
PSRAM mutations use `cargo +1.99.0 test --release -p esp-periph --lib cs1_quad_id_tracks_capacity_and_keeps_octal_and_flash_ids`.

| Mutation | Test that kills it |
| --- | --- |
| Remove ordinary cycle bound | `busy_runs_honor_cycle_ceiling_with_virtual_and_batched_rounds` |
| Round virtual cycle bound up with div_ceil | `busy_runs_honor_cycle_ceiling_with_virtual_and_batched_rounds` |
| Round batched cycle bound up with div_ceil | `busy_runs_honor_cycle_ceiling_with_virtual_and_batched_rounds` |
| Disable the absent-PSRAM early return | `cs1_quad_id_tracks_capacity_and_keeps_octal_and_flash_ids` |
| Change quad KGD byte from 0x5d to zero | `cs1_quad_id_tracks_capacity_and_keeps_octal_and_flash_ids` |
| Return zero for 0x200000 density | `cs1_quad_id_tracks_capacity_and_keeps_octal_and_flash_ids` |
| Return zero for 0x400000 density | `cs1_quad_id_tracks_capacity_and_keeps_octal_and_flash_ids` |
| Return zero for 0x800000 density | `cs1_quad_id_tracks_capacity_and_keeps_octal_and_flash_ids` |
| Remove 0x02 transfer alias | `cs1_quad_id_tracks_capacity_and_keeps_octal_and_flash_ids` |
| Remove 0x38 transfer alias | `cs1_quad_id_tracks_capacity_and_keeps_octal_and_flash_ids` |
| Remove 0x03 transfer alias | `cs1_quad_id_tracks_capacity_and_keeps_octal_and_flash_ids` |
| Remove 0x0b transfer alias | `cs1_quad_id_tracks_capacity_and_keeps_octal_and_flash_ids` |
| Remove 0xeb transfer alias | `cs1_quad_id_tracks_capacity_and_keeps_octal_and_flash_ids` |
