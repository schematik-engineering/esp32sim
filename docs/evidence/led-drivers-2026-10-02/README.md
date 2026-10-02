# EX217: RMT routing, DMA and C6 parallel LED output

Schematik integration base `6e562b863f4a5c94c5055747e8331452adaf2de1`,
frozen behavior reference `221080ffccfa829106b398f896653535853c76c8`.
Local adoption on `fix-leds`; no push or PR. This extends EX205's compact RMT
and EX215's GPIO waveform coverage with reused routes, S3 RMT DMA and C6 PARLIO.
It is a functional compatibility check, not a speed or silicon-timing claim.

## Causes and changes

- RMT delivery selected the first matrix route even after its pin was disabled.
  Shared `pins_for_signal` now selects every enabled, non-inverted route, as in
  the fork. S3/C3/C6 deliver each frame to all matching pins.
- S3 FastLED's DMA channel had no RMT consumer. GDMA trigger 9 now fills a
  bounded 48-symbol FIFO. RMT channel 3 consumes it at the programmed symbol
  rate, waits when empty, and completes on the end marker. Descriptor ownership,
  size/alignment, checked buffer addressing, writeback and interrupt behavior
  follow the fork. Current non-DMA stop, loop and memory-exhaustion behavior stays.
- C6 FastLED waited for an absent PARLIO FIFO/status/completion path. PARLIO now
  models TX configuration, PCR clock/reset, sample width/order and completion
  interrupt 63. GDMA trigger 9 prefills its 64-byte FIFO even with the TX clock
  disabled. Routed lanes reach `BoardModel::parallel_output`.
- The unchanged private add-on implements only `rmt_frame`. The default parallel
  callback therefore uses the fork's WS2812 pulse decoder and forwards decoded
  frames through that existing callback. Other parallel boards can override it.

Eight focused checks cover route reuse/fan-out, RMT descriptor return and bounds,
PARLIO clock/FIFO readiness and sample order, ownership rejection, and parallel
lane identity, mirrored routes and reset boundaries.

## Reproduction

Original acceptance negatives are preserved in `inputs.json` by log hash and
outcome: NeoPixel S3/C3/C6 overwrote the first strip while leaving the second
black; FastLED S3 left the DMA strip black; FastLED C6 never completed show.
The original candidate WASM hash was
`9338723baa2a1b3fde5f501e983f4b5d140e6f586f9614cda51e1f69abd8e63d`.

The add-on was cloned locally into `/private/tmp/esp32sim-leds-addon` and checked
out at `b0f59f359ca46509f7a1a4c0dbad9066ca6bb7e8`, the acceptance revision.
Only the scratch Cargo files changed. A
`[patch."https://github.com/schematik-engineering/esp32sim"]` table points
`esp32s3`, `esp32c3`, `esp32c6`, `esp32`, `esp-soc`, `esp-periph`, and `xtensa-lx7`
to their paths in this checkout. `cargo update --offline` refreshed the scratch
lockfile. The original builder then ran unchanged:

```sh
RUSTUP_TOOLCHAIN=1.99.0 python3 /private/tmp/esp32sim-leds-addon/scripts/build-assets.py \
  /private/tmp/esp32sim-leds-assets --assets-source /private/tmp/esp32sim-acceptance/assets
cd /private/tmp/esp32sim-acceptance
python3 restore-replay.py graphics-drivers-neopixel
python3 restore-replay.py graphics-drivers-fastled
cd app
node tests/fixtures/esp32sim/graphics-drivers/verify.mjs \
  ../replays/graphics-drivers-neopixel --wasm /private/tmp/esp32sim-leds-assets/esp32sim.wasm esp32s3
node tests/fixtures/esp32sim/graphics-drivers/verify.mjs \
  ../replays/graphics-drivers-fastled --wasm /private/tmp/esp32sim-leds-assets/esp32sim.wasm esp32s3
```

Repeat NeoPixel for C3/C6 and FastLED for C6. To replay the fork, omit `--wasm`
and its argument and add `--loader ../fork-loader.mjs` immediately after `node`.
All five candidate and five fork runs passed on identical firmware. The verifier
checks byte equality against the restored compiler artifacts, two instances on
distinct pins, exact four-pixel RGB patterns, alternate frames and firmware reboot.
Both candidate and fork receipts, firmware hashes, WASM hashes and single wall
samples are in `reproductions.json`. Runs overlapped native compilation; those
wall samples are not performance measurements.

The unmodified builder's manifest still reports the declared dependency rev
`d1d22ae` and `dirty: true`. The actual source is the path override above, identified
by the integration base and changed Rust file hashes in `inputs.json`.
The asset source supplies the same ROMs, JS runtime and JIT as acceptance.

## Quality gate

The first workspace run failed `rmt_channels_deliver_colours_and_raise_c3_interrupts`: expected two pin frames, received none. Its synthetic setup routed GPIO5/6 but never enabled their outputs. Adding the missing GPIO enable write fixes the test setup without weakening its color or interrupt assertions. The full gate was retried once.

All required gates pass: 654 release workspace tests, zero failures, 50 external tests filtered out; both strict Clippy checks; WASM build and all eight requested scenarios; evidence privacy. Gate results are recorded in `gates.json`. All Cargo commands use Rust 1.99.0;
the default toolchain was not changed. No `cargo fmt` was run. Initial staging hit the sandbox restriction on the shared Git metadata; the authorized local staging operation succeeded with escalation. No other worktree files were changed.

## Limits and privacy

Only the five assigned acceptance cases were replayed, not all 422 cases.
PARLIO RX, electrical contention, inverted output synthesis and calibrated
hardware timing remain outside this contract. The parallel fallback retains the
fork's permissive 150..1100 ns high-pulse window, 550 ns bit threshold and 50 us
reset delimiter, including flushing the final partial transmission at transfer end.
RMT observation retains at most 4096 RGB pixels. FIFO pumping bounds work per tick;
this is not a DMA arbitration model.

Receipts retain hashes, commands, results and anonymous platform/tool versions.
No private source, firmware, process inventory, login/hostname or raw capture is
committed. PASS records were extracted from raw logs; loader warnings and local
paths were omitted. This does not change the numeric or correctness results.
Raw logs remain locally under `/tmp/esp32sim-leds-logs`; original negative logs
remain under `/private/tmp/esp32sim-acceptance/logs`. The private firmware and
verifier must be available to rerun the acceptance checks.
