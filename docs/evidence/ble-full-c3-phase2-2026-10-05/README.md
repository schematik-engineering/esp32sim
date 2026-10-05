# EX210: C3 full BLE, phase 2 / milestone A

The unchanged Arduino 3.3.11 / ESP-IDF 5.5.5 Server executes its original controller,
programs exchange memory, emits legacy advertising packets, handles END interrupts,
and schedules subsequent events. There are no controller hooks, register pokes or
host-generated advertising intervals. This is a local implementation, not a PR or
an upstream agreement; discussion is mikroverk/esp32sim#189.

Base: `3827ad3f5215e61a1e1c43b3aa505f054bfcf41a` (phase-1 implementation
`609e7e0c`). CPU comparison: upstream main `29dd4623307853d8c036b65c5bb3249fa131758c`.
Implementation: the commit containing this receipt. Input hashes are in `result.json`;
source and environment hashes are in `checks.json`. No hardware oracle is available.

## Result and acceptance boundary

Native: 27 events / 81 ADV_IND PDUs in two modeled seconds, all three channels
37/38/39, default modeled AdvA `60:55:f9:00:11:24`. First event starts at
258437.5 µs. Event starts are 60625–70000 µs apart, median 67187.5 µs. The guest's
`lld_adv_env[0]+100` contains `0x60`: 96 × 625 µs = 60 ms. ROM
`r_lld_adv_frm_isr` at `0x4001721c..54` adds its own `random % 17` × 625 µs delay.
The external test pins the 60–70 ms range and at least five complete events.
Twenty-seven END callbacks also exercise event-table wrap past its 16 entries.

The observer decodes service UUID `4fafc201-1fb5-459e-8fcc-c5c9c331914b`, flags
`0x06`, and preferred connection interval units `6:18` from the emitted PDU.
The unchanged sketch enables scan responses: `BLE Server Example` exists only in
its SCAN_RSP descriptor. It is exposed as **configured** `[ble-config]` data;
no SCAN_RSP is emitted without a received SCAN_REQ. Thus repeated passive advertising
is achieved, but the literal requirement to observe the name in an emitted packet
requires milestone-B RX. No active scanner or fabricated name was added.

The real production WASM module also emits 27 events / 81 PDUs. It uses the
WASM machine's different default modeled AdvA `3c:84:27:b6:a7:1e`; both addresses
come from simulator defaults, not hardware identifiers. `wasm-result.json` records
its timing and module hash. The page can poll `esp32sim_ble_take` / `esp32sim_ble_ptr`
after enabling `esp32sim_ble_full` before boot. The CLI uses `--ble full --ble-observe`.

## Inferred fields and completion contract

**Every hardware field below is inferred from this ROM/application, not a public
register header and not silicon validated.** `rom-fields.txt` and
`app-completion.txt` retain the relevant disassembly. Source 8 alone has the public
IDF v5.5.5 `components/soc/esp32c3/include/soc/interrupts.h`
`ETS_RWBLE_INTR_SOURCE` definition, already cited by phase 1.

| Field | Meaning used | Guest writer / reader evidence |
| --- | --- | --- |
| LC `+0x204..0x2c0`, then `+0x2e0..0x2fc` | 56 exchange-memory mappings; high 14 bits × 4 = logical start; low 18 bits × 4 OR `0x3fc00000` = SRAM base | `r_emi_get_mem_addr_by_offset`, `0x400069e2..fe`, `0x40006a68..90`; final eight have a seven-register gap |
| LC `+0x100` bit 31, low nibble | Kick event index 0–15 | `r_sch_prog_ble_push_hack`, `0x4038760e..38` |
| Event stride 16, `+2/+4` and `+6` | 28-bit half-slot timestamp and `624 - fine_half_us` | `r_sch_prog_push`, writes `0x40030f1a`, `0x40030f3c`, `0x40030f9e` |
| Event `+8` | CS pointer in halfwords | `r_sch_prog_ble_push_hack`, `0x40387436..50` |
| CS stride 90, `+0 & 31` | Legacy advertising format 4 | `r_lld_adv_start_set_cs`, `0x400181e4..e8`; `r_lld_adv_evt_start_cbk` reads at `0x4001667a..7e` |
| CS `+6..11` | AdvA bytes, least significant first | `r_lld_adv_start`, `0x4001885c..ca` |
| CS `+28` | First TX descriptor logical byte offset | `r_lld_adv_start`, `0x40018684..b8` |
| CS `+38[7:5]` | Enabled advertising channels 37, 38, 39 | `r_lld_adv_start`, `0x400188ce..0x40018942` |
| TX stride 14, `+0[14:0]` | Next descriptor logical offset; this specimen forms ADV_IND → SCAN_RSP → ADV_IND | `r_lld_adv_start_init_evt_param`, `0x40017b9e`, `0x40017c16..50` |
| TX `+2` | LL advertising header, upper byte length includes AdvA | `r_lld_adv_adv_data_set`, `0x4001581a..58`; `r_lld_adv_start_init_evt_param`, `0x40017922` |
| TX `+4` | AD buffer logical offset; specimen ADV uses mapping LC `+0x224`, SCAN_RSP uses `+0x220` | `r_lld_adv_adv_data_set`, `0x40015864`; `r_lld_adv_scan_rsp_data_set` |
| Event `+0[5:3]` | 3 = completed, 4 = aborted callback | `r_sch_prog_end_isr_handler` reads `0x40387080`, accepts states 3–5 and calls callback with `a2=(state==4)` at `0x403870f0` |
| LC raw/FIFO status bit 5 | END interrupt after the last channel's airtime/window | `r_rwble_isr_hack`, `0x40386b4a..68`; `r_ip_funcs_p+0x6c0` resolves to `0x4038734e`, `r_sch_prog_end_isr_hack` |
| LC `+0x2d8` count `[9:5]`, status `[30:10]`, pop bit 0 | Pending source snapshots, independent of W1C status at `+0x18` | `r_rwble_isr_hack`, `0x4038699e..bc` |

