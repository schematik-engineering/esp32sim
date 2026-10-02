# EX213: pin-routed PCM sources with independent clocks

Base `8cc5d233611d32586bfbb4c4885f6570aa8a64ba`, Schematik integration.
The candidate is the commit containing this receipt. `results.json` records
source, firmware, ROM, test executable and WASM SHA-256 hashes.

Catalog searches for microphone, audio, PCM, I2S, PDM and timing found EX208,
EX134 and EX157. EX208 supplies the existing RX packing and GDMA path.
EX213 adds a different source contract: physical routing, independent host
sample rates, drop-oldest queues and consumption observation. It makes no
execution-speed claim. The preserved implementation at retired fork
`221080ffccfa829106b398f896653535853c76c8` was inspected in
`esp-periph/src/pcm.rs`, `esp-periph/src/i2s.rs` and the three chip buses.
The new implementation shares sources across S3 ports rather than moving
per-controller sources when firmware switches ports.

## Design and API

`SocBus::pcm_sources()` exposes 16 optional `PcmSource` slots on S3, C3 and C6.
Construct a source with a host sample rate from 8,000 through 96,000 Hz and
`PcmPins::I2s { bclk, ws, data }` or `PcmPins::Pdm { clk, data }`.
Pins must be distinct and fit the shared GPIO model's 0 through 48 range.
Use pins available on the chosen chip. `push` accepts stereo PCM16 frames;
duplicate mono samples into both lanes. Assign `None` to detach a slot.

Each source retains at most two seconds at its own rate. Oversized writes
keep their newest frames and discard older queued audio. The queue drains
in emulated peripheral time whether RX is running, unwired or stalled on
DMA. Empty sources produce silence. Sample lookup uses the exact RX frame
offset within each device tick; all sources advance once after all ports
sample that tick. Zero-order resampling makes this independent of device
tick chunk size. S3 flushes deferred device time before exposing the bank
for host writes.

Any attached slot enables physical routing for all RX ports. Every required
wire must match the GPIO matrix, including input matrix selection and no
signal inversion. The lowest matching slot wins. PDM CLK uses the RX WS
output signal, as in ESP-IDF's `i2s_pdm_rx_set_gpio`. An unmatched source
produces zero PCM through normal DMA completion. Removing all slots restores
the existing `i2s_input(port)` behavior, including its FIFO and tone API.

`i2s_selected_source(port)` reports the slot selected on the last active RX
DMA tick. `None` denotes legacy, unwired, stopped or unsupported RX.
`PcmSource::consumed_frames` counts RX frame samples, including underrun
silence. It counts sampling before DMA descriptor acceptance, not application
reads or elapsed host frames. Unselected sources still drain but do not
increment this counter. Chip resets preserve source slots, queues, clock
phase and counters; peripheral routing and selected-source observations reset.

No private add-on ABI or CLI option is needed. A host can attach sources and
poll the selected slot and counter through the generic Rust API.

## Results

All required gates pass with Rust 1.99.0. The release workspace suite passes
612 tests with zero failures and zero ignored tests after excluding
`external_*`. Native and WASM Clippy pass with warnings denied. The production
WASM build and all eight requested WASM scenarios pass, including panel and
the two-node network scenarios. Privacy and diff checks pass.

Five I2S unit tests and two register-driven integration tests cover legacy
input, queue bounds and overflow ordering, fractional source clocks, underrun,
tick partitioning, shared sampling, wrong wires, matrix selection and input
inversion, all three chips, stalled DMA, reset, S3 port changes and PDM wiring.
The full suite includes the existing RX DMA ownership and descriptor tests.

The unchanged EX208 Arduino-ESP32 3.3.8 `ESP_I2S` sketch was rebuilt for S3,
C3, C6 and S3 PDM using pioarduino `55.03.38-1`. Neither sketch nor driver
was changed. Four external tests run three wiring cases each, for 12 cases
and 24 reads of 512 bytes, including a read after software reset in each case:

- Slot 0 wired: 1,000 Hz, RMS 11,584.92.
- Slot 15 wired: 2,000 Hz, RMS 11,585.12.
- Neither wired: every sample is zero, RMS and frequency both zero.

