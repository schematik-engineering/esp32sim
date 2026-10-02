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

## Bus-time follow-up

Revision `d6931c7ab7ea28f3c31c36128cd09111646671b0` extends accepted head
`a3f2a9ebabc4acfc0dbdf0c9d2e5a54be5e197b2`. This changes EX211's correctness
contract from immediate transfers to scheduled START, byte/ACK, repeated START,
and STOP phases. The earlier address, alias and general-call results above remain
historical results. Fork `221080f` supplied the byte-timing reference; installed
ESP-IDF 5.5.4 I2C register definitions supplied the classic filter correction.

The shared controller now uses the existing APB tick/deadline interface. It
consumes one byte after nine configured SCL clocks, including ACK. It raises
NACK, END_DETECT or TRANS_COMPLETE when that phase finishes, and exposes the busy
status while active. END preserves device selection for a later command list;
modern FSM reset cancels the pending transfer. No device trait signature changes.
The earlier matching, pin routing, broadcast, wired-AND reads and replacement
rules remain in the byte callbacks.

S3/C3 use CLK_CONF; C6 supplies its PCR I2C clock divider in the same layout.
The source is 40 MHz XTAL or the fork's nominal 17.5 MHz RC_FAST. Fractional
clock periods round up to an APB tick for each phase. Classic ESP32 uses its
80 MHz APB, wider timing fields and SCL filter correction. Each chip advances
its board's clock before ticking devices, so callbacks see elapsed emulated
time. This also adds the previously absent board clock advance on classic.

Four focused tests cover exact byte/FIFO/interrupt boundaries, NACK, END
continuation, reset cancellation, fractional and alternate-source clocks,
repeated START, classic timing and C6 PCR division. At 100 kHz, the explicit
register fixture takes 800 APB ticks per START/STOP and 7,200 per byte. Its
one-byte read completes at tick 16,000, with no earlier completion interrupt.
The new busy assertion failed on the instantaneous base (0 instead of 16).
Raw register tests now advance the bus before checking completion; the C6 GPIO
fixture checks timestamps relative to its stimulus, after its I2C setup.

### Strict SCD4x firmware

The new [sketch](timing/src/main.cpp) uses unchanged Arduino-ESP32 3.3.8,
Sensirion SCD4x 1.1.0 at `fd169d39a3dd342ba05feef92d69c36fb3a340ee`, and
Sensirion Core 0.7.3. Its workflow follows the fork fixture: stop, start, wait
five seconds, check readiness, then read CO2/temperature/humidity. The fork's
bounded retry loop is retained. The final checks require zero retries and zero
early reads. The test-only board model enforces a 500 ms stop delay, five-second
measurement readiness, a 1 ms response deadline, CRC8 and sample consumption
only after the complete measurement response.

At **10 kHz**, all three chips print:

```text
SCD stop 0
SCD start 0
SCD ready 0 1
SCD sample 0 800 25.00 50.00
TIMING DONE
```

Each run has zero reboots, exactly four sensor commands and two accepted read
transactions. On the accepted instantaneous base, the same final firmware and
model fail all three strict tests: S3 obtains values but needs one early-read
retry; C3/C6 return error 527 for the sample and reject eight early reads each.
The final source, firmware, dependency, ROM and emulator hashes, command cycles
and read deadlines are in [timing/results.json](timing/results.json).

The initial **100 kHz** one-shot sketch failed on all three chips: Arduino's
`delay(1)` can return before a full millisecond has elapsed, and the read address
then arrives before the model's response deadline. The fork-style retry loop
passed on S3, but C3/C6 repeatedly restarted the deadline by sending the command
again. Changing only the configured bus rate to 10 kHz supplies enough bus time;
it does not shorten the model's deadline or patch the driver. Thus this receipt
proves elapsed bus time with a strict sensor, not reliable 100 kHz operation for
an application relying on `delay(1)` as a minimum delay.

Earlier harness corrections are retained in the receipt: the heavy test first
counted a combined repeated-start transfer as two command lists; the sensor
initially rejected the write address while busy and consumed a sample before
its response was read. The corrected model follows the fork's command-level
busy check and final-byte consumption. Initial one-shot artifact hashes are
preserved; that exploratory source used a single status/read call without the
retry loops or the extra `delay(1)` after stop, before the explicit Core pin.

Reproduce from the repository root:

