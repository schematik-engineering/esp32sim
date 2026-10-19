# S31 M1a verification receipt

Base: `f535f3b42293a76ab64c231a0425f35857ab6cf1`. Branch: `s31-m1a`. Offline verification on 2026-10-19. Exact commit SHA is supplied with the review handoff; no upstream publication.

## Required checks

All cargo commands use Rust 1.99.0 and `nice -n 10`, with `CARGO_TARGET_DIR` under the evidence directory. Full native workspace, not just the new crate, was exercised. Commands below are from the worktree root; ROM_DIR is the absolute worktree `web/wasm/fw` directory populated by `tools/fetch-demo-assets.sh --no-linux`.

| Command | Result |
|---|---|
| `cargo +1.99.0 clippy --workspace --all-targets -- -D warnings` | PASS, exit 0 |
| `cargo +1.99.0 clippy --release --target wasm32-unknown-unknown -p esp32sim-wasm --features jit-tests -- -D warnings` | PASS, exit 0 |
| `ESP32SIM_ROM_DIR="$ROM_DIR" cargo +1.99.0 test --release --workspace -- --include-ignored --skip external_` | PASS, 991 tests, 0 failures |
| `cargo +1.99.0 test --release --workspace` | PASS, 964 tests, 51 ignored, 0 failures |
| Empty HOME, ROM_DIR only as firmware input; ROM-enabled workspace command above | PASS, 991 tests, 0 failures |
| Empty HOME, no ESP32SIM_ROM_DIR; plain workspace command above | PASS, 964 tests, 51 ignored, 0 failures |
| `RUSTUP_TOOLCHAIN=1.99.0 tools/wasm-build.sh` | PASS, exit 0 |
| `node tools/wasm-test.mjs hello c3-hello c6-hello c6-energy-scan c6-contiki c6-contiki-net c6-rpl-net panel` | PASS, all eight demos and shared-memory JIT handoff check |
| `node tools/check-evidence-privacy.mjs` | PASS, exit 0; rerun with new files staged |
| `git diff --check` | PASS |
| `git diff --quiet f535f3b4 -- esp32c3 esp32c6 riscv-rv32` | PASS: existing C3/C6 core/SoC sources and tests byte-unchanged |

Empty-HOME runs preserve `RUSTUP_HOME` and `CARGO_HOME` only for toolchain/dependency resolution. No S31 inputs are implicitly discovered: the pinned-ROM test is explicitly external/ignored and requires S31_ROM.

Build-script prerequisite: `tools/wasm-build.sh` copies from literal `target/` despite CARGO_TARGET_DIR. A worktree-local untracked `target` symlink to the external build directory is required. Without it the Rust wasm build succeeds but the copy fails; without the copied artifact the demo command fails ENOENT. With the symlink the complete commands above pass. No build-script source was changed.

`tools/wasm-jit-test.sh` is not applicable: no JIT source changes. CPU comparison remains PENDING; no CPU timing benchmark was run. The new crate does not enter any existing chip's runtime path.

## S31-specific execution and static audit

`bash s31/reproduce.sh` passes with the pinned inputs and compiler paths specified in README.md:

- Seven ordinary S31 Rust unit tests pass; one ignored external test passes explicitly against the actual public rev0 ROM.
- Seven Python audit tests pass (classification, branch/call/jump reachability, AUIPC/JALR, unknown/indirect boundaries).
- Two independent CLI runs are byte-identical and match the expected first-stop line. Six instructions retire; PC 0x2f801724, opcode 0x00032303, read 0x20701030, width 4, unmapped. No banner, second stage, or application execution is claimed.
- Accepted M0 app ELF and bootloader input hashes were also checked directly against the blobs at evidence commit 89bcd093039a7049d4e94aa3380572b410c9dc23, not just the local files.
- Static candidate/linear counts: ROM 12753/89380, second stage 5218/5218, application 21131/38244. Unresolved edges/boundaries: 134/44/137 respectively. These are explicitly NOT dynamic execution counts or a closed control-flow proof.

## Mutation result

Command: `python3 s31/mutate_map.py --output <evidence>/mutations` with the external CARGO_TARGET_DIR. Baseline and restored baseline pass. Each mutant is killed by an assertion in `tests::map_pins_every_aperture_and_boundary`, exit 101, not a compilation failure.

| Mutation | Killed |
|---|---|
| HP SRAM base moved from 0x2f000000 to 0x2f010000 | yes |
| ROM upper bound shortened from 0x2f850000 to 0x2f840000 | yes |
| Flash additive alias offset changed from 0x60000000 to 0x50000000 | yes |
| PSRAM additive alias offset changed from 0x70000000 to 0x60000000 | yes |

[map-mutations.json](map-mutations.json) contains the tested source SHA256 and exact command. Source restoration hash matches the final library source. No existing goldens were changed.

## Limits

This is a strict address-map/reset probe and static inventory only, not completed M1. Cache windows and aliases are classified but deliberately fault until MMU/backing semantics exist; MMIO is not stubbed to zero. Full F/Zc/Zb/Xesp extensions, S31 CSRs, CLIC, dual-core scheduling, ROM console and flash boot remain implementation work. Static reachability cannot establish which optional context or floating-point routines execute on real hardware. No hardware port was opened, and no new upstream API/PR/issue was published.
