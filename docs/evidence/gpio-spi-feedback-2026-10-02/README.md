# EX210: same-cycle GPIO feedback and C6 physical SPI

Base: `8cc5d233611d32586bfbb4c4885f6570aa8a64ba` on `schematik-integration`.
Behavior reference: retired fork `221080f`, inspected at `esp32c6/src/bus.rs`
and `esp-soc/src/spi.rs` before implementation. Related experiments are EX201,
EX205 and EX206. This extends their correctness contract to GPIO reads with
zero elapsed cycles and C6 physical SPI attachment; it is not a speed experiment.

## Results

All required checks pass. The final release workspace run has 614 passing tests,
zero failures and 24 `external_*` tests filtered out. Focused tests: 22 passed,
six external tests ignored. The three new external Arduino tests pass when
explicitly supplied their firmware and ROM inputs. Both Clippy commands, the
WASM build, all eight Node workloads and the JIT handoff check pass.

| Chip | Software SPI | Hardware SPI right / wrong / released | Cycles | Instructions |
|---|---|---|---:|---:|
| S3 | `8123a55a` | `a5 / ff / ff` | 240000000 | 7002548 |
| C3 | `8123a55a` | `a5 / ff / ff` | 160000000 | 160000000 |
| C6 | `8123a55a` | `a5 / ff / ff` | 160000000 | 160000000 |

The prior release run before the C6 reset correction passed 613 tests; the
new reset regression brings the final count to 614. No goldens were changed.
Adopted locally for review, with no push or GitHub comment.

## Design and changes

Each chip reuses its tick-time board input delivery at GPIO input reads.
`advance_to(current_cycle)` makes due transitions available, then `take_edges()`
applies them with their original timestamps and existing interrupt handling.
No device tick or extra CPU cycle is introduced. The board retains future edges
until their deadline. Both S3 input banks and C3/C6 subword reads use this path.
The public callback signatures and controller-only boards remain compatible.

C6 opts into `spi_transfer_pins` through the existing `uses_spi_pins` callback.
The decoder follows the existing S3/C3 implementation and the C6 SDK register
headers: SCLK 63, MOSI 65, MISO 64, CS signals 68/101/102/103/104/105.
Native function 2 uses GPIO6/7/2 and CS0..5 on GPIO16..21. Masks retain mirrored
outputs. Enabled low software GPIO selects and active-low hardware selects
are included; inverted, disabled and invalid routes are rejected.
C6 now resets GPIO output selectors to 128, as C3 already does. S3's shared
constructor default 256 sets C6's inversion bit and cannot be used unchanged.

## Checks and reproduction

All Cargo checks use Rust 1.99.0, without changing the default toolchain.
The host is Darwin arm64. No benchmark or electrical timing claim is made.

```sh
tools/fetch-demo-assets.sh --no-linux
cargo +1.99.0 clippy --workspace --all-targets -- -D warnings
cargo +1.99.0 clippy --release --target wasm32-unknown-unknown -p esp32sim-wasm --features jit-tests -- -D warnings
ESP32SIM_ROM_DIR="$PWD/web/wasm/fw" cargo +1.99.0 test --release --workspace -- --include-ignored --skip external_
RUSTUP_TOOLCHAIN=1.99.0 tools/wasm-build.sh
node tools/wasm-test.mjs hello c3-hello c6-hello c6-energy-scan c6-contiki c6-contiki-net c6-rpl-net panel
node tools/check-evidence-privacy.mjs
```

Focused tests: `cargo +1.99.0 test -p esp32s3 -p esp32c3 -p esp32c6 --test gpio_feedback --test pin_transport --test spi_routes`.
The shared feedback test checks the very next read at cycle zero, both input
polarities, IRQ dirty/status, repeated reads, a future transition at cycle 100,
and exact recorded edge timestamps. C6 SPI tests cover matrix/native routes,
all six hardware selects, active-high rejection, software selects, mirrored
outputs, inversion, output enable, invalid MISO pins and the legacy callback.
Existing S3/C3 physical route tests remain in the focused run.

The same `main.cpp` is built unchanged for all three chips with the adjacent
PlatformIO configuration, pinned to pioarduino `55.03.38-1`. Build output reports
Arduino-ESP32 3.3.8 and SPI 3.3.8. The sketch clocks 32 bits using ordinary
`digitalWrite` / `digitalRead`; the board replies on each rising clock at that
same bus cycle. Expected value is `8123a55a`. Hardware SPI then reads `a5` on
CS10, `ff` on wrong CS3, and `ff` with both selects released.

```sh
mkdir -p /tmp/gap-feedback-arduino/src
cp docs/evidence/gpio-spi-feedback-2026-10-02/main.cpp /tmp/gap-feedback-arduino/src/
cp docs/evidence/gpio-spi-feedback-2026-10-02/platformio.ini /tmp/gap-feedback-arduino/
pio run -d /tmp/gap-feedback-arduino
# Use chip=s3/c3/c6 and rom=esp32s3_rev0/esp32c3_rev3/esp32c6_rev0 respectively.
ESP32SIM_FEEDBACK_BUILD="/tmp/gap-feedback-arduino/.pio/build/$chip" \
ESP32SIM_ROM="$PWD/web/wasm/fw/${rom}_rom.elf" \
cargo +1.99.0 test --release -p "esp32$chip" --test gpio_feedback external_ -- --ignored --nocapture
```

## Negative results and limits

- With the three new read hooks disabled, all three feedback tests fail on
  the first read: 32 instead of 0. With C6 routed dispatch disabled, both
  physical-route tests fail. The legacy callback test still passes.
  These are temporary ablations, restored before acceptance checks.
- The initial shared test tried an 8-bit GPIO read on S3. S3 correctly returned
  `Prohibited`; subword tests were limited to C3/C6. S3 word reads still test
  same-cycle feedback. No emulator behavior was changed for this test correction.
- Initial C6 Arduino run: software SPI passed (`8123a55a`) but hardware SPI
  returned `ff/ff/ff`. A diagnostic replay found the correct SCLK/MOSI/MISO
  routes and no CS; register reads showed CS selectors `0x180`, enable `0x4c8`,
  and IO_MUX `0x1202`. Arduino preserves the inversion bit inherited from the
  incorrect S3 reset value `0x100`. Applying C3's existing reset initializer
  (`128`) fixes the root cause. A focused test checks all 31 selectors at
  construction and after reboot. Firmware bytes were unchanged on retry.
- Rust 1.99.0 has no installed rustfmt component. Only new Rust files were
  formatted with the installed stable rustfmt. No crate/workspace format ran.
- This validates a generic bit-banged SPI board response, not the private
  MAX31855/MAX31865 model implementations or complete 1-Wire/DHT protocols.
- SPI phase selection and routing are modeled; signal inversion and electrical
  contention remain unsupported, matching the S3/C3 contract.
- EX211 is reserved for the separate I2C gaps. No I2C behavior changes here.

## Evidence and privacy

`results.json` records check outcomes, firmware/ROM hashes, exact console
assertions and execution counts. Raw build/test logs remain under `/tmp/gap-*`;
they are not committed. The receipt retains only relevant summaries, removing
host account paths and compiler inventories. No private add-on source or report
is copied into Git. Source hashes and input hashes allow replay; no timing
samples or full runtime traces are claimed or needed for this correctness check.
