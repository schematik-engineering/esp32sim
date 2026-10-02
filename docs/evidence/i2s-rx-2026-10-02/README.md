# EX208: host PCM through I2S RX DMA

Base `dddb128052dca15250e2169b92ab73c4d87f524c`. The change is independent of
classic ESP32 and other peripheral branches. `provenance.json` records the
measured source file hashes, toolchain, and native/WASM hashes. The commit that
adds this receipt contains those source files.

The fork at `221080ffccfa829106b398f896653535853c76c8` was inspected for behavior,
specifically `esp-periph/src/{i2s,pcm}.rs` and C3's GDMA register translation.
This port uses one bounded stereo queue per controller, not the fork's device
slots, pin records, or product ABI. The TX DMA implementation is unchanged.
C3 now translates its combined GDMA interrupt words and relocated channel
registers while preserving the existing public `Gdma` field type. C6 reads its
RX clock from PCR. Active RX participates in device scheduling on every chip.

Catalog searches for I2S, PCM, microphone, PDM, RX, and DMA found no previous
host I2S RX experiment. EX134 and EX157 concern tick cadence and passive DMA
channels. EX208 differs by adding a clocked receive producer and testing its
PCM, descriptor, and interrupt contract. It is not a speed experiment.

## Contract and results

- `SocBus::i2s_input(port)` defaults to unavailable and is implemented on all
  three chips. `PcmInput::push` accepts stereo i16 frames, returns the accepted
  count, and caps the queue at 65,536 frames. Empty input generates silence.
  Mono callers duplicate a sample into both lanes. The guest chooses its sample
  rate and slot; no host resampling occurs. The queue and tone survive reset.
- Standard RX supports 16/24/32-bit mono/stereo. The DMA format for 24/32-bit
  samples is a signed i16 shifted left by 16, stored in a 32-bit word.
- S3 I2S0 supports converted PDM16, using the programmed 64/128 decimation rate.
  PCM enters after conversion, so this does not measure filter response.
- The DMA check covers descriptor splits, lengths, owner writeback/checks,
  terminal completion, interrupt status/EOF address, zero-progress rings,
  buffer faults, and host input persistence across full chip reset.
- All eight Arduino runs pass. Standard S3/C3/C6 and S3 PDM report 512 bytes,
  RMS 11,584.92 and dominant bin 1,000 Hz for a half-scale 1 kHz tone at 16 kHz.
  Every silence sample is zero. Each run includes another successful read after
  a software reset. The acceptance tolerances are RMS within 1% of
  `32767 * 0.5 / sqrt(2)` and frequency within one 256-point DFT bin, 62.5 Hz.
  Serial readings and reset markers are retained in `arduino.json`.

`RTC_DATA_ATTR boots` reinitializes to 1 in these ROM boots, so the firmware
continues resetting. Reset proof uses the emulator's reset marker followed by
another complete read, not that counter. This existing RTC initialization
behavior was not changed.

## Reproduce

Copy `platformio.ini` and `main.cpp` into a temporary project, with the latter
at `src/main.cpp`. Build with `pio run -d /path/to/project`. The pinned platform
is `https://github.com/pioarduino/platform-espressif32.git#55.03.38-1` and the
framework reports Arduino-ESP32 3.3.8. No framework or driver source is patched.
The four environments use `esp32-s3-devkitc-1`, `esp32-c3-devkitm-1`, and
`esp32-c6-devkitc-1`. S3 and C6 images require 8 MiB flash; C3 requires 4 MiB.

```sh
cargo build --release
cargo test --workspace
tools/wasm-build.sh
python3 docs/evidence/i2s-rx-2026-10-02/check.py \
  --emulator target/release/esp32sim \
  --firmware /path/to/project/.pio/build \
  --rom-dir /path/to/roms --output /tmp/i2s-check.json
node tools/check-evidence-privacy.mjs
git diff --check
```

The checker contains the exact emulator arguments and applies assertions to
all measurements, including the post-reset reads. ROM names and input hashes
are in `arduino.json`; firmware ELF and binary hashes are in `firmware.json`.
The PlatformIO `.pio` directory was removed after recording these hashes.
Compiled firmware is reproducible from the retained sketch and configuration;
byte identity can depend on tool paths and build timestamps.

`validation.json` contains the command outcomes. The WASM check is a production
build check; firmware execution here uses the native emulator. No browser or
real-hardware microphone comparison was performed. Concurrent build/test work
makes the printed wall times unsuitable for a performance claim.

## Limits and retained failures

The host source attaches at the controller's PCM/DMA boundary, without GPIO
routing or serial bit timing. Raw PDM is unsupported, including C3/C6 and S3
I2S1, which have no hardware PDM-to-PCM converter. External slave clocks, TDM
beyond two slots, companding, and reversed bit/byte order are unsupported.
The standard and PDM mode distinction follows the
[ESP-IDF 5.5 I2S mode table](https://docs.espressif.com/projects/esp-idf/en/v5.5/esp32s3/api-reference/peripherals/i2s.html#overview-of-all-modes).
C3/C6 TX remains unimplemented, as on the base revision.

`attempts.json` preserves the failed package-lock attempt and the incorrect
flash-size runs, their exact errors, artifact hashes, and resolutions. No
failed run is counted as a pass. Preliminary attempts used the uncommitted
implementation; only the final run's full source-file hashes are retained.

Receipts retain relevant serial lines, numeric samples, and artifact hashes.
Unrelated boot output and local path listings are omitted. The package-lock
error's home directory is normalized to `/Users/alice`. Build logs are reduced
to outcomes and test counts; no process lists, hostnames, or device identifiers
are retained. These omissions prevent reconstructing full log bytes, but do
not affect the PCM or reset checks.
