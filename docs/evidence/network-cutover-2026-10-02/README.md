# EX218: C3/C6 cutover networking

Base: integration `6e562b863f4a5c94c5055747e8331452adaf2de1`. Behavior
reference: frozen fork `221080ffccfa829106b398f896653535853c76c8`, inspected
with `git show` before editing. This is local Schematik integration work.
No push or GitHub operation was performed.

EX202 proved DHCP and C3 WPA2/AES, and explicitly retained C6 PLL warnings.
EX218 extends that correctness contract to the acceptance firmware's TLS
WebSocket exchange and unstubbed C6 PHY startup. It is not a speed experiment
or hardware timing measurement. No previous SHA DMA/TLS experiment was found.

## Changes and checks

C3 and C6 exposed SHA DMA_START/DMA_CONTINUE but never serviced the requests.
Their buses now call one shared bounded SHA descriptor consumer. It handles
split blocks, continuation, owner return and EOF interrupts. Cycles and unowned
descriptors do not hash fabricated bytes. The existing S3 DMA engine is unchanged.
Six per-chip known-answer/continuation/invalid-chain checks exercise real bus MMIO.

C6's baseband handled only channel switching and IQ estimation. The missing
TX DC and calibration handshakes prevented normal PHY startup. ModemBb now
provides all four fork handshakes with the fork's 80 APB-tick completion and
rearm semantics. The existing peripheral clock/deadline routing handles them.
The Wi-Fi checks cover channel/IQ and repeated TX DC/calibration completion.

After those fixes C6 passed DHCP, HTTP and MQTT but stalled during TLS.
The firmware enables CONFIG_MBEDTLS_HARDWARE_ECC; ECC_MULT was unmapped.
The C6 peripheral now implements the fork's P-192/P-256 scalar multiplication,
point validation, reset and interrupt behavior using existing big-integer
arithmetic. Four checks retain independent known-answer vectors, invalid
points, zero/group-order scalars and unsupported-mode behavior. Unsupported
modes do not report successful completion. No dependency was added.

## Build and reproduction

The input acceptance tree is local/private and is not committed. Set
`ACCEPTANCE` to its root and `ADDON` to a disposable local clone of the add-on.
The original add-on is read-only. Pin the clone to
`b0f59f359ca46509f7a1a4c0dbad9066ca6bb7e8`. In that clone only, add a Cargo
`[patch."https://github.com/schematik-engineering/esp32sim"]` section pointing
esp32s3, esp32c3, esp32c6, esp32, esp-soc, esp-periph and xtensa-lx7 at this
checkout. Generate its lockfile with Rust 1.99.0, then use the unchanged builder:

```sh
RUSTUP_TOOLCHAIN=1.99.0 cargo generate-lockfile --offline --manifest-path "$ADDON/Cargo.toml"
RUSTUP_TOOLCHAIN=1.99.0 python3 "$ADDON/scripts/build-assets.py" "$OUTPUT" --assets-source "$ACCEPTANCE/assets"
python3 "$ACCEPTANCE/restore-replay.py" network
```

The builder uses `cargo build --locked --release --target wasm32-unknown-unknown`,
fat LTO, one codegen unit, opt-level 3, a growable exported table and 256 MiB
maximum memory. The patch resolves all emulator dependencies locally.

Copy the generated assets into a new `app/public/esp32sim/fix-net-pinned/`
directory in the acceptance scratch tree. Set only that copied manifest's
emulatorVersion to `fix-net-pinned`. A Node loader substitutes the same version
in the imported version.mjs; no verifier, production adapter or firmware is
changed. Run from `$ACCEPTANCE/app`, sequentially for C6 then C3:

```sh
ESP32SIM_RELAY_BINARY="$ACCEPTANCE/fork-relay-src/target/release/esp32sim-relay" \
  node --loader "$LOADER" tests/fixtures/esp32sim/network/verify.mjs \
  ../replays/network/esp32c6 --protocols --repeat=1
# Same command with esp32c3. For the frozen control use --loader ../fork-loader.mjs.
```