The other slot's consumption count remains zero. Selection observation agrees
with the wired slot. A three-second upload retains exactly 32,000 frames at
16 kHz. After the sketch calls `mic.end()`, another oversized upload still
retains only 32,000 frames. The queue then drains during the sketch's delay
before its next reset, while the consumption counter stays unchanged.
`results.json` retains each run's queue length, elapsed cycles, instruction
count, samples and source counters.

## Reproduce

Copy the existing EX208 sketch and PlatformIO configuration to a temporary
project. Provide a PlatformIO core with the pinned platform and its packages.
No other worktree needs modification.

```sh
mkdir -p /tmp/gap-audio-arduino/src
cp docs/evidence/i2s-rx-2026-10-02/main.cpp /tmp/gap-audio-arduino/src/main.cpp
cp docs/evidence/i2s-rx-2026-10-02/platformio.ini /tmp/gap-audio-arduino/platformio.ini
PLATFORMIO_CORE_DIR=/path/to/pio-core pio run -d /tmp/gap-audio-arduino
tools/fetch-demo-assets.sh --no-linux
cargo +1.99.0 clippy --workspace --all-targets -- -D warnings
cargo +1.99.0 clippy --release --target wasm32-unknown-unknown -p esp32sim-wasm --features jit-tests -- -D warnings
ESP32SIM_ROM_DIR="$PWD/web/wasm/fw" cargo +1.99.0 test --release --workspace -- --include-ignored --skip external_
PCM_FIRMWARE_DIR=/tmp/gap-audio-arduino/.pio/build ESP32SIM_ROM_DIR="$PWD/web/wasm/fw" cargo +1.99.0 test --release -p esp32sim --test pcm_sources_arduino -- --ignored --nocapture --test-threads=1
RUSTUP_TOOLCHAIN=1.99.0 tools/wasm-build.sh
node tools/wasm-test.mjs hello c3-hello c6-hello c6-energy-scan c6-contiki c6-contiki-net c6-rpl-net panel
node tools/check-evidence-privacy.mjs
git diff --check
```

The external tests fail with an explicit missing-input message if firmware or
ROM paths are absent. S3 and C6 use 8 MiB flash, C3 uses 4 MiB. The C3 ROM is
revision 3; the S3 and C6 ROMs are revision 0. Reset evidence uses the emulator's
software-reset stop followed by a second read, rather than the sketch's RTC
boot counter, which reinitializes during these ROM boots.

## Retained failures and limits

The first register PDM test used an 8,333 Hz receiver clock against an 8,000 Hz
source and incorrectly expected an available sample before the source's first
frame. The test now programs a 4,000 Hz receiver and advances a full frame.
The first Arduino PDM run then exposed a real routing error: CLK was matched
to BCLK instead of WS. Standard S3, C3 and C6 already passed that run. The
mapping and register check were corrected; all four firmware tests passed on
retry and again with the stopped-RX assertion. The receipt retains that
failed run's original log hash and its result, without private boot output.

The first external test compile used equality on `Stop`, which does not
implement `PartialEq`; it now uses `matches!`. Initial Clippy found one
ambiguous test expression, corrected with parentheses. Rustfmt is absent
from the installed 1.99.0 toolchain, so the installed stable rustfmt formatted
only the three new Rust files. All compilation, lint, native and WASM checks
used 1.99.0; the default toolchain was unchanged.

This is PCM at the peripheral boundary. It does not simulate serial edges,
PDM filtering, analog microphone noise, clock drift, external slave clocks or
anti-alias filtering. Converted PDM is supported only on S3 I2S0, as in EX208;
C3, C6 and S3 I2S1 raw PDM remain unsupported. No real-hardware or browser
microphone capture was compared. Browser demo checks validate the existing
WASM workload, not host microphone input. ADC microphone sources are outside
this gap. Concurrent compilation and checks make wall times unsuitable for
performance conclusions.

Only this worktree was changed. No GitHub write, push or product add-on change
was made. Evidence omits personal paths, unrelated boot logs, process lists
and environment inventories. Selected numeric lines are retained verbatim;
raw local logs are identified by hash, not committed. There are no compressed
profiles or private capture backups in this receipt. These omissions prevent
reconstructing full logs but preserve the assertions and measured values.
