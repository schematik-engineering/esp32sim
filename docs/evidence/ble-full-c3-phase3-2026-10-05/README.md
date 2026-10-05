# EX210: C3 full BLE, phase 3 — active scanning

Base: `5dec562ad59d398b27fdd1ce3d85aa8e7efc6755`, the published phase-2
branch tip. Implementation: the commit containing this receipt. Inputs remain the
unchanged Arduino 3.3.11 / IDF 5.5.5 C3 Server and rev3 ROM; hashes are in
`result.json`. No hardware oracle, guest patch, HCI substitution or register poke
is used by the active-scanner implementation.

## Acceptance boundary

**Step 1 passes:** 27 advertising events, 81 SCAN_REQ and 81 SCAN_RSP packets
in two modeled seconds, both native and production WASM. Every response contains
the configured complete name **BLE Server Example**. Native AdvA is the
simulator default `60:55:f9:00:11:24`; WASM uses `3c:84:27:b6:a7:1e`.
Both request and response start exactly 150 µs after the preceding packet ends,
using 1M PHY airtime including preamble, access address and CRC. Advertising
starts remain 60–70 ms apart. No guest assertions or exceptions occur.

The controller receives SCAN_REQ in guest-allocated exchange-memory buffers.
The original `r_lld_adv_pkt_rx` checks each descriptor and calls
`r_lld_rxdesc_free`; the guest replenishes the ring. More than eight complete
ring wraps occur. SCAN_RSP bytes come from the guest's TX descriptor chain,
not a host-side copy of the sketch name.

CLI: `--ble full --ble-scan --ble-observe`. WASM:
`esp32sim_ble_full(emu)`, then `esp32sim_ble_scan(emu, 1)`; poll the existing
observation exports. Disabling scanning stops new requests; an exchange already
started completes. Reset cancels pending exchanges and preserves scanner selection
across a machine reboot.

**Steps 2–3 do not pass.** The separate CONNECT_IND probe below reaches a connection
event but cannot execute it. Step 4 has scan controls only; connect/read commands
and connection-state observations are absent. Step 5 covers scanning, descriptor
ownership, timing and off-mode checks, not the unimplemented connection/GATT path.

## Inferred receive contract

Every hardware field in this section is **inferred**, not a public-header definition
or a silicon-validated value. `rx-fields.txt` preserves the relevant instructions.

| Field | Model / guest evidence |
| --- | --- |
| LC `+0x24` | RX ring head, initialized to exchange offset `0x1000` by app `r_lld_core_init`, `0x4203cd80`. Model advances it through descriptor links; hardware readback semantics remain unverified. |
| RX stride 20, ten slots | Initialization at `0x4203cc82..b0`; ROM uses consumer index `p_lld_env+216` and stride 20. |
| RX `+0[14:0]` | Next logical descriptor; written at `0x4203ccb0`. |
| RX `+0[15]` | Set on completed reception. `r_lld_rxdesc_check`, `0x40020288..8e`, requires it set. Free/replenish code clears it only after assigning a buffer. A guest-owned descriptor is never overwritten. |
| RX `+2[15]` | Released/invalid marker, set by initialization and free; valid reception clears it. `r_lld_rxdesc_check`, `0x400203c4..ca`. |
| RX `+2` error mask `0x403d` | Zero for the scanner's error-free packets. `r_lld_adv_pkt_rx`, `0x400165cc..d8`, rejects nonzero masked bits. Individual CRC/error bit meanings remain unresolved; no CRC-error injection or claim of CRC validation is made. |
| RX `+4` | LL header including type/length/address type; `r_lld_adv_pkt_rx` selects SCAN_REQ=3 and CONNECT_IND=5. |
| RX `+6[7:0]`, `[13:8]` | Signed RSSI and channel, read by `r_lld_con_rx_channel_assess`, `0x40019dbc..dc8`; `r_rf_rssi_convert`, `0x4002e026`, sign-extends RSSI. Scanner uses fixed −40 dBm, a model input rather than a measurement. |
| RX `+8/+10`, `+12[9:0]` | Half-slot timestamp and `624-fine`; `r_lld_adv_pkt_rx_connect_post`, `0x40015db2..e48`. Timestamp placement at receive completion is provisional. Scan handling does not read it; connection synchronization requires resolving its relation to the access-address sync point. |
| RX `+12[15:11]` | Activity ID, checked by `r_lld_rxdesc_check`, `0x400203aa..b0`; comes from CS `+2[4:0]`, initialized by `r_lld_core_init`, `0x4203cdb6..c6`. |
| RX `+14` | Zero: no resolving-list entry. `r_lld_adv_pkt_rx_send_scan_req_evt`, `0x40016470`. Privacy resolution is absent. |
| RX `+18` | Guest-allocated payload buffer pointer; `r_lld_adv_pkt_rx_send_scan_req_evt`, `0x4001644c..5e`, copies 12 bytes. Payload is ScanA then AdvA, without LL header. |
| END bit 5 / ET state 3 | Existing completion contract. `r_lld_adv_frm_isr` drains RX through `r_lld_adv_pkt_rx` before scheduling the next advertisement. |

