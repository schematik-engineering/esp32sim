# EX216: host PCM into one-shot ADC conversions

Base `796f64a`, Schematik integration. The implementation is the parent of the
commit containing this receipt; `results.json` records its full revision and
input/output hashes. This is local adoption for review, with no push.

Catalog searches for ADC, analog, microphone, PCM and streaming found EX207,
EX208 and EX213. EX207 supplies voltage conversion and completed observations;
EX213 supplies the independent host clock and two-second drop-oldest design.
EX216 adds that contract to analog pads rather than I2S routing. The existing
EX213 source and retired fork `221080f:esp-periph/src/pcm.rs` were inspected
before implementation. Unlike the fork's raw ADC counts and underrun silence,
this source uses the existing voltage transfer curves and holds the last sample,
as requested. No execution-speed experiment or claim is made.

## API and timing

`esp_periph::analog::AnalogStream::new(rate, cpu_hz, bias, amplitude, now)`
accepts 8,000 through 96,000 samples/second, the chip's `Soc::CPU_HZ`, finite
bias and nonnegative finite amplitude in volts, and current `bus.cycles()`.
Attach `AnalogSource::Stream(stream.clone())` with `SocBus::analog_set`.
Between emulator runs, call `stream.push(&mono_pcm16, bus.cycles())`.
The host retains the clone; an Arc/Mutex shares the queue with the analog pad.
No dependency, private device model or product ABI was added.

The voltage is `bias + PCM16 / 32768 * amplitude`. Before the first host sample
it is bias. After underrun it holds the last sample; push zero PCM for a return
to bias. Each sample becomes current after one host sample period. Integer
fractional phase determines sampling independently of ADC read count or host
chunk size. Wide arithmetic avoids overflow on large timestamp gaps.

Every push, conversion or `queued_samples(now)` query first accounts for elapsed
emulated time. Lazy accounting avoids a new per-tick bus hook: elapsed samples
are skipped even if no ADC conversions occur. The queue retains at most two
seconds, dropping oldest pending samples on oversized or repeated uploads.
Memory remains bounded while a caller does neither reads nor pushes. A query
advances the stream, so callers must supply current time, not a future timestamp.
Older timestamps do not rewind it. Clones may feed multiple pads on the same
chip timeline; they must not be shared across independent machine clocks.

A regression exposed a pre-existing clock mismatch after chip reset. The
peripheral clock restarts but `SocBus::cycles()` continues. ADC conversion writes
now receive bus time on S3/C3/C6, as classic ESP32 already does. Sources and
completed observations retain the existing reset persistence. `Const`, `Wave`,
raw inputs and existing method signatures are unchanged. External exhaustive
matches on `AnalogSource` need to handle its new variant.

## Coverage and limits

Only one-shot ADC conversions are covered. C3/C6 `SarAdc` implements the
one-shot start bit; S3 uses the SENS one-shot controller and has no ADC DMA
producer. None of these three models supports continuous ADC/DMA capture.
Adding the stream does not implement that missing controller mode.

The source uses zero-order hold without anti-alias filtering, microphone noise,
clock drift or hardware calibration. The conversion still completes instantly
at the model's start-register write. Host gain/bias remain adjustable. Classic
ESP32 can use the shared source but is outside this firmware acceptance check.
There is no browser microphone ABI or private add-on acceptance claim.

## Reproduction

Copy the retained sketch and configuration into a temporary project. The same
sketch is used on every chip and was not changed after its initial build.
Arduino-ESP32 3.3.8 and its ADC driver are unmodified. Pioarduino is pinned to
`55.03.38-1`, framework libraries to the platform's IDF 5.5.4 build.

```sh
mkdir -p /tmp/ex216-arduino/src
cp docs/evidence/adc-pcm-2026-10-02/main.cpp /tmp/ex216-arduino/src/main.cpp
cp docs/evidence/adc-pcm-2026-10-02/platformio.ini /tmp/ex216-arduino/platformio.ini
PLATFORMIO_CORE_DIR=/path/to/pinned-pio-core pio run -d /tmp/ex216-arduino
tools/fetch-demo-assets.sh --no-linux
cargo +1.99.0 test -p esp-periph stream_
cargo +1.99.0 test -p esp32sim --test adc_stream stream_conversions
cargo +1.99.0 clippy --workspace --all-targets -- -D warnings
cargo +1.99.0 clippy --release --target wasm32-unknown-unknown -p esp32sim-wasm --features jit-tests -- -D warnings
ESP32SIM_ROM_DIR="$PWD/web/wasm/fw" cargo +1.99.0 test --release --workspace -- --include-ignored --skip external_
ADC_PCM_FIRMWARE_DIR=/tmp/ex216-arduino/.pio/build ESP32SIM_ROM_DIR="$PWD/web/wasm/fw" cargo +1.99.0 test --release -p esp32sim --test adc_stream -- --ignored --nocapture --test-threads=1
RUSTUP_TOOLCHAIN=1.99.0 tools/wasm-build.sh
node tools/wasm-test.mjs hello c3-hello c6-hello c6-energy-scan c6-contiki c6-contiki-net c6-rpl-net panel
node tools/check-evidence-privacy.mjs
git diff --check
```