Successful completion changes only the event state and END source. The guest then
executes `r_sch_prog_end_isr_handler` → `r_lld_adv_frm_cbk` →
`r_lld_adv_frm_isr_eco` → `r_lld_adv_frm_isr` and re-arms its scheduler timer.
No additional CS/TX ownership/status bits proved necessary for this no-RX specimen;
unspecified fields remain stored rather than receiving invented success values.

Model choices, not hardware facts: one active event, FIFO kick order, at most 16
queued kicks; up to nine descriptors per activity; snapshot the CS and TX chain at
event start; ascending enabled channels; 1M PHY packet airtime followed by a fixed
300 µs silent receive window. No radio energy, whitening, CRC bytes or RF calibration
is simulated. Repeated pending interrupt sources coalesce; full hardware FIFO overflow
semantics are not implemented. The host observation queue retains 1024 entries and
reports dropped entries. Unsupported/malformed mapped descriptors report an error
and complete as aborted; an unmapped event entry reports an error without writing SRAM.

## Genuine negative results

| Contract tested | Result | Implication |
| --- | --- | --- |
| ET state 3 plus interrupt bit 2 alone | One event, three PDUs, two source-8 deliveries total; `r_lld_adv_frm_cbk(a2=2)` returns without rescheduling | Bit 2 is TX notification, not END. Resolve indirect callback tables, rather than infer interrupt names from their order. END is bit 5. |
| Decode the 56 mappings as one contiguous register array | Post-table control registers were mistaken for mappings; no advertising PDUs | The ROM's seven-register gap before mappings 48–55 matters. |

These are model-contract negatives, not benchmark samples or hardware observations.
Superseded full traces are not retained.

## Reproduction and checks

Set `BUILD` to the unchanged Server build directory, `ROM` to the C3 rev3 ROM ELF,
and `OUT` to an output directory outside Git. No input firmware is redistributed.

```sh
cargo +1.99.0 build --release --bins
python3 docs/evidence/ble-full-c3-phase2-2026-10-05/replay.py \
  --emulator target/release/esp32sim-c3 --build "$BUILD" --rom "$ROM" --output "$OUT"
ESP32SIM_BLE_SERVER_DIR="$BUILD" ESP32SIM_ROM_DIR="$(dirname "$ROM")" \
  cargo +1.99.0 test --release -p esp32sim --test ble \
  external_full_ble_server_advertises_repeatedly -- --ignored
RUSTUP_TOOLCHAIN=1.99.0 tools/wasm-build.sh
node docs/evidence/ble-full-c3-phase2-2026-10-05/external_wasm.mjs \
  web/wasm/esp32sim.wasm "$BUILD" "$ROM"
```

CPU samples use the unchanged phase-1 `bench.py` harness: main and candidate release
binaries, 30 modeled seconds per hello, one warmup then seven alternating main/candidate
pairs, median child **user** CPU time. Both arms use the same firmware and pinned Rust
1.99.0; instruction counts and console hashes must match. See `cpu.json` and the
report's per-round table. No speedup or hardware-time claim is made.

Final checks and environment are in `checks.json`. Goldens are not regenerated.
Raw final native output, check logs and CPU samples are retained locally under the
caller-owned notes directory (`ble2-artifacts`); `result.json` identifies raw capture
hashes and sanitized hashes. Captures are not publicly hosted. Paths are replaced by
`<BUILD>`, `<ROM>`, `<WORKTREE>` and `<HOME>`; numerical samples, function addresses and
artifact hashes are preserved. No process inventories, personal machine identifiers,
private-repository references or full event streams are committed.

## Milestone B and hardware validation

`rom-rx-map.txt` provides the next boundary:

- RX descriptors start at exchange offset `0x1000`, stride 20, ten slots in the
  `r_lld_rxdesc_check` path. Its consumer index is `p_lld_env+216`; bit 15 at
  descriptor `+0` gates ownership (`0x40020288..8e`). An alternate RX FIFO mode
  also exists and needs tracing before choosing the receive implementation.