Correction to the phase-2 negative-result label: interrupt **bit 2 is RX notification**,
not TX. The live indirect table points `r_ip_funcs_p+0x6cc` to veneer
`0x40001544`, which jumps to `r_sch_prog_rx_isr` at `0x40030824`.
It invokes the frame callback with `a2=2`; legacy advertising returns without work.
The original negative outcome (bit 2 alone does not reschedule) remains valid.
This implementation uses END to drain advertising RX, without adding an ineffective
RX notification or claiming its ordering has been hardware validated.

Model limits: one built-in scanner, public simulated ScanA `02:00:00:00:00:01`,
no loss/collision/CRC corruption, no arbitrary injected packets, no whitelist or
resolving-list filtering, one request per enabled advertising channel. The scanner
can only target the just-emitted scannable PDU, on that channel at T_IFS.
A configured SCAN_RSP is required. The model still uses a fixed silent 300 µs
window without scanning and a 150 µs inter-channel gap after a scan response.
These timings and automatic-response eligibility need a hardware check.

## Connection blocker: genuine negative

A separate build with `connect-probe.patch` replaces the single scanner request
with CONNECT_IND, copies its 34-byte payload through the same RX ring, and ends
advertising. This is an isolated diagnostic, not a supported central or part of
step-1 acceptance. It negotiates CSA#1 by leaving CONNECT_IND ChSel clear;
ADV_IND's ChSel=1 advertises capability, not a requirement to use CSA#2.

The probe supplies access address `0x9a328370`, CRC init `0x123456`, 30 ms
interval, zero latency, 2 s supervision timeout, all 37 data channels and hop 5.
The guest reaches `r_lld_adv_pkt_rx_connect_ind_hack`,
`r_lld_adv_pkt_rx_connect_post`, `r_lld_con_start(activity=1)`, and
`r_lld_con_evt_start_cbk`. The first connection event then hits the model's
**unsupported control structure format** check. CS logical `0x045a` starts
with halfword `0x0603` (format 3); TX pointer is `0x147e`.
The guest copied the supplied access address and CRC seed into CS `+12/+16`.
The rejected event invokes `r_lld_con_frm_isr(a2=1)`, an abort, not a completed
central/peripheral exchange. See `connect-negative.json`.

This establishes the next boundary without claiming a functioning connection.
Required work before step 2 can pass:

- Resolve RX timestamp placement relative to the sync point. The connection-post
  reader subtracts twice `lld_exp_sync_pos_tab[RX+6 >> 14]` from `624-fine`;
  using receive-completion time without that derivation shifts the anchor.
- Execute CS format 3 at its anchor, with the programmed channel map/hop and
  selected CSA. Decode connection-specific TX descriptors and CS completion fields.
- Implement central/peripheral T_IFS, SN/NESN, retransmission and empty PDU flow;
  `r_lld_con_tx` reads TX ownership bit 15 at `0x4001a410..16`, while
  `r_lld_con_rx` branches on RX status at `0x4001a2d8..31c`.
- Prove supervision timeout and feature/version exchange before attempting ATT.
  No GATT read or connection acceptance is claimed by this probe.

To reproduce the negative, apply the patch with `git apply --unidiff-zero` in a disposable checkout of this
receipt's commit, build `esp32sim-c3`, and run the same Server command below with
`--max-seconds 0.265`, `--trace-fn r_lld_adv_pkt_rx_connect`,
`--trace-fn r_lld_con_start`, `--trace-fn r_lld_con_evt`,
`--trace-fn r_lld_con_frm_isr`, and `--peek 0x3fca7952,23`.
The peek address is specific to the hashed specimen, not part of the model.

## Reproduction and verification

Set `BUILD` to the unchanged Server build directory, `ROM` to the rev3 ROM
ELF, and `OUT` to a caller-owned output directory outside Git.

