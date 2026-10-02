# EX212: S3 camera start and streaming capture

Base `8cc5d233611d32586bfbb4c4885f6570aa8a64ba`, local branch `gap-camera`.
Inspected `221080f:esp32s3/src/bus.rs::dma_cam_step` and its `LcdCam` model before editing.
The implementation is identified by the source hashes in [inputs.json](inputs.json).
Related EX134/EX157 concern cadence; EX208 supplies the shared streaming GDMA receive method.
This experiment tests the camera driver's VSYNC-before-start contract, not throughput.

## Design and changes

The old engine required CAM_START, an armed GDMA channel and a complete successful frame scatter
before raising VSYNC. The unmodified camera driver waits for VSYNC before starting capture.
The sensor clock now advances while capture is stopped and survives CAM_RESET. At each frame
boundary the existing BoardModel::camera_frame hook supplies the sensor frame; an available frame
raises VSYNC regardless of CAM_START or GDMA. The driver can arm capture during blanking.

The retained frame is an Arc, not a per-tick copy. The existing GdmaInCh::receive method handles
partial descriptors, length/ownership writeback, descriptor faults and byte-count EOF. Camera
bit reversal and VSYNC-selected frame EOF follow the fork. Bytes advance even with capture
stopped, so late starts do not replay the beginning of a frame. Enabling CAM_VSYNC keeps device
ticks at the active cadence before CAM_START. No board API signature or private sensor model changes.

The fork's approximate timing remains: 5% blanking followed by half a period of active bytes.
The public frame_cycles setting remains adjustable, default 10 fps. This is not a calibrated
PCLK/HREF waveform model. The board hook owns sensor power/configuration and returns None when
it cannot supply a frame. Its existing API cannot distinguish an absent sensor from absent host
image data; both suppress frame publication/VSYNC. DVP pin validation, physical electrical behavior,
PSRAM framebuffer mode, arbitrary sensor rates and the full WiFi CameraWebServer application
are not tested here. Frames are counted at sensor publication, as in the fork.

## Checks and results

Rust/Cargo 1.99.0, Node v22.23.1, macOS arm64. Toolchain selected per command; default unchanged.
Single correctness runs under uncontrolled host load; smoke-test wall times in results.txt are
observations, not performance comparisons. No golden outputs changed.

```sh
cargo +1.99.0 test -p esp32s3 --lib camera
cargo +1.99.0 clippy --workspace --all-targets -- -D warnings
cargo +1.99.0 clippy --release --target wasm32-unknown-unknown -p esp32sim-wasm --features jit-tests -- -D warnings
tools/fetch-demo-assets.sh --no-linux
ESP32SIM_ROM_DIR="$PWD/web/wasm/fw" cargo +1.99.0 test --release --workspace -- --include-ignored --skip external_
RUSTUP_TOOLCHAIN=1.99.0 tools/wasm-build.sh
node tools/wasm-test.mjs hello c3-hello c6-hello c6-energy-scan c6-contiki c6-contiki-net c6-rpl-net panel
node tools/check-evidence-privacy.mjs
```

Three focused camera tests pass: VSYNC-before-start and partial descriptor delivery with both
EOF modes and bit reversal; reset/stop independence and DMA failure; stopped capture discards
sensor bytes and unavailable input clears the retained frame. Both Clippy commands pass.
Release workspace: 610 passed, zero failed/ignored, 21 external tests filtered out.
All eight WASM workloads and the JIT scheduler-handoff check pass. Privacy check passes.
Eight native tests in the unchanged add-on pass, including its camera SCCB, geometry, reset,
formats and host-input bounds test.

## Unchanged driver proof

The exact previously compiled Arduino-ESP32 3.3.8 sketch is preserved as [firmware.cpp](firmware.cpp).
It uses esp_camera_init, esp_camera_fb_get, esp_camera_fb_return and esp_camera_deinit without
patching the Espressif driver. PlatformIO platform: pioarduino 55.03.38-1, S3 devkit, 8 MiB flash,
no PSRAM, CAMERA_FB_IN_DRAM, one framebuffer. No firmware was recompiled for this experiment.
The saved compiler flash manifest is checked against every loaded segment. The artifact's old
emulatorVersion field identifies its earlier recording, not the emulator used for this run.

Private add-on revision `77fad324fda139476206606879447da999b33570` supplies OV2640/OV5640 SCCB
models and host frames through the existing board API. A temporary copy of its src, Cargo.toml
and build.rs was used; only Cargo dependencies were redirected to this worktree. Original add-on
worktrees were read only. No private model source or firmware binary is committed here.

To reproduce, supply a copy of that add-on with path dependencies to this source tree, the saved
firmware directory containing esp32s3/{artifact.json,fixture.json,flash-artifacts.json}, and the
JPEG identified by inputs.json. The external check requires those developer inputs and is not a CI test.

```sh
# ADDON_COPY, FIRMWARE_ROOT and JPEG are caller-supplied local paths.
RUSTUP_TOOLCHAIN=1.99.0 cargo test --manifest-path "$ADDON_COPY/Cargo.toml"
(
  . tools/wasm-rustflags.sh
  RUSTUP_TOOLCHAIN=1.99.0 cargo build --manifest-path "$ADDON_COPY/Cargo.toml" --release --target wasm32-unknown-unknown
)
node docs/evidence/camera-start-2026-10-02/external-camera.mjs \
  "$PWD" "$FIRMWARE_ROOT" "$PWD/web/wasm/fw" \
  "$ADDON_COPY/target/wasm32-unknown-unknown/release/esp32sim_schematik.wasm" "$JPEG"
```

For each sensor the check asserts three 96×96 RGB565 captures, size 18432, format 0, FNV-1a
19e161b5; another identical capture after ESP.restart; then deinit/init to JPEG and three 160×120
captures, size 2443, format 4, FNV-1a 0ead8274. It also checks absent host input, reset and bounds.
The RGB565 host pattern writes little-endian `(byte_offset * 37) & 65535` per pixel. The JPEG is
a generated gradient with hash in inputs.json. Both sensor runs compile JIT blocks and assert zero
JIT failures. [results.txt](results.txt) preserves serial output and JIT counts.

## Earlier failures and limitations

- The port report at the base records CAMERA:EMPTY for OV2640; OV5640 delivery was not reached.
- First focused-test compile omitted SocBus::new constructor arguments. Corrected the test fixture.
- First repeated-frame unit test advanced a whole period from mid-frame, then crossed another
  VSYNC while checking DMA. Aligning to the next boundary corrected the test; production clock
  behavior was unchanged. The final test repeats the driver start sequence three times.
- The first temporary add-on copy omitted build.rs. Firmware hashes passed with interpreter
  fallback, but JIT compilation failed for lack of the exported table. Rebuilt with the unchanged
  add-on build.rs and repository WASM flags. The final external check now rejects a missing table
  and asserts compiled > 0 and failed == 0; both sensors pass.
- One input-manifest generation command had a Python syntax error; corrected before writing it.

## Evidence retention

inputs.json preserves source, ROM, WASM, JPEG and compiler-segment hashes. results.txt contains
only relevant numeric/serial samples. Local raw build logs and externally supplied binaries remain
outside Git; no raw captures, private backups or personal paths are committed. Commands use
caller-supplied paths, and the manifest keeps segment basenames, offsets, sizes and hashes only.
No measured values were redacted. The standalone firmware source is the existing test sketch,
not a replacement camera driver. Manual text/JSON/script review plus the repository privacy checker
covers the committed evidence. Reproducing the add-on proof requires access to that private
revision; the three generic regression tests run from this repository alone.