- `r_lld_adv_pkt_rx` reads descriptor `+2` and rejects mask `0x403d`
  (`0x400165cc..d8`). Descriptor `+4 & 15` distinguishes CONNECT_IND (5) from
  SCAN_REQ (3). The exact error/status meanings remain inferred and unresolved.
- `r_lld_adv_pkt_rx_connect_ind` reads descriptor `+18` as an exchange-buffer
  pointer (`0x40016090..a2`) and copies 34 bytes. Add real receive-window eligibility,
  address/CRC checks, SCAN_REQ→SCAN_RSP turnaround, descriptor ownership and RX IRQs;
  do not manufacture a host connection directly.
- Follow CONNECT_IND through `r_lld_adv_pkt_rx_connect_post` and
  `r_lld_con_start`. Connection events need access address/CRC seed, channel map/hop,
  anchor/window/interval timing, SN/NESN/MD, acknowledgment/retransmission and timeout
  behavior. Trace `r_lld_con_evt_start_cbk`, `r_lld_con_rx` and
  `r_lld_con_frm_isr` before defining their status fields.

Validate on a real C3: mapping boundaries and flag bits; event states 3/4/5; END/TX/RX
interrupt ordering and FIFO coalescing; actual channel order, inter-channel receive
windows and completion timestamp; queued/overlapping events, cancellation and reset;
TX descriptor bit-15 ownership; advertised interval versus the controller field;
and radio timing with both SCAN_REQ and CONNECT_IND. Phase-1 reset/latch timing also
remains a hardware-validation question.

## Off-mode CPU comparison

Seven alternating main/candidate pairs after a warmup; child user seconds, 30 modeled
seconds per run. Both arms were built with `cargo +1.99.0 build --release --bins`.
Main is `29dd4623307853d8c036b65c5bb3249fa131758c`. Runs were sequential; no build or
test jobs ran during measurement. Background load was not excluded (aggregate load
is recorded in `cpu.json`). Differences are within ordinary run variation; no speedup claim.

| Round | S3 main | S3 candidate | C3 main | C3 candidate | C6 main | C6 candidate |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 0.297449 | 0.301558 | 2.643198 | 2.658557 | 3.539895 | 3.479821 |
| 2 | 0.303862 | 0.294304 | 2.691146 | 2.642432 | 3.470193 | 3.479706 |
| 3 | 0.296749 | 0.298474 | 2.644216 | 2.602735 | 3.483457 | 3.475256 |
| 4 | 0.310686 | 0.301346 | 2.654102 | 2.615430 | 3.529723 | 3.462079 |
| 5 | 0.299140 | 0.311824 | 2.688564 | 2.645853 | 3.476358 | 3.438422 |
| 6 | 0.298439 | 0.290501 | 2.694728 | 2.646676 | 3.476649 | 3.458809 |
| 7 | 0.293768 | 0.301730 | 2.629424 | 2.623429 | 3.480558 | 3.473722 |
| Median | 0.298439 | 0.301346 | 2.654102 | 2.642432 | 3.480558 | 3.473722 |

- hello: +0.97%; 18,788,848 instructions in every run, identical console SHA-256.
- c3-hello: -0.44%; 4,800,000,000 instructions in every run, identical console SHA-256.
- c6-hello: -0.20%; 4,800,000,000 instructions in every run, identical console SHA-256.

C3 satisfies the requested ±1% criterion. Full mode is off in all these runs.

Exact final check commands (run from the worktree; ROM assets already present):

```sh
mkdir -p .ble2
export TMPDIR="$PWD/.ble2"
cargo +1.99.0 clippy --workspace --all-targets -- -D warnings
cargo +1.99.0 clippy --release --target wasm32-unknown-unknown -p esp32sim-wasm --features jit-tests -- -D warnings
ESP32SIM_ROM_DIR="$PWD/web/wasm/fw" cargo +1.99.0 test --release --workspace -- --include-ignored --skip external_
cargo +1.99.0 test --release --workspace
RUSTUP_TOOLCHAIN=1.99.0 tools/wasm-build.sh
node tools/wasm-test.mjs hello c3-hello c6-hello c6-energy-scan c6-contiki c6-contiki-net c6-rpl-net panel
node tools/check-evidence-privacy.mjs
```

Baseline and CPU command (no other build/test jobs during sampling):

```sh
mkdir -p .ble2/main
git archive 29dd4623307853d8c036b65c5bb3249fa131758c | tar -x -C .ble2/main
cargo +1.99.0 build --release --bins --manifest-path .ble2/main/Cargo.toml
cargo +1.99.0 build --release --bins
python3 docs/evidence/ble-full-c3-phase1-2026-10-05/bench.py \
  .ble2/main/target/release target/release web/wasm/fw > "$OUT/cpu.json"
```