```sh
mkdir -p .ble3
export TMPDIR="$PWD/.ble3"
cargo +1.99.0 build --release --bins
python3 docs/evidence/ble-full-c3-phase3-2026-10-05/replay.py \
  --emulator target/release/esp32sim-c3 --build "$BUILD" --rom "$ROM" --output "$OUT"
ESP32SIM_BLE_SERVER_DIR="$BUILD" ESP32SIM_ROM_DIR="$(dirname "$ROM")" \
  cargo +1.99.0 test --release -p esp32sim --test ble external_full_ble_server -- --ignored
cargo +1.99.0 clippy --workspace --all-targets -- -D warnings
cargo +1.99.0 clippy --release --target wasm32-unknown-unknown -p esp32sim-wasm --features jit-tests -- -D warnings
ESP32SIM_ROM_DIR="$PWD/web/wasm/fw" cargo +1.99.0 test --release --workspace -- --include-ignored --skip external_
cargo +1.99.0 test --release --workspace
RUSTUP_TOOLCHAIN=1.99.0 tools/wasm-build.sh
node tools/wasm-test.mjs hello c3-hello c6-hello c6-energy-scan c6-contiki c6-contiki-net c6-rpl-net panel
node docs/evidence/ble-full-c3-phase3-2026-10-05/external_wasm.mjs \
  web/wasm/esp32sim.wasm "$BUILD" "$ROM"
node tools/check-evidence-privacy.mjs
```

The direct Server command is:

```sh
target/release/esp32sim-c3 --boot rom --rom "$ROM" --flash-mb 4 \
  --bootloader "$BUILD/Server.ino.bootloader.bin" --ptable "$BUILD/Server.ino.partitions.bin" \
  --app "$BUILD/Server.ino.bin" --elf "$BUILD/Server.ino.elf" \
  --ble full --ble-scan --ble-observe --max-seconds 2 --no-dump
```

All required checks pass; `checks.json` records commands and test totals.
JIT code is unchanged. Goldens were not regenerated. The external test needs
both the named build directory and ROM input and is ignored in ordinary/CI runs.

Native/WASM captures, final check logs and the connection negative are retained
locally in the caller-owned notes directory under `ble3-artifacts`; they are
not publicly hosted. Compact summaries identify capture hashes and byte sizes.
Absolute paths are replaced with `<BUILD>`, `<ROM>`, `<WORKTREE>` or `<HOME>`.
No numerical sample, packet byte, address within guest memory or artifact hash
is redacted. Firmware and complete event streams are not redistributed.


## Off-mode CPU comparison

Upstream main `29dd4623307853d8c036b65c5bb3249fa131758c`, verified against the
remote main ref, versus the implementation in this receipt. Both use Rust 1.99.0
release bins, 30 modeled seconds per demo, one warmup followed by seven alternating
main/candidate pairs. Samples are child **user** seconds. Runs are sequential;
no build, test or probe runs overlap measurement. Background load is not excluded;
aggregate load is retained in `cpu.json`.

| Round | S3 main | S3 candidate | C3 main | C3 candidate | C6 main | C6 candidate |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 0.285304 | 0.298334 | 2.684325 | 2.661235 | 3.453796 | 3.427683 |
| 2 | 0.299888 | 0.297612 | 2.639282 | 2.682975 | 3.459173 | 3.412025 |
| 3 | 0.284526 | 0.292572 | 2.658562 | 2.636820 | 3.421718 | 3.398376 |
| 4 | 0.293611 | 0.295825 | 2.638297 | 2.625503 | 3.407789 | 3.312174 |
| 5 | 0.293513 | 0.285135 | 2.636351 | 2.625964 | 3.299790 | 3.413275 |
| 6 | 0.289164 | 0.295827 | 2.618104 | 2.625227 | 3.342588 | 3.296315 |
| 7 | 0.291306 | 0.284570 | 2.595542 | 2.639620 | 3.311165 | 3.266812 |
| Median | 0.291306 | 0.295825 | 2.638297 | 2.636820 | 3.407789 | 3.398376 |

C3 median changes **−0.056%**, within the required ±1%. S3 changes +1.55%
(4.519 ms), C6 −0.28%; no speedup is claimed, and small differences cannot be
distinguished from run variation here. S3 core code is untouched.
Every arm and round has identical console hashes and instruction counts:
18,788,848 for S3; 4,800,000,000 for C3 and C6. Goldens remain bit-identical.

```sh
mkdir -p .ble3/main
git archive 29dd4623307853d8c036b65c5bb3249fa131758c | tar -x -C .ble3/main
cargo +1.99.0 build --release --bins --manifest-path .ble3/main/Cargo.toml
cargo +1.99.0 build --release --bins
python3 docs/evidence/ble-full-c3-phase1-2026-10-05/bench.py   .ble3/main/target/release target/release web/wasm/fw > "$OUT/cpu.json"
```