```sh
pio run -d docs/evidence/i2c-address-2026-10-02/timing
tools/fetch-demo-assets.sh --no-linux
ESP32SIM_ROM_DIR="$PWD/web/wasm/fw" \
I2C_TIMING_FIRMWARE_DIR="$PWD/docs/evidence/i2c-address-2026-10-02/timing/.pio/build" \
  cargo +1.99.0 test --release -p esp32sim --test i2c_timing -- --include-ignored --nocapture
```

This runs four focused checks, three sensor firmware checks and the heavy sketch:
**8 passed**. The earlier `i2c_address` command above also passes all **13** checks,
including six unchanged address/general-call/PCA9685 firmware runs.

### Workload cost and CI

The heavy S3 sketch performs 5,000 combined register reads at 100 kHz, checks
sum 825,000 and zero errors, and completes exactly 5,000 command lists. One
warm-up pair followed by five alternating serial pairs compares the accepted
instantaneous base with the timing implementation, using identical final test
source and firmware. Other build/test jobs had finished; ambient host load was
not controlled. Both builds use Rust 1.99.0 release on macOS 26.6.2 arm64.

| Measure | Instantaneous base | Timed controller |
| --- | ---: | ---: |
| Median wall seconds | 0.290505916 | 0.421094459 |
| Wall range, five samples | 0.285035500–0.302873542 | 0.412901708–0.432790500 |
| Emulated cycles | 47,000,000 | 518,000,127 |
| Retired instructions | 47,349,764 | 64,089,970 |
| Emulated seconds at 240 MHz | 0.195833333 | 2.158333863 |

The measured wall cost is **44.95%** for the fixed workload. It includes firmware
waiting/interrupt work and boot; it is not pure controller overhead. Run 0 and
all measured samples are retained. The stop marker is polled at one-million
instruction batches, identically in both builds.

The baseline is an archive inside this worktree's ignored `target`, not another
worktree. To build the same comparison harness:

```sh
mkdir -p target/i2c-timing-baseline-src
git archive a3f2a9ebabc4acfc0dbdf0c9d2e5a54be5e197b2 | tar -x -C target/i2c-timing-baseline-src
cp cli/tests/i2c_timing.rs target/i2c-timing-baseline-src/cli/tests/i2c_timing.rs
CARGO_TARGET_DIR="$PWD/target/i2c-timing-baseline-build" \
  cargo +1.99.0 test --manifest-path target/i2c-timing-baseline-src/Cargo.toml \
  --release -p esp32sim --test i2c_timing --no-run
```

Run each emitted test executable with `--ignored --exact external_i2c_heavy_s3
--nocapture`, using the same absolute ROM and firmware environment variables
above. Alternate baseline/timed and timed/baseline pairs. The test prints cycles,
instructions and an `Instant` wall duration from first ROM execution to the stop
marker. The sensor regression uses `--ignored external_scd4x --nocapture` on the
baseline executable and intentionally fails.

All required CI commands listed earlier were rerun using Rust 1.99.0 and the
absolute ROM directory: both strict Clippy checks pass; **618 release tests pass**,
with zero failures/ignored and 31 external tests filtered; WASM builds and all
eight demos pass; privacy and manual review pass.

Only the panel demo's three goldens change, as permitted by PR176 CONTRIBUTING
for intentional behavior changes. Its I2C expander initialization now takes bus
time: main startup moves from 725 to 1,147 ms. All console text except timestamps
is identical. Scripted Commando playback starts at 5,778 instead of 5,789 ms.
The fixed seven-second capture therefore changes audio length and hash, and
instructions change from 396,469,561 to 398,637,173. The new mono 22,050 Hz WAV has
135,079 frames and nonzero PCM data; its exact hash repeats in the regenerated
and subsequent full CI runs. The old/new hashes are retained in the receipt.
Golden assertions remain byte-exact; all other goldens remain unchanged.

This models byte/ACK bus time, not SDA/SCL electrical edges, arbitration or clock
stretching. Callbacks observe their enclosing bus tick; direct callers advancing
many phases in one tick must honor `next_deadline` for precise callback time.
Existing machine scheduler granularity still applies. Classic has focused MMIO
coverage, not SCD4x firmware coverage. SEN66/MLX90393 private models were not run,
and no private add-on acceptance or physical sensor timing is claimed. The
100 kHz short-delay limitation remains open at the application timing boundary.

Evidence contains relevant hashes, counts, protocol values and software versions.
Raw logs stay local under `/tmp/gap-i2c-timing-*.log`; compiled firmware and
baseline archives remain ignored. No private device source, personal paths or
host identifiers were copied into this receipt. Earlier receipts are unchanged.
