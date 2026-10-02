# EX215: instruction-position GPIO waveforms

Base: `8cc5d233611d32586bfbb4c4885f6570aa8a64ba`. Behavior reference:
`221080ffccfa829106b398f896653535853c76c8`. This extends EX201, EX205 and
EX206 from approximately 100 us pulses to WS2812 bit decoding at 400/800 ns.
EX204 observes steady-state PWM rather than this software-generated waveform.

## Design and changes

The old `gpio_output_at` calls delivered output changes immediately but stamped
them with the scheduling round's bus cycle. Multiple writes could share a timestamp.
C3/C6 have an interpreter, not a JIT. S3 also requires positions within interpreted,
native compiled and WASM compiled blocks, including resumed blocks and chained calls.

`Machine` anchors the active core's instruction count to shared time once per
scheduling batch. CPU stores supply their instruction position. C3/C6 update the
observation clock only on the peripheral branch of a store. S3 compiled memory
fast paths are unchanged; slow stores pass the native budget position or the WASM
retired prefix. This does not tick devices more often, shorten quanta or change the
scheduler's instruction accounting. Existing raw-mask `gpio_output_at` callbacks
receive the resulting time, and their default `gpio_changes` behavior is preserved.

The optional `gpio_waveform_at` callback also exposes matrix and IO_MUX state after
GPIO, routing and mux writes. `Gpio::software_output` resolves a selected software
GPIO pin, output enable and inversion. Boards choose their attached pins; no full
pin scan is added to each write. The retired fork's GPIO pulse decoder and its
boundary tests are ported into the existing `Ws2812Chain`, retaining `from_bits`
and its GRB-to-RGB mapping.

C6 additionally inherited the shared S3 reset route `0x100`, which is the output
inversion bit in C6's narrower matrix. Arduino preserves that bit when selecting
software GPIO. C6 now resets output selectors to `128`, as C3 already does.
The local Arduino 3.3.8 IDF header `soc/esp32c6/register/soc/gpio_reg.h` defines
OUT_SEL bits 7:0, default 128; OUT_INV_SEL bit 8; OEN_SEL bit 9; OEN_INV_SEL bit 10.

## Correctness checks

`cli/tests/gpio_waveform.rs` executes encoded guest store/NOP programs and decodes
three exact RGB values: `[7,11,13]`, `[17,19,23]`, `[29,31,37]`. Its 24 configurations
cover S3 interpreter/native JIT on both cores, C3/C6, and quanta 1, 64, 256 and 1024.
Each checks all 72 high widths, all 145 level observations, monotonic per-pin time,
and equality between the existing raw callback and the routed waveform callback.
A separate C6 test checks reset, routing, inversion and disabled output.

The WASM JIT tests check six compiled store/resume combinations and compare
interpreted and compiled timestamps through repeated chained/resumed dispatches.
Decoder tests preserve invalid-width, disabled-output, low-reset, partial-frame
and long-uptime behavior from the fork.

One unchanged sketch in `firmware/src/main.cpp` is compiled with Arduino-ESP32
3.3.8 / pioarduino `55.03.38-1` for S3 at 240 MHz and C3/C6 at 160 MHz. It uses
`esp_cpu_get_cycle_count`, normal GPIO registers and critical sections. All three
print `WAVEFORM DONE`, produce 145 observations and decode the exact three colours.
Observed first zero/one high widths are S3 104/200 cycles and C3/C6 70/136 cycles.
The loop/read/write overhead explains the difference from the requested 96/192
and 64/128 cycle waits. No firmware or decoder threshold was patched to pass.

The original retained NeoPixelBus C3/C6 flash artifacts also pass unchanged, with
library revision `882b804205f537b12788978c1abb7cd50fefffd4`. Each strip is checked in a
fresh run: UART0 command A produces red/green/blue on GPIO0; E produces the three
colours above on GPIO1. Each run records 289 observations, including setup's black
frame, and the expected `OUTPUT_A:DONE` or `OUTPUT_E:DONE`. The original app adapter
is not modified here; an adapter can now attach its decoder to the generic callback.

Build and external test commands:

```sh
cp -R docs/evidence/gpio-waveform-2026-10-02/firmware /tmp/ex215-firmware
platformio run -d /tmp/ex215-firmware
# Copy each .pio/build/{s3,c3,c6}/firmware.factory.bin into {s3,c3,c6}/.
# For the original fixtures, decode each artifact.json's base64 flash segments at
# their recorded offsets into c3-neopixel/ and c6-neopixel/firmware.factory.bin.
ESP32SIM_WAVEFORM_DIR=/tmp/ex215-firmware \
ESP32SIM_ROM_DIR="$PWD/web/wasm/fw" \
cargo +1.99.0 test --release -p esp32sim --test gpio_waveform -- --include-ignored --nocapture
```

The original private fixture images are retained in `/tmp/gpio-firmware/remaining-outputs/`;
their source/compiler/input hashes are in `receipt.json`. They are an additional
external check, not a CI dependency. The committed three-chip sketch reproduces
the public bit-banging contract without those private inputs.

## Failed attempts and cost qualification

The initial implementation updated the observation clock for all interpreted S3
instructions and RISC-V stores. It passed the native waveform checks but measured panel
1.183860→1.214005 s, +2.55%, and noisy C3 hello 4.497707→4.831526 s, +7.42%.
It was rejected. `initial.patch.gz` preserves that implementation over the base;
`native-initial.json` retains all 24 samples and binary hashes.

