# EX211: runtime I2C addresses, aliases, and general calls

The initial-revision sections below preserve the `996fddd` results and limits.
The PCA9685 follow-up at the end supersedes first-match selection for nonzero
addresses. The original `results.json` remains unchanged and identifies the
earlier artifacts.

The shared controller updates each selected device's stored address after a data
write. General-call writes select every matching device on the active pin pair,
with an ACK if any selected device ACKs. Existing trait implementations compile
unchanged. Ordinary transactions retain first-match selection.

Base: `8cc5d23` on `schematik-integration`. Implementation: `996fdddab6ac2dd4334c53916de3cb409d419e90`.
Behavior reference: retired fork
`221080f`, specifically `esp-periph/src/i2c.rs`. The fork's `address`,
`matches_address`, and `start_address` hooks informed this change. Its scheduled
byte execution and general address-collision handling were not ported.
The source and input identities are in [results.json](results.json).

EX205 and EX206 established C3 and C6 pin-aware I2C. EX209 established host detach
and replacement. EX211 adds a different correctness contract: a guest write can
change a device address, and address zero can select multiple devices. It does
not measure speed or change the timing model.

## API and checks

- `address(configured)` defaults to the stored attachment address. Devices can
  return their current 7-bit address after a write. Subsequent lookup, `has_device`,
  `detach`, and same-pin replacement use the updated stored address.
- `matches_address(configured, address, read)` defaults to equality with
  `address(configured)`. A general-call device also accepts address zero for writes.
- `start_address(address, read)` defaults to `start(read)`. A general-call device
  uses the actual address to distinguish reset data from its ordinary protocol.
- General-call selection retains all devices that ACK the address. Every selected
  device receives each byte, even if an earlier device ACKs or NACKs. END retains
  selection. STOP notifies each selected device. Detach adjusts all selected
  indices, and route changes and clear remove selection.

The six focused checks cover the shared controller and MMIO transactions through
S3, C3, C6, and classic ESP32. Shared checks include same-address devices on
separate pins, replacement at the changed address, continuation after an address
change, mixed data ACKs, rejected starts, absent recipients, read rejection,
STOP callbacks, END continuation, detach, and route invalidation.

The three external tests boot the same sketch using unchanged Arduino-ESP32
3.3.8, IDF libraries 5.5.4, and the real mask ROM on S3, C3, and C6. The test model
implements VL53L1X-style 16-bit registers: address register `0x0001` and identity
register `0x010f`. Two reset-capable devices at `0x58` and `0x59` independently
record the general-call byte `0x06`. All three devices use SDA8 and SCL9.

All three firmware runs produce exactly these test lines:

```text
I2C before 234
I2C change 0
I2C old 2
I2C after 234
I2C reset 0
I2C DONE
```

Both reset counters equal one, and every run has zero reboots. C3 and C6 each
execute 4,000,000 instructions and cycles. S3 executes 4,352,323 instructions
and 4,000,000 cycles across its cores. These are bounded functional runs, not
hardware timing or performance measurements.

## Reproduction

Run from the repository root. Rust checks use 1.99.0 without changing the default
toolchain. The PlatformIO configuration pins pioarduino `55.03.38-1`.

```sh
pio run -d docs/evidence/i2c-address-2026-10-02
tools/fetch-demo-assets.sh --no-linux
ESP32SIM_ROM_DIR="$PWD/web/wasm/fw" \
I2C_FIRMWARE_DIR="$PWD/docs/evidence/i2c-address-2026-10-02/.pio/build" \
  cargo +1.99.0 test --release -p esp32sim --test i2c_address -- --include-ignored --nocapture
cargo +1.99.0 clippy --workspace --all-targets -- -D warnings
cargo +1.99.0 clippy --release --target wasm32-unknown-unknown -p esp32sim-wasm --features jit-tests -- -D warnings
ESP32SIM_ROM_DIR="$PWD/web/wasm/fw" cargo +1.99.0 test --release --workspace -- --include-ignored --skip external_
RUSTUP_TOOLCHAIN=1.99.0 tools/wasm-build.sh
node tools/wasm-test.mjs hello c3-hello c6-hello c6-energy-scan c6-contiki c6-contiki-net c6-rpl-net panel
node tools/check-evidence-privacy.mjs
```

## Validation results

| Check | Result |
| --- | --- |
| Focused controller and chip checks | 6 passed |
| External Arduino checks | 3 passed |
| Native strict Clippy, all workspace targets | Passed |
| WASM strict Clippy, release with jit-tests | Passed |
| Release workspace with ignored tests included | 613 passed, 0 failed, 24 external tests filtered |
| WASM build and all eight requested demos | Passed, including shared-memory JIT handoff |
| Evidence privacy pattern check and manual review | Passed |

## Limits and retained failures

The initial implementation did not compile because an edit left an extra closing
brace. The initial test did not import the `Bus` trait for concrete classic-ESP32
MMIO calls. Both were corrected before the first passing focused run. Rustfmt was
not installed in toolchain 1.99.0, so stable rustfmt formatted only the new test
file. All compilation, Clippy, and Rust execution use 1.99.0.

