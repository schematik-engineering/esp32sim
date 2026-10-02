# EX214: released GPIO inputs and S3 sonar timing

Base `8cc5d233611d32586bfbb4c4885f6570aa8a64ba`; implementation
`77f7dbeb9e034b7bcee426c232b582881b25b1bb`. Local branch `gap-gpio-input`.
Nothing pushed. The GPIO change is adopted locally. The S3 sonar failure under
default instruction timing remains open; the existing approximate timing mode
passes. No timer, CPU, scheduler or JIT implementation was changed.

## Design and related work

EX209 exposes programmed GPIO state without resolving pad levels. EX201 and EX206
supply output timestamps and board input deadlines. EX047 documents that scheduler
quantum changes alter timing semantics. EX138 and EX139 cover the existing optional
S3 timing model. This experiment adds a different correctness contract: releasing
an externally driven input and running the retained keypad/sonar firmware at the
add-on's quantum of one. It is not a throughput experiment.

Inspected the shared GPIO and board/bus implementations at retired fork
`221080ffccfa829106b398f896653535853c76c8` before implementing. Reused its external
mask and pull resolution rule, without its duplicated IO_MUX state or unrelated
waveform/device features.

`Gpio::set_input` remains an absolute external drive. New `release_input` clears
that drive. GPIO reads and edge detection resolve external drive first, then an
enabled output, then IO_MUX pulls. No pull and simultaneous pulls resolve high,
matching the fork's deterministic digital model. This does not model contention,
analog pad voltages or peripheral output routing.

`BoardEdge` is unchanged. `BoardModel::released_inputs` has an empty default and
returns the current set of board-managed undriven pins. S3/C3/C6 apply this after
board edges at device ticks and on board attachment/reset. A board emits a low
`BoardEdge` when it closes a switch, then includes the pin in `released_inputs`
when it opens the switch. It must remove a pin from that set before driving it.
`SocBus::gpio_release_input` also permits immediate host release, with an empty
default for other implementations. S3 refreshes IRQ and PCNT work; changed output
pads can now generate edge interrupts as well as level interrupts.

IO_MUX writes update shared pull masks on S3/C3/C6. Classic ESP32 retains its routed
pad and RTC rules, while its pull emulation stops using `set_input` to impersonate
an external driver. Classic host release has a focused pull/override check; no
classic Arduino or board-release scenario is claimed.

## Firmware and observations

The original source, pin maps, package identities and flash segment hashes are
retained in [firmware](firmware/) and [inputs.json](inputs.json). These are the
report's already-built Arduino-ESP32 3.3.8 artifacts, with NewPing 1.9.7,
Keypad 3.1.1 and Encoder 1.4.4. The sketch and embedded flash bytes were unchanged.
No local firmware rebuild was needed. Reproduction INIs omit only the private
post-build capture scripts; the compiler's original configuration hash is retained.

| Check | S3 | C3 | C6 |
| --- | --- | --- | --- |
| Keypad, normal native quantum 64 | `1,5,D` | `1,5,D` | `1,5,D` |
| Sonar, instruction timing, quantum 1 | **fails**, 0 us | 5800 us | 5799 us |
| Trigger width, same run | **2388 / 2400 required cycles** | 1748 / 1600 | 1779 / 1600 |
| Sonar, existing S3 approximate timing | 5800 us, trigger 2624 cycles | not applicable | not applicable |

All listed runs had zero reboots. The keypad board only drives connected rows
low, releasing every other row. Each key is held 100 ms and released 100 ms;
assertions require exactly three output lines and no spurious key.
The sonar keeps the fork's strict 10 us minimum, 100 us echo delay, 58 us/cm
width and the original 5800 +/- 50 us assertion. These are single functional
samples, not hardware timing measurements or a repeated statistical battery.

## Why the S3 returns zero

The native run reproduces the failure before any browser JIT is involved. At
quantum 1 the board receives a 2388-cycle trigger, or 9.95 us at 240 MHz, and
correctly schedules no echo. Therefore `pulseIn` times out. The GPIO release
change leaves that sample unchanged. Turning virtual quanta off also leaves it
unchanged, excluding that optimization as the cause in this reproduction.

Arduino 3.3.8's [delayMicroseconds implementation](https://github.com/espressif/arduino-esp32/blob/3.3.8/cores/esp32/esp32-hal-misc.c#L199-L212)
waits for a whole-microsecond timer value to reach its starting value plus the
requested delay. It does not guarantee ten full microseconds between GPIO writes
under an ideal one-cycle instruction model. The temporary SYSTIMER trace shows
initial count 91306, or 5706.625 us, and final count 91456, or 5716 us. Their integer
values differ by ten while the actual sampling interval is only 2258 CPU cycles,
9.408333 us. The GPIO/call overhead adds 130 cycles, giving 2388. Counter values
advance at the configured 16 MHz; no timer drift was found.

