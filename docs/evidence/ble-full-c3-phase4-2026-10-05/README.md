# EX210: C3 full BLE, phase 4 — connection events

Base: `0824de29971106410ccc3415fc19affcefc61a16`. Implementation: the commit
containing this receipt. Unchanged Arduino-ESP32 3.3.11 / IDF 5.5.5 Server,
C3 rev3 ROM; input hashes are in `result.json`. No hardware oracle, guest
patches, register pokes or HCI substitution.

## Step 2 acceptance

The virtual central sends CONNECT_IND after ADV_IND, negotiates CSA#1, and
exchanges empty data PDUs for more than two seconds. The guest's VERSION_IND
is transmitted from its TX descriptor and released only after an ACK.
The central stops after 2,500 ms from the first anchor. Subsequent receive
windows complete without packets; the guest's own supervision logic calls
`r_llc_disconnect_end(activity=1, reason=0x08)`. Advertising resumes.
The unchanged sketch calls `pServer->advertiseOnDisconnect(true)`.

The central chooses access address `0x9a328370`, CRC seed `0x123456`,
1M PHY, WinSize 1, WinOffset 6, interval 24 (30 ms), latency 0,
timeout 200 (2 seconds), all 37 channels and hop 5. CONNECT_IND ChSel is clear:
ADV_IND ChSel advertises capability and does not force CSA#2. These are virtual
central inputs, not discovered hardware constants.

CLI: `--ble full --ble-connect --ble-stop-after-ms 2500 --ble-observe`.
Omit the stop option to keep sending. Stop duration is measured from the first
anchor, rounded up to a connection event. One connection attempt is made.
Scan controls from step 1 remain available. No new WASM control is claimed here.

## Inferred hardware contract

**All fields below are inferred from the ROM/application, not public headers or
silicon validation.** The controller belongs to this IDF 5.5.5 specimen; no IDF
4.4 compatibility is claimed. `connection-fields.txt` preserves selected readers
and writers. Earlier receipts describe exchange mappings and descriptor layouts.

| Field / behavior | Reader or writer and model |
| --- | --- |
| RX timestamp +8/+10, +12[9:0] | `r_lld_adv_pkt_rx_connect_post`, 0x40015e24..eca, subtracts twice `lld_exp_sync_pos_tab[rate]` from 624-fine and borrows 625 half-microseconds. |
| Timestamp sync offset, rate RX+6[15:14]=0 | App `r_lld_core_init`, 0x4203cdd6..ee, sets table[0] to 40 + LC+0x90[14:8] microseconds. Publish packet-start plus this offset, not packet-end. The live 1M value is 44 µs. |
| First anchor | `r_lld_con_start`, 0x4001c08c..c100, adds CONNECT_IND duration, WinOffset and the 1.25-ms transmit-window delay; its listen window includes half WinSize. The central transmits at the window's start: CONNECT_IND start +352+8750 µs. |
| CS format 3 | Connection structure initialized by `r_lld_con_start`; reuse mapped 90-byte CS and 16-byte event entries. |
| CS +22[5:0], [12:8], [14] | Previous unmapped channel, hop, CSA selection. `r_lld_con_start`, 0x4001ba30..36, and `r_lld_con_evt_start_cbk`, 0x4001ae4e..aeaa. Add one hop modulo 37, then remap through enabled channels. CSA#2 is rejected. |
| CS +34..38 | Channel map, written by `r_lld_con_start`, 0x4001baf2..bb8c. Sparse-map remapping has a unit test; the live central uses all channels. |
| CS +26 | Receive-window width: 2-µs units, or 625-µs units with bit 15, from `r_lld_con_evt_start_cbk`, 0x4001aec0..af14 / 0x4001b172..18c. Reject anchors outside the programmed window; never move the central clock to hide a missed window. |
| CS +28 | Connection TX pointer, initialized at 0x4001bfd2..bff8. |
| TX +0 bit15/link, +2 header, +4 payload | `r_lld_con_tx`, 0x4001a410..16 / 0x4001a570..58e, and `r_lld_con_tx_prog`, 0x4001ac2e..66. Cache the pending PDU until peer NESN acknowledges it; then set completion ownership and advance CS+28 through the link. |
| RX status/header/buffer | Same 20-byte ring as phase 3, with data-channel numbers and the corrected sync timestamp. Error-free packets only. No fabricated CRC check. |
| IRQ RX bit2, TX bit6 | App `r_rwble_isr_hack`, 0x40386aee..b48, routes to `r_sch_prog_rx_isr` / `r_sch_prog_tx_isr`, then the connection callbacks. RX is published at packet completion; TX completion waits for ACK. |
| ET state 2, state 3 / END bit5 | Mark active before RX IRQ; finish after the exchange. Existing `r_sch_prog_end_isr_handler` completion contract. Missing central packets produce no RX IRQ. |

