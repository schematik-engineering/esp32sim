# S31 M1-isa2: inventory-bounded Zcb/Zcmp

Base: 2e89bd8a274ef4b70d4ab4caabe9963531e311bd (M1-isa1).
Local/fork-only, no board access. Shared crates and Probe::run are untouched;
the only lib.rs change is `pub mod isa_zc;`. This is an unwired execution layer,
not evidence that the second stage or application boots.

## Sources and scope

Ratified RISC-V Zc v1.0, sections 2.1-2.12 (Zcb), 3.6-3.8 (RV32 push/pop
examples, encodings, UABI lists, stack adjustments and pseudocode), 3.11-3.12
(saved-register pair moves):
https://github.com/riscvarchive/riscv-code-size-reduction/releases/tag/v1.0
https://github.com/riscvarchive/riscv-code-size-reduction/releases/download/v1.0/Zc.pdf

Exactly the mnemonic union in m1a-isa.json's linear second-stage/app inventories:

| Mnemonic | Second stage | Application |
|---|---:|---:|
| c.lbu | 11 | 62 |
| c.lhu | 2 | 8 |
| c.lh | 0 | 10 |
| c.sb | 5 | 30 |
| c.sh | 1 | 5 |
| c.zext.b | 18 | 60 |
| c.sext.b | 0 | 1 |
| c.zext.h | 1 | 22 |
| c.sext.h | 0 | 1 |
| c.not | 0 | 2 |
| c.mul | 9 | 15 |
| cm.push | 75 | 752 |
| cm.pop | 69 | 738 |
| cm.mvsa01 | 5 | 1 |
| cm.mva01s | 7 | 1 |
| Total | 203 | 1708 |

15 mnemonics, 1911 sites, zero unsupported Zcb/Zcmp sites in these inventories.
Zcb: 47/216, Zcmp: 156/1492 second-stage/application sites. These are linear
executable-section addresses, including unused code and potential embedded data,
NOT dynamic frequencies or a complete execution-path inventory.

cm.popret and cm.popretz have zero sites in both inventories. To honor the
inventory-only rule, they return None, rather than accidentally decoding as pop.
The spec defines popret as pop plus ret (to restored ra with JALR bit 0 cleared),
and popretz additionally zeroes a0. Neither return operation is implemented here.
The rejected-return decode and the rule that pop only advances PC are mutation
tested. Zcmt, RV64 c.zext.w, reserved rlist 0..3, equal-destination mvsa01 and
reserved Zcb forms likewise return None. No operand accepts x0: compact Zcb uses
x8-x15; pair moves use s0-s7; stack operations use ra/s0-s11.

## Fixture recipe

PC / raw halfword / objdump mnemonic fixtures in esp32s31/tests/fixtures/zc are
collected from the same M0 inputs as isa1, with no synthetic replacement words.
The collector checks both input SHA256 pins and every per-stage mnemonic count
against m1a-isa.json. Code segments in the ESP second-stage image are wrapped
using audit_isa.boot_elf with the application's ISA attributes; rodata excluded.

    python3 s31/collect_zc.py --app APP_ELF --bootloader BOOTLOADER_BIN --objdump OBJDUMP --objcopy OBJCOPY --work WORK --out esp32s31/tests/fixtures/zc

GNU esp-15.2.0_20251204 objdump/objcopy (objdump 2.45).
APP_ELF SHA256 c16d55efdd65676282af02b39f879a0b7f3ed62ec56ce919c606895a3f82ae12;
BOOTLOADER_BIN SHA256 d38e48ad52800c79ef97613f9ab452296fbcbd731ef51a0f46919978adc9b664.
At 0x2f06a2ba the exact bytes 4e b8 decode as cm.push {ra},-64 (rlist=4,
spimm=3). Fixtures hold instruction words only, not redistributable firmware ELFs.

## Execution and fault contract

Zcb byte uimm has encoding bit 5 -> offset bit 1 and bit 6 -> offset bit 0.
Halfword offset is 0 or 2. Loads use signed/unsigned widening as specified;
stores truncate to their width. Multiply returns the low RV32 word.

The UABI list is ra,s0,s1,s2,...,s11 (x1,x8,x9,x18,...,x27). rlist 4..14
selects 1..11 entries; rlist=15 selects all 13, never just ra through s10.
The highest numbered selected register goes at old_sp-4, then descending registers
at successively lower addresses. Pop reads from old_sp+stack_adj-4 downward.
stack_adj_base is register storage rounded UP to a multiple of 16 bytes:
16 for rlist 4..7, 32 for 8..11, 48 for 12..14, 64 for 15. Add spimm*16.
This preserves an aligned stack pointer; it does not silently round an unaligned
incoming pointer. Alignment faults, if any, are the bus's responsibility.

All reads complete before pop commits any register, including ra and sp. On a
mid-sequence fault all CPU registers, PC and counters remain unchanged. Push
stores already completed before a later fault remain in memory; retry repeats
them. Memory read/device side effects cannot be rolled back. Accesses follow
the spec's descending-register pseudocode; this is not an atomic memory group.
ExtFault::Memory preserves failing address, width, read/write and bus Fault.
Pair moves read original sources and commit both destinations together.

PC mismatch faults before any bus/register effect. Each successful instruction
advances PC by two with RV32 wrap, hard-wires x0 and increments insn_count,
cycle_count and retired_count once. Caller owns interrupt/wait arbitration and
bus/device ticking; it must not also retire this instruction via shared step.
No CSR or trap delivery is added, and fault attempts do not retire.

## Verification

21 ordinary tests: a real fixture word for every mnemonic, independent expected
execution values, all compact-register combinations/aliases, signed edges,
all rlist 4..15 (every image size), all spimm 0..3, full linear-site differential,
26 push/pop fault-position cases (including every mid-sequence position), Zcb
faults, mismatch/no access, PC/address wrap and stack granularity. Independent
specification-field construction checks every 16-bit word, both with zero and
nonzero upper fetched bytes (131072 decoder comparisons).

    CARGO_TARGET_DIR=BUILD nice -n 10 cargo +1.99.0 test -p esp32s31 --lib isa_zc::tests::
    CARGO_TARGET_DIR=BUILD python3 s31/mutate_isa_zc.py --output OUTPUT
    CARGO_TARGET_DIR=BUILD python3 s31/mutate_isa_zb.py --output OUTPUT_ZB

Zc mutation harness: baseline / 22 assertion-killed mutants / exact-byte restore
in finally / restored baseline. Mutants cover rlist order/15, rounded stack
base, spimm, pop vs return decode/execution, signed/unsigned loads and unary
extensions, not/multiply, pair-move direction, store operands, byte offset,
partial pop writeback and retirement. Names of failing assertions are recorded
in mutations.json; compilation failures do not count as kills. isa1's 22 mutants
and the prior map/register/ROM-data mutations are also part of the handoff.

The exact-SHA handoff retains all retire-common command logs/results, plain
and ROM workspace suites, empty-HOME variants, native/wasm clippy, wasm build
and eight demos, privacy, deterministic M1c reproduction and isolated RV32/C3/C6
object/disassembly comparison against ce16eb7b9ce65b87c110f2207931130c8c44ef11.
The pre-existing probe stop remains the expected 6331 / 0x2f83e838 / CSR 0xbdc.
No hardware observation, boot advancement or timing improvement is claimed.

## CPU comparison

PENDING. No timing run; the unwired layer adds no shared execution work.
Isolated object/disassembly byte identity is the structural non-interference gate.
