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
