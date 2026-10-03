# EX199 review revision

The original Arduino input and ROM are identified in README.md. Both runs used
Rust 1.99.0 release builds, native AArch64 JIT, `--chip esp32 --board none --boot rom
--max-insns 242000 --no-dump` and the original merged flash image. The baseline
was core commit 71302a4; the candidate changes only page versions and timer edge
source constants. Both stopped at 242,040 instructions and 242,048 cycles, with
11 exceptions, no interrupts and byte-identical console output through bootloader entry.

Blocks built: 42,642 before, 1,524 after. JIT code: 32,929 KiB before, 1,300 KiB after.
One deterministic block-count sample each; wall time is not used as speed evidence.
The review's 27,817 figure used an unspecified input/stop setup and is not a baseline here.
No existing golden was regenerated. New classic hello_world goldens cover the app.
Local paths are omitted; the input hashes in README.md retain artifact identity.

The host-device port reuses the instruction-position mechanism retained in EX215
at integration revision ffaa4a4. This extends it to LX6; it does not repeat the
S3/C3 timing experiment. The original rejected broad hooks are not reintroduced.
Classic UART tests retain ffaa4a4 and 7ae2af4's APB/AHB and matrix-input cases;
the GPIO test checks instruction-position feedback and host pulls across reboot.

C3 hello used 30 emulated seconds in three alternating before/after pairs,
child user + system CPU time. The clean baseline was a441c69 and the candidate
e2445f7. Before: 2.912639, 2.914933, 2.926169 s. After: 2.921754, 2.928061,
2.925753 s. Median change +0.37%; all runs account for 4,800,000,000 instructions
and identical console hashes. Concurrent validation was uncontrolled, so this
is not a speedup or statistical-equivalence claim. C3 does not attach the new
classic host endpoints, and its default timestamp hooks are empty. Earlier
baseline-only samples are retained in review.json, not substituted for the pairs.

Reproduce after building both revisions with `cargo +1.99.0 build --release
-p esp32sim --bin esp32sim-c3`:

```sh
python3 measure-c3.py WORKTREE C3_ROM_ELF BEFORE_BINARY AFTER_BINARY OUTPUT_JSON
```

The hello fixture uses the repository's CC0 `examples/hello_world/main/hello_world_main.c`,
ESP-IDF 5.5.4 and pioarduino platform 55.03.38-1 (`board = esp32dev`,
`framework = espidf`). Build from a project with that file as `src/main.c` using
`pio run`; commit its bootloader, partition table and app binaries under the
classic-hello names. The three committed input hashes are in review.json. The
model loads the ECO3 mask ROM; the current eFuse revision fields report v2.0.
This fixture adds new goldens only. Binary strings were checked for personal paths.

Mutation checks restored each old bug independently: ungated DFP registers,
each wrong edge-source constant, whole-vector invalidation and the UART output-mux
prerequisite. All five produced a failed assertion, exit 101, and were reverted.
review.json records each exact substitution and test filter.
