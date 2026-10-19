# S31 M1-isa1: inventory-bounded Zba/Zbb/Zbs

Base: ce16eb7b9ce65b87c110f2207931130c8c44ef11. Offline, local/fork-only.
No changes to shared crates or probe execution. The only lib.rs change is
`pub mod isa_zb;`. This layer is not yet reached by the ROM probe.

## Scope and sources

Ratified RISC-V Bitmanip specification v1.0.0, RV32 encodings and semantics:
https://github.com/riscv/riscv-bitmanip/releases/tag/1.0.0
Instruction chapters for Zba shift-add, Zbb logical/minmax/count/byte/rotate,
and Zbs single-bit operations. Exact funct7/funct3/opcode and unary rs2 checks
reject RV64 forms, reserved encodings and unimplemented extension instructions.

Implemented exactly the union in m1a-isa.json's linear bootloader/app inventories:

| Extension | Mnemonics | Bootloader sites | Application sites |
|---|---|---:|---:|
| Zba | sh1add, sh2add, sh3add | 8 | 599 |
| Zbb | andn, orn, maxu, minu, ctz, cpop, orc.b, rev8, rori, zext.h | 15 | 62 |
| Zbs | bclr, bclri, bext, bexti, binvi, bset, bseti | 96 | 441 |

20 mnemonics; 119 bootloader + 1102 app sites = 1221 total. Unsupported within
these inventories: zero sites. Other Zb mnemonics (e.g. clz, sext.b, xnor, rol,
register binv) deliberately return None. No F/Zc/vendor instructions added.
These are linear executable-section sites, including unused code and potential
embedded data, NOT dynamically reached instructions or execution frequencies.

## Fixtures and reproduction

Compact test fixtures in esp32s31/tests/fixtures/zb are PC, raw 32-bit word,
objdump mnemonic, taken from the pinned M0 inputs, not assembled substitutes.
Input hashes are checked against audit_isa.PINS, and per-mnemonic counts against
m1a-isa.json. Bootloader code segments are wrapped by the existing audit_isa
recipe with the application's exact ISA attributes; rodata is excluded.

    python3 s31/collect_zb.py --app APP_ELF --bootloader BOOTLOADER_BIN --objdump OBJDUMP --objcopy OBJCOPY --work WORK --out esp32s31/tests/fixtures/zb

Use GNU objdump/objcopy esp-15.2.0_20251204, objdump 2.45 (m1a-isa.json).
APP_ELF SHA256 c16d55efdd65676282af02b39f879a0b7f3ed62ec56ce919c606895a3f82ae12;
BOOTLOADER_BIN SHA256 d38e48ad52800c79ef97613f9ab452296fbcbd731ef51a0f46919978adc9b664.
The fixtures contain instruction words/mnemonics only, not distributable ELFs.

## Semantics and accounting

exec requires cpu.pc to equal the decoded PC; mismatch faults before mutation.
Operands are read before writing rd (including aliases); x0 reads zero and is
hard-wired on write. Shift indices use their low five bits; arithmetic wraps
at RV32 width. PC advances by four, wrapping at the address-space end.
Each success increments insn_count, cycle_count and retired_count once, matching
riscv-rv32/src/exec.rs step_outcome lines 60/66. This is an instruction executor,
not interrupt/wait arbitration. Future wiring must arbitrate those states before
calling it and must not also retire through the shared step for the same word.
No memory access, CSR change, or bus/device tick is performed here.

## Verification

23 ordinary tests: one per mnemonic using a real objdump word and independent
bit-level/u64 reference over edge values (zero, all ones, sign bit, mixed bytes,
shift 0/31 and masked 32/63), destination x0 and rs aliases; whole linear-site
differential; reserved/unsupported encodings; x0-source/PC-wrap/fault atomicity.

    CARGO_TARGET_DIR=BUILD nice -n 10 cargo +1.99.0 test -p esp32s31 --lib isa_zb
    CARGO_TARGET_DIR=BUILD python3 s31/mutate_isa_zb.py --output OUTPUT

Mutation harness checks a baseline, assertion failures rather than compile
errors, restores exact bytes in finally, then checks the restored baseline.
It covers every mnemonic separately: wrong shifts/operand order, signed minmax,
wrong count direction, byte mapping, rotate direction, zero/sign extension,
clear/set/extract/invert errors; plus wrong funct decode and retire accounting
(22 mutants). Named failing tests and source SHA256 are in mutations.json.

The exact-commit handoff records full retire-common commands/results, empty-HOME
variants, isolated RV32/C3/C6 objects/disassembly versus the pinned base, and
reproduce-m1c.sh's unchanged 6331 / 0x2f83e838 / CSR 0xbdc stop. No hardware access,
ROM banner, second-stage execution, or application execution is claimed.

## CPU comparison

PENDING. No timing run: shared execution is untouched and the layer is not wired.
Isolated object/disassembly identity is the structural non-interference gate.