The unchanged verifier compares all flashing bytes with the compiler receipts.
Results JSON retains firmware hashes and exact assertions. Each protocol round
requires 256 exact MQTT bytes, 256 exact WebSocket bytes and UTF-8 echo,
disconnects, four denied destinations, the guest's two-second timeout, and
serial responsiveness after relay shutdown. HTTP checks require 256 and 65,536
exact response bytes and a 19-byte POST echo.

## Gates and earlier outcomes

Rust 1.99.0 was selected per command; the default toolchain was not changed.

```sh
cargo +1.99.0 clippy --workspace --all-targets -- -D warnings
cargo +1.99.0 clippy --release --target wasm32-unknown-unknown -p esp32sim-wasm --features jit-tests -- -D warnings
tools/fetch-demo-assets.sh --no-linux
ESP32SIM_ROM_DIR=$PWD/web/wasm/fw cargo +1.99.0 test --release --workspace -- --include-ignored --skip external_
RUSTUP_TOOLCHAIN=1.99.0 tools/wasm-build.sh
node tools/wasm-test.mjs hello c3-hello c6-hello c6-energy-scan c6-contiki c6-contiki-net c6-rpl-net panel
node tools/check-evidence-privacy.mjs
```

Both final Clippy gates and the full release test gate pass. No external_* test
or cargo fmt command was run. Final smoke/privacy and acceptance results are
recorded below and in results.json.

Retained failures and intermediate observations:

- The first focused test compilation failed on a ClockDomain import from the
  wrong crate; the attempted esp_periph::clock import also failed. The correct
  emu_core import was verified by the later gates. The first add-on build failed
  on the same import; its retry passed.
- Initial Clippy rejected chunks_exact in the fork-derived test. Using
  as_chunks::<64>() fixed the warning; the retry and final gates passed.
- The first C3/C6 verifier launches rejected the scratch version/manifest
  mismatch before execution. Matching the copied manifest label fixed this.
- With SHA and baseband only, C3 passed all 12 checks; C6 passed DHCP, HTTP and
  MQTT, then timed out during TLS (before ECC was implemented).
- With ECC, an initial C6 run and a frozen-fork control both failed MQTT with
  `NET:MQTT:0:0:0:-4` after passing DHCP/HTTP. This public endpoint failure
  occurred before TLS. It was not hidden by changing the verifier or firmware.
- Initial scratch builds used current add-on `320f29d`; the final build pins
  the original acceptance add-on `b0f59f3`. The only add-on changes in the final
  clone are dependency overrides and its regenerated lockfile.
- C6 PLL warnings occur on the frozen control too. They remain visible; no
  assertion was relaxed or console warning suppressed.

## Limits and retained evidence

One final protocol round per chip, not a 422-case cutover rerun. Public MQTT
and WebSocket availability can vary. PHY timing is the fork's deterministic
approximation, not calibrated RF behavior; ECC completes synchronously.
No performance claim is made from concurrent builds/tests or verifier wall time.

Source, artifact and firmware SHA-256 values and compact check results are in
results.json. Raw task logs remain locally under
`/private/tmp/esp32sim-net-results`; they are not committed. Curated evidence
omits personal paths, transient connection identifiers, full boot serial and
unrelated host details. No private firmware bytes or add-on source are retained
in Git. These omissions do not change the numeric results or firmware hashes.

## Final result

Both pinned-add-on reproductions exit 0 and pass all 12 checks each (24 total).
C3's explicit silent-broker timeout is 2,000 guest milliseconds; C6's is 2,001.
The final release gate passes 657 tests with zero failures and zero ignored;
50 external_* tests are filtered out. Both strict Clippy commands, the WASM
build and all eight requested smoke scenarios pass. Privacy passes over 1,607 tracked evidence files, including 17 gzip files.
The test count is summed from the completed final test output.

Adopted locally on fix-net. Remaining limitations are the fork's PLL warnings,
approximate PHY/ECC timing and public endpoint variability. There is no open
failure in either final requested reproduction. The other acceptance failures
and the complete fixture matrix were outside this task.
