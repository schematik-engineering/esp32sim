# EX222: camera host settings across guest reboot

Base: upstream/main `017af524`. The final source is the commit containing this receipt.
Related EX212 implements camera capture. This change tests a different lifecycle
contract: a guest reboot must not replace host-selected frame cadence with 10 fps.

The S3 reboot path restores frame_cycles and reapplies stored debug flags through
the same dispatcher used by set_debug. This also preserves other devices' log
flags and MMIO logging. The separate log_unknown copy stays because the dispatcher
does not cover it.
Guest registers, counters, pending frame and capture activity still reset.
The board's picture and Machine's input queue already survive; they are reused.
No new state, per-tick work, helper in a hot path or input channel is introduced.

The regression uses the real waveshare-cam board and Machine::reboot once,
with a period corresponding to 25 fps. It checks the retained period,
the actual frame_due boundary, logging set through Machine::set_debug, cleared capture state,
picture dimensions/content, queue identity and delivery of a post-reboot frame.
Pictures are synthetic two-pixel RGB arrays committed in the test. No firmware
fixture or golden was added or regenerated. The CLI assigns --cam-fps to
frame_cycles; --cam-size configures the persistent stream reader, and both
--cam-image and streamed frames use the persistent board picture.

No guest firmware reboot fixture or hardware timing was measured. This tests
the shared reset entry point used by software/watchdog resets. No changes to
LCD_CAM register meanings, guest sensor programming or DMA paths.

## CPU comparison

PENDING

## Mutation table

Command for each mutation:
`cargo +1.99.0 test --release -p esp32s3 --test machine reboot_preserves_camera_host_settings_and_stream_but_resets_capture`.
Each mutation compiled and failed that regression test. Restore between runs.

| Mutation | Assertion that fails |
| --- | --- |
| Remove frame_cycles restoration | Configured period survives reboot |
| Remove debug dispatch | Host LCD_CAM logging stays enabled |
| Remove MMIO logging restoration | MMIO logging stays enabled |
| Remove log_unknown restoration | Unknown-register logging stays enabled |
| Copy all of old.lcd_cam instead of restoring only host fields | Capture counters and pending state reset |

The unmutated focused test passes. No private captures or machine identifiers
are retained; synthetic RGB input arrays are in the test source.

## Verification

Darwin arm64, cargo 1.99.0, Node v22.23.1. Fetch ROMs and demos with
`tools/fetch-demo-assets.sh --no-linux`; [input SHA-256 hashes](inputs.json)
identify the downloaded public assets.

Workspace checks use an empty HOME, retaining the installed CARGO_HOME and
RUSTUP_HOME only for the toolchain/cache. Remove all ESP32SIM_* variables first.
For CI policy only, set `ESP32SIM_ROM_DIR="$PWD/web/wasm/fw"`.
The CI-policy release workspace run passes 642 tests. The plain release workspace
run with no ESP32SIM_* variables passes 621, with 35 ignored. Both Clippy checks
pass with warnings denied. Existing golden files are byte-identical to the base.
No JIT code changed.

All eight production WASM demos and the evidence privacy check pass.
[Exact commands and exit codes](checks.json); [additional CI Node/Python checks](extra-checks.json).
