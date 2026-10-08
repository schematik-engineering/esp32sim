# EX228: classic ESP32 shared VHCI adapter

Base: `esp32-classic-6-analog`, `19840b55`. Builds on EX227 and the shared
HCI controller/VHCI lifecycle merged in #172 (EX200). Adds classic LX6
windowed-ABI calling glue, DRAM bounds, ELF-sized installation and function
hooks. S3's existing trampoline bytes move into the shared VHCI module;
classic and S3 reference that single constant. No second controller exists.

BLE state is appended to SocBus. Disabled BLE uses the existing empty function
hook list and adds no tick work. The controller remains the existing boxed
HciController implementation. Reboot restores substituted flash by physical
offset, even if the MMU changed, and resets controller/session state.

## Inputs and verification

The inherited fixture and ROM hashes are in [EX223](../classic-core/README.md).
Adapter tests construct instruction bytes and symbols locally, execute real
LX6 register windows, and check task arguments, receive/send-ready callbacks,
yielding, DROM callback tables, DRAM boundaries, packet limits, ELF extents,
function-boundary dispatch and physical flash restoration.

Moved trampoline: 150 bytes, SHA-256 `a1503a0de37163ce1ec858c6c32f40f9f183b5ccae13ffea97de7286a0815db4`.
The byte sequence is unchanged from the parent branch's S3 adapter.

Run the full EX223 Rust 1.99.0 check set on this branch, including both Clippy
targets, empty-HOME workspace runs, all eight WASM scenarios and evidence
privacy. Existing goldens are unchanged. JIT implementation is unchanged.

[Mutation table](mutations.json) records nine killed replacements. For each,
apply the source replacement and run `cargo +1.99.0 test -p CRATE --lib TEST`,
then restore it. Callback entry checks, instruction/cycle accounting, RAM
bounds, hook registration, ELF size and reset restoration are exercised.

Results: both Clippy targets pass with warnings denied; 734 CI-mode
workspace tests and 712 plain tests (36 ignored) pass with empty HOME.
All eight WASM scenarios and evidence privacy pass; goldens unchanged.

## CPU comparison

PENDING

## Limits and overlap

The tests establish the adapter contract, not an unchanged classic Bluedroid
firmware boot or RF behavior. The shared controller's one-link, MTU-23 and
no-pairing limits remain. No private firmware or hardware capture is retained.
Open #196 changes C3 controller-driven BLE; classic uses the already merged
VHCI/controller path and does not duplicate that implementation.

All four chips now share flash restoration during reboot. VHCI chips use
`Ble::restore_flash`; C6 has its own NimBLE type and calls the same helper.
This changes no
per-tick path. Classic retains the physical-offset restoration regression;
shared packet-cap and trampoline-extent tests remain in S3 instead of being
copied into classic. The extent mutation runs against that S3 test.