The [pulseIn implementation](https://github.com/espressif/arduino-esp32/blob/3.3.8/cores/esp32/wiring_pulse.c)
uses cycle-count reads. Those reads are not responsible for the missing echo.
C3/C6 instruction paths have different overhead and CPU frequencies. They cross
the same strict threshold in this sample. Quantum 64 adds enough scheduling
rounding to produce 2432 cycles on S3 and a 5799 us echo, but that is not an
adopted fix. Changing timer values, lowering the sensor threshold, or imposing an
S3-only trigger allowance would conceal the observed cause.

The existing `ApproximateCostModel::default()` produces a 2624-cycle trigger and
5800 us echo with the same firmware and sensor model. A caller can configure it
before any execution:

```rust
machine.set_cost_model(Box::<esp32s3::ApproximateCostModel>::default())?;
```

This is a verified configuration option, **not a default-timing emulator fix**.
The read-only add-on has not been modified. Making physical instruction latency
the default would change the established timing and golden-output contract. No
such broad change is adopted here. S3 sonar at default timing/quantum 1 remains
an explicit acceptance failure.

## Reproduce

From this worktree, using the retained artifact directory supplied by the caller:

```sh
python3 docs/evidence/gpio-input-2026-10-02/extract.py "$ARTIFACT_ROOT" /tmp/gpio-input-fixtures
export GPIO_INPUT_FIRMWARE=/tmp/gpio-input-fixtures
export ESP32SIM_ROM_DIR="$PWD/web/wasm/fw"
cargo +1.99.0 test --release -p esp32sim --test gpio_inputs -- --include-ignored --skip external_s3_sonar --nocapture
# Expected negative under default timing:
cargo +1.99.0 test --release -p esp32sim --test gpio_inputs external_s3_sonar -- --ignored --nocapture
# Existing timing model, same strict check:
GPIO_APPROXIMATE=1 cargo +1.99.0 test --release -p esp32sim --test gpio_inputs external_s3_sonar -- --ignored --nocapture
# Diagnostic scheduling ablations, not adopted:
ESP32SIM_VQ=1 cargo +1.99.0 test --release -p esp32sim --test gpio_inputs external_s3_sonar -- --ignored --nocapture
GPIO_QUANTUM=64 cargo +1.99.0 test --release -p esp32sim --test gpio_inputs external_s3_sonar -- --ignored --nocapture
```

`ARTIFACT_ROOT` contains `<chip>/artifact.json`. The extractor checks all four
flash segments per chip against the retained hashes before writing test inputs.
The local source is `/tmp/gpio-firmware/inputs`. For a fresh build, copy
`firmware/main.cpp`, the chip's pins header as `src/fixture-pins.h`, and its INI
as `platformio.ini` into a scratch project, then run `pio run -e serial`.
The external test consumes files named by decimal flash offsets and `flash.txt`
listing those offsets. Copy bootloader, partitions, boot_app0 and firmware to
`0`, `32768`, `57344`, `65536` respectively. Fresh builds must have their own
hashes recorded; the extractor intentionally rejects a different binary.

The timer trace was temporary instrumentation before `self.periph.write32(a, v)`
in S3 `periph_write_inner`, logging bus cycles and `systimer.unit[0]` on writes
to `0x60023004` while GPIO1 output and enable were high. No instrumentation remains
in production source. [results.json](results.json) retains its first/last samples,
count, original raw-log hash, and the exact work counts for each run. Raw local
logs are retained under ignored `target/ex214`; no raw logs are committed.

## Required checks and limits

All use Rust 1.99.0 without changing the default toolchain:

```sh
tools/fetch-demo-assets.sh --no-linux
cargo +1.99.0 clippy --workspace --all-targets -- -D warnings
cargo +1.99.0 clippy --release --target wasm32-unknown-unknown -p esp32sim-wasm --features jit-tests -- -D warnings
ESP32SIM_ROM_DIR="$PWD/web/wasm/fw" cargo +1.99.0 test --release --workspace -- --include-ignored --skip external_
RUSTUP_TOOLCHAIN=1.99.0 tools/wasm-build.sh
node tools/wasm-test.mjs hello c3-hello c6-hello c6-energy-scan c6-contiki c6-contiki-net c6-rpl-net panel
node tools/check-evidence-privacy.mjs
```

Both Clippy checks pass. Workspace tests pass, 611 passed, none failed or ignored;
external tests are filtered as requested. All eight WASM scenarios and the JIT
handoff smoke check pass. Five focused GPIO checks and five default-timing firmware
checks pass; the separate S3 approximate-timing check passes. Existing goldens are
unchanged. The new firmware tests are named `external_*` and report required
inputs. Only the newly created Rust test file was formatted, using installed
stable rustfmt because the 1.99.0 toolchain lacks that component.

The initial fixture test compile omitted the C3/C6 flash-size constructor argument;
that test setup error was corrected before baseline execution. The unsuccessful
1.99.0 rustfmt invocation changed no file. No full-repository formatting occurred.

Committed receipts omit local identities, thread identifiers and build-session
information. Numeric measurements and original artifact/log hashes are unchanged.
No private original or backup is committed. No browser-specific sonar acceptance,
physical hardware calibration, analog contention, new PWM/waveform model, or
private add-on integration is claimed.
