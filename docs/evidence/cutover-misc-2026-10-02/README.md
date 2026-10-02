# EX219: Schematik cutover miscellaneous regressions

Base `6e562b863f4a5c94c5055747e8331452adaf2de1`, Schematik integration branch.
The implementation is the commit containing this receipt. Behaviour reference:
`221080ffccfa829106b398f896653535853c76c8`. The private add-on was cloned locally,
checked out at acceptance revision `b0f59f359ca46509f7a1a4c0dbad9066ca6bb7e8`, and
built with Cargo patches pointing its seven emulator dependencies at this checkout.
The original add-on and other worktrees were not modified. No push or GitHub operation.

## Mechanisms and results

EX047 adopted a 256-instruction browser scheduling quantum. Unlike its throughput
experiments, this change tests host calls requesting less than one round. The add-on
already sets `Machine::max_cycles` before `run(u64::MAX)`. Busy execution previously
ignored the remaining cycle budget. The final ordinary round now stops at that
ceiling, and EX133 virtual quanta / EX177 round batches use only whole rounds that
fit before it. Defaults and the scheduling-step budget contract remain unchanged.
No add-on quantum override is needed.

The shift diagnostic requests one cycle repeatedly. S3/C3/C6 now each advance
exactly 1 cycle and report 43 GPIO writes and one latch rise. The frozen fork
advances 64 cycles and reports the same writes and latch. The acceptance baseline
reported 256 cycles and a lost latch. The full unchanged shift-register verifier
passes on all three chips, including exact words, two instances, buffered writes,
and firmware reboot. The frozen fork passes the same firmware and assertions.

The CS1 command path lacked quad PSRAM READ ID and quad read/write opcodes.
The first ID-only candidate still reported zero bytes. Adding the fork's quad
read/write aliases to the existing PSRAM buffer operations makes the unchanged
quad fixture report 2 MiB and pass its 1 MiB patterned allocation and reboot.
The octal fixture reports 8 MiB and passes the same checks. Both fixtures also
pass on the fork. Unit coverage checks absent/unsupported devices, 2/4/8 MiB IDs,
flash ID isolation, octal mode registers, and quad/octal command round trips.

S3 reboot replaced GPIO state without carrying external drivers across reset.
`restore_external` copies only the external mask and levels, resolves those pins,
and clears reset-generated input edges. The S3 reboot path calls it. The unchanged
GPIO fixture passes pull, output readback, external override, CHANGE IRQ, and
reboot checks on candidate and fork. EX214's released-pin and pull behaviour is
retained: a released pin is not restored as a driven input. A focused reboot test
checks both driven levels, a released pin, and fresh pulls/output enable.

The exact cycle ceiling removes 60 trailing instructions from the native seven
second panel SID golden: 398637173 -> 398637113. Its console text and WAV hash
are unchanged. Only its instruction-count pin was updated. No speed claim, timing
calibration, or full 422-case acceptance claim is made.

## Reproduction

Use Rust 1.99.0 via `RUSTUP_TOOLCHAIN=1.99.0`, without changing the default.
The add-on's original `scripts/build-assets.py OUTPUT --assets-source SOURCE`
was used after `cargo update --offline` resolved the local patches. It builds
`cargo build --locked --release --target wasm32-unknown-unknown`, with its original
fat-LTO profile and 256 MiB maximum WASM memory. Its manifest retains the original
upstream revision field; the local patch and the emulator revision above supersede
that field for this build. `receipt.json` identifies the actual resulting bytes.

Acceptance helpers and app verifier files were copied to a disposable replay
folder. Firmware was reconstructed with the original `restore-replay.py` for
`shift-register`, `memory-quad`, `memory-octal`, and `gpio`. Missing shift source
files were copied from the unchanged fixture source, then checked by the original
verifier's source hash. The raw GPIO runner also needed the existing `web/wasm`
JIT support files copied alongside the replacement WASM.

From the replay root:

```sh
node repro-shift.mjs esp32s3
node repro-shift.mjs esp32c3
node repro-shift.mjs esp32c6
```

From its `app` directory:

```sh
node tests/fixtures/esp32sim/shift-register/verify.mjs ../replays/shift-register esp32s3 esp32c3 esp32c6
node tests/fixtures/esp32sim/memory/verify.mjs ../replays/memory-quad/esp32s3
node tests/fixtures/esp32sim/memory/verify.mjs ../replays/memory-octal/esp32s3
node tests/fixtures/esp32sim/gpio/verify.mjs ../replays/gpio ../raw ../assets/roms esp32s3
```

For the frozen fork, add `--loader ../fork-loader.mjs` after `node`, except GPIO,
which takes `../fork-raw` instead of `../raw`. No assertions or firmware changed.

Required gates, from the emulator root:

```sh
export RUSTUP_TOOLCHAIN=1.99.0
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --release --target wasm32-unknown-unknown -p esp32sim-wasm --features jit-tests -- -D warnings
tools/fetch-demo-assets.sh --no-linux
ESP32SIM_ROM_DIR=$PWD/web/wasm/fw cargo test --release --workspace -- --include-ignored --skip external_
tools/wasm-build.sh && node tools/wasm-test.mjs hello c3-hello c6-hello c6-energy-scan c6-contiki c6-contiki-net c6-rpl-net panel
ESP32SIM_VQ_NATIVE=1 cargo test --release -p esp32s3 --test machine busy_runs_honor
node tools/check-evidence-privacy.mjs
```

## Final gates

Both strict Clippy gates pass. The release workspace gate passes 648 tests,
zero failed or ignored, with 51 `external_*` tests filtered. Assets fetched with
`--no-linux`. WASM build and all eight named scenarios pass, including the JIT
handoff check. The directed ceiling regression also passes with native virtual
quanta enabled and confirms that both batching paths execute. Privacy and diff
whitespace checks pass. No workspace or crate formatting command was run.

## Retained negatives and limits

- Initial strict Clippy rejected redundant parentheses in the two batch limits;
  removed them and reran successfully.
- ID-only quad implementation failed initialization; the fork's quad buffer
  read/write aliases were also required. The corrected replay passes.
- First workspace run failed only the panel instruction pin described above.
- Initial replay preparation selected a directory before creating it; recreated
  from the emulator directory. Initial GPIO and full shift verifiers lacked local
  JIT support/source files; copied them and their original checks passed.
- One edit command used the scratch add-on working directory for an emulator
  path and failed before editing. Reissued it in the emulator checkout.
- A process query was sandbox-denied; it was unnecessary and was not retried.
- Single functional runs on Darwin arm64, Node v22.23.1. Concurrent compilation
  makes wall durations unsuitable as benchmark evidence. No external tests ran.
- The add-on remains pinned to acceptance, not newer private changes. Remaining
  cutover failures outside this assignment are not evaluated here.

`receipt.json` retains firmware hashes, revisions, WASM hashes, numeric GPIO
samples and the unchanged audio hash. Private firmware, binary payloads, raw
logs, absolute personal paths and session/process details are omitted from Git.
This limits independent reproduction to holders of the private acceptance inputs;
it does not change measured values. Raw local results remain under
`/private/tmp/esp32sim-fix-misc-results`, and the requested standalone report is
`/tmp/esp32sim-fix-misc-report.md`.

## C3/C6 reset follow-up

Base `8859951ee128f9dc832b1b14110d00fec5dba8c2`. The follow-up acceptance
reported the same lost external HIGH on C3 and C6 after the add-on's GPIO pull
snapshot fixes. Added one `p.gpio.restore_external(&old.gpio)` call to each
peripheral reset, beside the existing strapping-pin restoration. The shared
EX214 resolver is unchanged: only externally driven pins survive as external
drivers; released inputs and guest pulls are not copied across reset.

The private add-on was cloned again, pinned to
`f0caca33f475ab335ac7eb04f3c50bb61f1b1c72`, and built with the same seven local
Cargo patches and unchanged asset builder under Rust 1.99.0. The original
add-on was read-only. This extends the original receipt's add-on scope; all
earlier observations and artifact hashes remain above and in `receipt.json`.

The unchanged GPIO verifier passes on S3, C3 and C6, including pulls, output
readback, external overrides, CHANGE IRQs and HIGH across guest reboot:

```sh
node tests/fixtures/esp32sim/gpio/verify.mjs ../replays/gpio ../raw-f0caca3 /private/tmp/esp32sim-fix-misc-assets-f0caca3/roms esp32s3 esp32c3 esp32c6
```

Run from `/private/tmp/esp32sim-fix-misc-replay/app`. The new add-on clone is
`/private/tmp/esp32sim-fix-misc-addon-f0caca3`; raw WASM and JIT support files
are in the replay's `raw-f0caca3` folder. Follow-up logs are retained under
`/private/tmp/esp32sim-fix-misc-results/followup`. The receipt adds all three
GPIO firmware hashes and the new WASM hash. The reported pre-fix C3/C6 failure
is supplied acceptance evidence, not a new pre-fix measurement in this turn.

Follow-up quality gate: both Rust 1.99.0 strict Clippy commands pass; assets
fetched with `--no-linux`; 648 release workspace tests pass, zero failed or
ignored, 51 external tests filtered. The existing released-pad/pull/interrupt
regression passes. WASM build and all eight named scenarios plus JIT handoff
pass. No failed gate or reproduction attempt occurred in this follow-up.
Privacy and whitespace checks pass before the local commit. No push.