Address changes are sampled after data writes. Changes deferred to STOP or caused
by out-of-band device mutation are outside this contract. Colliding nonzero
addresses retain first-match behavior. The controller still executes command
lists atomically, without electrical arbitration or byte timing. Classic ESP32
has register-level coverage here, not an Arduino firmware run. These tests do
not constitute acceptance of the private add-on or complete sensor models.

Evidence contains only the sketch, platform configuration, source and input
hashes, numeric samples, check summaries, and failure descriptions. Raw compiler
and test logs stay outside Git under `/tmp/gap-i2c-*.log`; build outputs are ignored.
Personal paths and unrelated host details are omitted from the curated receipt.
This does not affect protocol outputs or work counts. No private add-on sources
or captures are included, and no remote publication is performed.


## PCA9685 alias follow-up

Base: `f3a2ec3965806d39ec2ddc5fc44aae667848c1cd`.
Implementation: `bf5d672f7b0daf48a9a349a741120ab687f4ff78`. This extends EX211's
correctness contract to nonzero group addresses and per-transaction refusal.
The fork `221080f` controller and PCA9685 address methods were inspected before
implementation. Existing `matches_address` and `start_address` hooks already
support programmable aliases; no new trait methods are needed.

The controller now visits every match at any address and retains every device
that ACKs `start_address`. Writes reach all selected devices, and reads combine
their bytes with bitwise AND, matching the fork's open-drain data resolution.
This supersedes the initial implementation's first-match collision behavior.
Attachment identity, same-address/same-pin replacement, runtime address changes,
and pin-aware filtering are unchanged. Aliases do not create extra attachments.

The focused regression `shared_programmable_aliases_and_all_call` failed before
the fix: the second device's register was `0`, expected `85` after a `0x55`
all-call write. It now passes. The test programs and enables all three subaddresses
and the all-call address, checks disable/re-enable, observes the actual address
in `start_address`, excludes devices on another pin pair, rejects general-call
reads, and allows a later matching device to ACK when the first refuses.
A repeated-start read combines `0xf0` and `0x5a` into `0x50`.

The test-only board supplies two register-level PCA9685 models at `0x40` and
`0x41` on SDA8/SCL9, plus an isolated `0x40` model on SDA6/SCL7. The same
[pca9685 sketch](pca9685/src/main.cpp) runs unchanged on S3, C3, and C6 using
unmodified Arduino-ESP32 3.3.8 and Adafruit PWM Servo Driver 3.0.3 at commit
`a98850b815bf9696c8ffdc2a0f89d657c52dd44b`. Adafruit BusIO is pinned to 1.17.4.
The group driver's `begin()` resets MODE1, so the sketch initializes that handle
before explicitly enabling all-call on the individual devices.

The library writes and reads channel-zero PWM registers through both main
addresses and the all-call address. Every chip prints:

```text
PCA begin 1 1 1
PCA main 300 450 and 256
PCA group 600 600
PCA disabled 600 700
PCA reset 0
PCA DONE
```

Both models observe exactly one general-call reset and return MODE1 `0x11`.
The isolated model observes no address phases. Every run has zero reboots.
Final pinned-build work counts:

| Chip | Cycles | Instructions |
| --- | ---: | ---: |
| S3 | 18,000,063 | 5,012,461 |
| C3 | 12,000,058 | 12,000,058 |
| C6 | 13,000,000 | 13,000,000 |

The first successful build resolved BusIO 1.17.4 transitively. Its S3 run retired
5,012,592 instructions at the same cycle count. Rebuilding with the explicit BusIO pin changed the S3 instruction count;
all protocol outputs stayed equal.
The final artifact hashes and samples are in [pca9685/results.json](pca9685/results.json).
These single functional runs do not establish speed or hardware timing.

Reproduction from the repository root:

```sh
pio run -d docs/evidence/i2c-address-2026-10-02/pca9685
tools/fetch-demo-assets.sh --no-linux
ESP32SIM_ROM_DIR="$PWD/web/wasm/fw" \
I2C_FIRMWARE_DIR="$PWD/docs/evidence/i2c-address-2026-10-02/.pio/build" \
PCA_FIRMWARE_DIR="$PWD/docs/evidence/i2c-address-2026-10-02/pca9685/.pio/build" \
  cargo +1.99.0 test --release -p esp32sim --test i2c_address -- --include-ignored --nocapture
```

The original sketch build command above supplies `I2C_FIRMWARE_DIR`. Both sets
of external tests run together: 13 passed, including the seven focused checks
and six firmware runs. The full CI commands above were rerun with Rust 1.99.0:
614 release tests passed, 27 external tests were filtered, both strict Clippy
commands passed, and the WASM build and all eight demos passed. The final privacy
check and manual review cover the added evidence.

This fixture implements the register and address behavior needed to prove bus
transport. It does not model PCA9685 PWM waveforms, oscillator settling, OE, or
full device semantics. No private add-on source was copied, and no private
add-on acceptance or physical timing result is claimed. The earlier limitations
on STOP-deferred address changes remain. Raw logs stay outside Git under
`/tmp/gap-i2c-alias-*.log` and `/tmp/gap-i2c-pca-build*.log`; compiled firmware
and dependency checkouts remain ignored. The curated receipt contains only
relevant hashes, configuration, numeric samples, and test outcomes.
