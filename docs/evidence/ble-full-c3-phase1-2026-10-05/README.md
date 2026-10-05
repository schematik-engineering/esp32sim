# EX210 phase 1: modeled C3 controller startup and timer

Continues the [feasibility spike](../ble-full-c3-2026-10-05/README.md), commit
`52fa5237`, on upstream main `29dd4623307853d8c036b65c5bb3249fa131758c`.
The material change is replacing scheduled diagnostic pokes with an opt-in
register device driven by emulated CPU cycles. Implementation commit `609e7e0c`. Same unchanged Arduino 3.3.11,
IDF 5.5.5 Server specimen and C3 rev3 ROM; hashes in `result.json` match the spike.
No hardware oracle, RF transmission, HCI substitution, guest patch or stub.

## Result

The sketch prints `Characteristic defined! Now you can read it in your phone!`.
Alarm `600310ec/f0 = 0338/0270` selects coarse 824, fine zero,
exactly 0.2575 seconds in this model. At instruction 41,200,117, 0.2575 modeled seconds, CPU interrupt 9 enters
`r_rwble_isr_hack`. The matrix maps peripheral source 8 to that line. The guest
calls `r_sch_arb_event_start_isr_hack`, `r_lld_adv_evt_start_cbk`, `r_sch_prog_push`
and `r_sch_prog_ble_push_hack`. It writes event entry 0 and kicks it with
`0x60031100 = 0x80000000` at PC `0x40387638`. The one-second run ends with zero
exceptions, 1,045 interrupts, exactly one source-8 interrupt and 160,000,000
modeled instructions/cycles including idle accounting. No event completion
is generated, so there is no next advertising event or emitted PDU.
This meets phase 1, not milestone A from the feasibility report.

The initial status-only implementation reached application success but entered
337,500 source-8 interrupts without event programming. The application patch
uses the interrupt FIFO at `0x600312d8`, not just the ROM's status polling path.
Adding the observed FIFO count/status/pop behavior resolved that negative run.

## Reproduce

Caller supplies an unchanged built Server directory and output directory inside
the worktree or notes tree. Keep ROM and build inputs identical to `result.json`.

```sh
cargo +1.99.0 build --release --bins
python3 docs/evidence/ble-full-c3-phase1-2026-10-05/replay.py \
  --emulator target/release/esp32sim-c3 --build "$BUILD" \
  --rom web/wasm/fw/esp32c3_rev3_rom.elf --output "$OUTPUT"
ESP32SIM_BLE_SERVER_DIR="$BUILD" ESP32SIM_ROM_DIR="$PWD/web/wasm/fw" \
  cargo +1.99.0 test --release -p esp32sim \
  external_full_ble_server_programs_first_event -- --ignored
```

The CLI mode is `--ble full`, C3 only and mutually exclusive with `--ble`.
The existing HCI mode remains unchanged. Full mode does not install hooks and
needs no application ELF to operate; replay supplies ELF symbols for tracing.
RF-interface/BB `0x60011000` remains in existing register RAM. Existing analog
I2C/FE calibration suffices for this specimen. That is not RF validation.

## Register contract and its evidence

Every controller field here is **inferred** from the pinned disassembly.
Only the interrupt source is a public-header fact: IDF v5.5.5
`components/soc/esp32c3/include/soc/interrupts.h`, `ETS_RWBLE_INTR_SOURCE = 8`.
That version matches this firmware; no IDF 4.4 compatibility is asserted.

| Address | Modeled behavior | Guest consumer and limits |
| --- | --- | --- |
| `60031000` bit 31 | Self-clear after 80 modeled CPU cycles; cancel alarm/raw/FIFO state and preserve other control bits | `r_rwip_driver_init` poll `42048514`, `r_rwble_hw_disable` poll `42047f52`. The 0.5 us completion latency and retained configuration are model choices awaiting silicon. |
| `60031004` | Read-only `09001b00` | `r_lld_core_init`, load `4203ce76`, required comparison. Individual identity fields unknown. |
| `6003101c/20` | Bit-31 snapshot request, atomic completion after 80 cycles; coarse 28-bit half-slot and fine downcount 624..0 | `r_rwip_time_get`, `4002ee5c..4002eeb2`. Model time is `cycles / 80` half-us units; coarse divides by 625. Snapshot records completion time even across a larger tick. No read advances time. Counter epoch and oscillator behavior unvalidated. |
| `6003100c/10/18` | Mask, masked status, write-one-to-clear; acknowledgement reads zero | ROM `r_rwble_isr` load `4002e8ee`, app `r_rwble_isr_hack`, `r_rwip_timer_hus_set`. Only timer bit 11 can become pending. Unused offsets keep ordinary register storage. |
| `600310ec/f0` | Coarse target and `624-hus` fine target; fine write arms a one-shot alarm | `r_rwip_timer_hus_set`, stores `4002f312/34c`. Half-period modular comparison handles wrap; past targets are due at the next scheduler tick. Commit edge, past/invalid-target behavior and timing relative to mask/ack need silicon validation. |
| `600312d8` | Count bit 5, status shifted by 10, pop bit 0, FIFO reset bit 31; retain configuration bits 1..4 | App `r_rwble_isr_hack`, loads/extracts `403869a6..b2`, pop `403869bc`; `r_lld_core_init` resets/configures the FIFO. One coalesced timer source only. No event-source queue, overflow or hardware FIFO-depth claim. |