SN/NESN are independently tracked for central and peripheral. Duplicate sequence
numbers do not deliver payload twice; unacknowledged TX remains cached with the
same SN. Peripheral MD requests another exchange in the event. All packet
turnarounds use 150 µs and 1M airtime including preamble/access address/CRC.
The built-in central sends error-free packets; loss/retransmission is tested at
the sequence/descriptor layer, not with a live RF-loss scenario. Duplicate RX
timestamp refresh and hardware error/status bits remain unvalidated.

## Reproduce and checks

Build with `cargo +1.99.0 build --release --bins`, then:

```sh
python3 docs/evidence/ble-full-c3-phase4-2026-10-05/replay.py \
  --emulator target/release/esp32sim-c3 --build "$BUILD" \
  --rom "$ROM" --output "$OUTPUT"
ESP32SIM_BLE_SERVER_DIR="$BUILD" ESP32SIM_ROM_DIR="$ROM_DIR" \
  cargo +1.99.0 test --release -p esp32sim --test ble external_full_ble_server -- --ignored
```

Caller supplies the unchanged Server build and ROM directories. The external
test verifies anchor arithmetic, every channel, SN/NESN, T_IFS, more than two
seconds of traffic, reason 0x08 and advertising restart. Unit tests cover sync
timestamp wrap/borrowing, CSA#1 sparse maps, duplicate sequence detection and
descriptor ownership before/after ACK.

`checks.json` records the full required checks. JIT code is unchanged.
`cpu.json` records seven alternating pairs after warmup, using the phase-1
`bench.py` against upstream main `29dd4623307853d8c036b65c5bb3249fa131758c`.
Both arms use Rust 1.99.0 release binaries and identical firmware; no load
exclusion or speedup claim. Instruction counts, console hashes and goldens
must match. Raw run output is retained outside Git; the compact result records
hashes and relevant samples. Paths are replaced by caller-input labels; numeric
measurements and simulated peer addresses are retained.

## Negative results and limits

The guest resumes advertising after timeout but logs `NimBLE: ble_uuid_flat rc=3`
and its service UUID differs from the initial advertisement. This is a genuine
unresolved result, not evidence of a correct post-disconnect GATT database.
The model does not patch the sketch or attribute the cause to the library without
proof. The initial advertisement still carries the expected service UUID.

A zero WinOffset connection probe missed the first programmed guest window.
Moving the central anchor to that window hid the miss and introduced drift;
that approach is not used. The accepted central gives six 1.25-ms units of
WinOffset, then maintains its independent 30-ms schedule.

This step supports the selected unencrypted, 1M, zero-latency CSA#1 connection,
not arbitrary parameter updates, PHY changes, encryption or CSA#2. A channel
mismatch or missed receive window is an explicit model error. No access-address
or CRC corruption is injected. Hardware validation must confirm receive-window
units, sync placement, duplicate/status semantics, IRQ ordering and descriptor
completion side effects. Step 3 (LL procedures and ATT), the remaining step-4
CLI/WASM surface and step-5 GATT acceptance are not claimed by this commit.

Final CPU result: C3 median user 2.402440 → 2.407438 s (+0.208%); S3 +0.260%, C6 +0.309%. Work/output match in every sample. The earlier complete run, retained in `cpu-negative.json`, measured C3 +2.053% with 2.47–4.43 s samples and failed the limit. The final run has no concurrent build/test jobs; neither run excludes samples.