The next version moved timestamp work to stores and the RISC-V peripheral branch.
`store-only.patch.gz` preserves that version over the base; `native-store.json` retains its screen: panel +1.35%, C3 -2.71%, C6 -0.49%.
The final refinement passes the already-available block offset to the interpreter
and calculates the absolute position only inside a store, rather than doing a
64-bit addition for every interpreted ALU instruction.

Other retained negatives: the first focused harness used S3/C3 instruction RAM for
C6; choosing C6's mapped RAM fixed the harness. The first Arduino C6 run captured
an inverted waveform and decoded black, exposing the reset-selector bug described
above. The first original-fixture replay sent its command to USB instead of UART0;
changing the host script to `uart0` fixed delivery without changing firmware.
Rust 1.99 Clippy required `as_chunks` in the new test. The WASM benchmark initially
counted four output lines but the existing driver also reports its JIT handoff check;
the harness now verifies all five lines.

The added WASM chained/resumed-store test then exposed a missing compiled-chain
prefix: the 33rd store was stamped 195 instead of 196. The prefix assignment had
landed in the pure interpreted-bridge branch. Moving it to the compiled-chain
branch fixes the source of the error. The earlier WASM timing samples are retained
as `wasm-prefix-incomplete.json`, not used to qualify the corrected implementation.

All required checks pass: native and WASM Clippy with `-D warnings`, 611 release
workspace tests with zero ignored and 23 external tests filtered, all eight requested
WASM smoke scenarios, 111,132 WASM differential cases with 99,685 modules released,
the four focused/external waveform tests, and evidence privacy validation.

Final native median deltas are panel -0.60%, C3 hello -2.34% and noisy C6 hello
-14.87%. The corrected Node/WASM comparison is +0.32%, within observed variation.
These screens did not resolve an ordinary-code slowdown. They do not establish
formal performance equivalence. Numeric summaries are in `validation.json`.
`native-final.json` records balanced ABBA/ABBA samples for five modeled seconds of
panel and thirty seconds each of C3/C6 hello. Each workload preserves the exact
per-core instruction counts and SHA-256 of guest console stdout across all arms.
`bench.py` reproduces these runs. `bench-wasm.py` uses the existing Node smoke driver
against a clean archive build of the base and the candidate, with matched output
checks. Its instruction totals are the driver's rounded million-instruction values,
not an additional exact-instruction oracle; native goldens and the differential
suite provide the exact checks.

Commands for the baseline WASM archive do not create or modify another worktree:

```sh
mkdir -p /tmp/ex215-baseline-src
# Extract the workspace packages, Cargo files and wasm build scripts from 8cc5d23.
RUSTUP_TOOLCHAIN=1.99.0 /tmp/ex215-baseline-src/tools/wasm-build.sh
python3 docs/evidence/gpio-waveform-2026-10-02/bench.py BASE_NATIVE CANDIDATE_NATIVE native.json
python3 docs/evidence/gpio-waveform-2026-10-02/bench-wasm.py BASE_WASM CANDIDATE_WASM wasm.json
```

These are local macOS arm64 wall-time screens, not hardware calibration or a formal
equivalence test. Builds and this task's other tests were finished before each
performance campaign. Uncontrolled host activity remains a limitation, especially
for C3/C6; negative deltas are not claimed as speedups. Node is not a browser.

## Limits and evidence handling

The timing contract is the emulator's default one-instruction/one-cycle model.
The optional approximate CPI/cache-cost modes are not calibrated by this experiment.
Device scheduling remains batched. Concurrent cores writing the same physical pin
are not arbitrated cycle by cycle; the tests give each waveform one owner.
VCD retains its existing scheduler-time contract. Classic ESP32, peripheral-generated
waveform synthesis and electrical contention are outside this change.

The decoder retains its existing permissive 150..1100 ns high envelope, 550 ns
zero/one split and >=50 us reset threshold. It does not enforce revision-specific
maximum low durations. The frequency argument remains explicit.

`receipt.json` retains source, input, compiler and binary identities. Committed
receipts omit tool installation paths, usernames, device identities and raw console
logs. Raw build/test output and local firmware remain under `/tmp/ex215-*` and
`/tmp/gpio-firmware/`. These paths are local retention, not published download links.
No remote action was performed. Redaction removes unrelated provenance details;
it does not change measured values or artifact hashes. The evidence privacy checker
and manual inspection cover the committed text and the preserved patch. Its uncompressed and compressed hashes are both retained.

Required-check commands, all successful:

```sh
export RUSTUP_TOOLCHAIN=1.99.0
tools/fetch-demo-assets.sh --no-linux
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --release --target wasm32-unknown-unknown -p esp32sim-wasm --features jit-tests -- -D warnings
ESP32SIM_ROM_DIR="$PWD/web/wasm/fw" cargo test --release --workspace -- --include-ignored --skip external_
tools/wasm-build.sh
node tools/wasm-test.mjs hello c3-hello c6-hello c6-energy-scan c6-contiki c6-contiki-net c6-rpl-net panel
tools/wasm-jit-test.sh
node tools/check-evidence-privacy.mjs
```