The register model uses the existing optional-device CPU clock and cached source
bits. Disabled mode has no tick callback. Scheduler deadlines bound idle waits;
existing instruction batching still bounds delivery precision. Neither native
instruction accounting nor the zero observed IRQ latency establishes silicon
clock fidelity. There is no clock gating, low-power sleep, RF arbitration,
controller-wide reset specification or packet engine in this phase.

## Phase-2 map

The tables and descriptor layouts below are inferred, not a public hardware ABI.
`registers.tsv` contains MMIO by address/direction/PC with first/last values and
counts, excluding diagnostic peek reads. `result.json` includes the exact SRAM snapshots and function order.
`event-stores.txt` binds event writes to guest register values.

* `r_emi_em_base_init` writes mapping entries beginning at `60031204`.
  `r_emi_get_mem_addr_by_offset` reads them at ROM `400069f6/40006a68`.
  Logical start is `(entry >> 18) << 2`; SRAM base is
  `3fc00000 | ((entry << 2) & 000ffffc)`, plus the logical offset within that
  segment. SRAM storage already exists; phase 2 must consume this mapping.
* Logical `0000` maps to `3fca7794`, entry `00029de5`. Event entries are 16
  bytes, derived from `r_sch_prog_ble_push_hack` shifting the index by four.
  Entry 0 is `033b2802 02700000 0af70200 0f000c00`. At `40387426` it stores
  `0f00` at +14; at `40387450`, `0200` at +8; at `403874fa`, `2802` at +0.
  The +8 halfword points to the control structure in halfword units, logical
  `0400`. The event kick is `60031100`, index in the low nibble plus bit 31.
  Completion/status ownership and exact scheduling fields still need validation.
* Logical `0400` maps to `3fca78f8`, entry `04029e3e`. Control structures have
  90-byte stride, from the multiply in `r_sch_prog_ble_push_hack`. The guest
  writes them in `r_lld_adv_start_set_cs` and the advertising callback.
  Snapshot +12 contains `8e89bed6`, the advertising access-address value;
  +28 contains `00001400`, a candidate TX-descriptor logical pointer. Channel,
  ownership, length, whitening/CRC and completion flags require field-by-field
  tracing before an event engine consumes them.
* Logical `1400` maps to `3fca80dc`, entry `1402a037`. ROM
  `r_lld_adv_adv_data_set` uses logical `1400` and 14-byte descriptors, updates
  the high byte at +2 with advertising-data length plus six, and writes the
  buffer offset at +4 (`40015858/40015864`). First descriptor begins
  `140e 2120 2c00 0000 0000 0d00 0000` as halfwords. Logical `2c00` maps via
  `60031224 = 2c0271a0` to `3fc9c680`. These are guest-prepared memory bytes,
  not a transmitted packet. Next work is resolving descriptor linkage and
  TX completion, then extracting channel/timestamp/PDU from an executed event.

Validate reset delay/clock epoch, latch edge, alarm commit/late behavior, FIFO
mask/pop/reset interaction and descriptor ownership on a real C3 before claiming
hardware accuracy. The phase-1 model intentionally leaves the kick stored and
never invents TX success or an advertising frame.

## Evidence and verification

`checks.json` records requested checks. `timing.json` records seven alternating
pairs per hello demo after warmup, against upstream main, with equal instruction
counts and console hashes. `timing-pilot.json` retains the preliminary run that
overlapped short diagnostic runs. No golden regeneration or JIT changes.

Full sanitized captures and the unchanged firmware remain local in the notes
artifact directories, not public downloads. Replay records original and
sanitized SHA-256 values; it replaces build/worktree/home path labels only.
Values, ordering and instruction counts are unchanged. Committed files include
no local absolute path or firmware binary. Full compiler/test logs remain in
notes; curated results here are sufficient to reproduce checks. No GitHub
write, upload, comment, PR or push was performed.

## Disabled-mode CPU comparison

Seven alternating measured main/branch pairs after one warmup per arm, 30 modeled
seconds per hello demo. Both built with `cargo +1.99.0 build --release --bins`.
No concurrent build or diagnostic run from this task during the final series;
ambient host load was uncontrolled. Main was an archive of upstream `29dd4623`.

| Demo | Main median user seconds | Phase 1 median user seconds | Change |
| --- | --- | --- | --- |
| hello | 0.287466 | 0.289671 | +0.77% |
| c3-hello | 2.660345 | 2.663097 | +0.10% |
| c6-hello | 3.457660 | 3.496655 | +1.13% |

All instruction counts and console SHA-256 values match. C3's +0.10% is within
the variation of individual pairs; this is no evidence of a measurable C3
regression, not a proof of mathematically zero overhead. The S3/C6 CPU code was
unchanged. Their small differences and the pilot's opposite C3 sign reinforce
the measurement limit. No performance improvement is claimed.

```sh
python3 docs/evidence/ble-full-c3-phase1-2026-10-05/bench.py \
  "$MAIN_BIN_DIR" target/release web/wasm/fw > "$OUTPUT/timing.json"
```