C3 uses ROM revision 3 and 4 MiB flash; S3/C6 use ROM revision 0 and 8 MiB.
All Rust build/lint/test gates use 1.99.0 without changing the default toolchain.
Only new Rust files were formatted, using the installed stable rustfmt.

The sketch samples GPIO1 on S3 and GPIO0 on C3/C6, 1,024 times, with a 125 us
`micros()` deadline and ordinary `analogRead`. It emits raw ADC values and
sample timestamps after capture, so serial output does not disturb sampling.
The host sends PCM16 at 16 kHz in repeated chunks, generating 440 Hz at half
scale with 1 V bias and 0.4 V full-scale amplitude. The host analysis converts
raw counts to millivolts using the existing calibration curves, removes DC and
searches 1..3999 Hz in 1 Hz steps using actual firmware sample timestamps.
Acceptance is 440 +/- 5 Hz, 200 +/- 10 mV peak and 8 kHz +/- 2% sampling rate.
These are synthetic functional checks, not independent calibration evidence.
Silence requires identical samples at 1000 +/- 2 mV. After capture, two
three-second uploads each retain exactly 32,000 samples while the conversion
generation stays unchanged. Tests needing these local firmware files have
`external_*` names and explicit missing-input errors.

## Failures retained

The first firmware harness compile failed because `uart_input` needs a byte
slice and an imported Bus trait was unused. Both were fixed. A subsequent
register test needed that trait restored. The reset test then failed with raw
samples `[2824, 930, 930, 930]` instead of advancing past the second sample;
the bus-clock change fixes the root cause. The same check now passes on all
three chips. Initial and final local log hashes are retained in `results.json`.
No firmware modifications, tolerance relaxations or golden updates were used.

## Evidence privacy

The receipt retains numeric summaries, source/firmware/ROM/WASM hashes and
command outcomes. Raw local logs remain outside Git; personal paths, boot
output, unrelated inventories and session identifiers are omitted. No private
captures or compressed profiles are committed. This limits reconstruction of
complete logs, but preserves the actual checks, inputs and measured values.

## Final results

Implementation `72f123d`. All required gates pass: 640 release workspace tests,
zero failures or ignored tests after excluding `external_*`; both strict Clippy
commands; production WASM build and all eight requested Node workloads;
evidence privacy and diff checks. Three focused checks cover stream timing,
overflow, bias, hold-last, clone sharing, fractional phase, extreme timestamps
and actual conversion/reset paths. Three external tests each check silence
and tone, for six captures and 6,144 firmware ADC samples.

| Chip | Firmware sample rate (Hz) | Dominant tone (Hz) | Peak amplitude (mV) | Empty-stream AC amplitude (mV) |
| --- | ---: | ---: | ---: | ---: |
| S3 | 8000.063 | 440 | 200.053 | 0 |
| C3 | 8000.000 | 440 | 200.341 | 0 |
| C6 | 8000.000 | 440 | 200.463 | 0 |

Every empty-stream reading is 1000 mV. Each capture's stopped-reader bound is
32,000 samples at 16 kHz. `results.json` retains instruction/cycle counts and
host push counts. Reported tone frequency is the peak of the 1 Hz search grid;
the finite 128 ms capture has roughly 7.8 Hz spectral resolution. The +/- 5 Hz
assertion checks this known synthetic tone, not general 1 Hz resolving power.
The peak amplitude error is below 0.24%, within the declared 5% tolerance.

The initial local commit command hit the shared Git-metadata sandbox boundary.
An authorized escalation created the local commit; no review rejection occurred.
Only this worktree's index and branch metadata were updated. No other worktree
files, private add-on files, GitHub comments or remote refs were changed.
