# S31 M1e: retained PMA CSR file and native ROM progress

Base: 6ac51373eebe1a6391fb80d78fc3d7f0f0f2eec6. Local/fork-only M1 slice.
This supersedes M1d's deliberately narrow address-write special case; M1d's
receipt remains historical. No shared crate sources or extension wiring change.

## PMA contract and sources

ESP-IDF 9a97f6c54ec638111ce55cd36581b3c192f15207,
components/riscv/include/riscv/csr.h:52-85 defines PMACFG(i)=0xbc0+i,
PMAADDR(i)=0xbd0+i and these fields:

- EN bit 0, X bit 2, W bit 3, R bit 4, L bit 29.
- TOR=0x40000000, NA4=0x80000000, NAPOT=0xc0000000.
- Noncacheable bit 27, writethrough bit 26, write-miss-no-allocate bit 25,
  read-miss-no-allocate bit 24; address shift 2.

All sixteen configuration entries and sixteen address entries retain the exact
written value in S31-local optional state, not the generic RV32 CSR fallback.
None means unknown, NOT zero or a silicon reset value. Zicsr 2.0 semantics apply:
RW/RWI suppress the old-value read only for rd=x0; RS/RC/RSI/RCI always read,
including rd=x0, and suppress writes only for rs1/zimm=x0. A nonzero register
index containing zero still attempts a write. Destination/source aliasing uses
the original source. Unknown reads stop without changing CPU or CSR state.
Successful operations update PC, instruction, retirement and cycle counts once.

An observed PMACFG L bit makes subsequent writes to that entry (including its
paired address) a typed Csr stop, not silent lock-bit ignoring. Reads remain
available. Address bit 29 is retained address data, not a lock. This is a
conservative explicit-stop boundary, not a claim to emulate lock priority,
TOR predecessor locking, WARL normalization or all PMA fault rules.

M1 limitation: attributes are RETAINED, NOT ENFORCED. S31Bus's explicit memory
map is the only access authority; unmodeled accesses still stop. No PMA faults
appear in the pinned reference hello_world console, so non-enforcement does not
change the required M1 reference output. This is not a general PMA-correctness
claim or a guarantee for firmware that intentionally triggers PMA faults. No
fabricated console bytes, PC-specific dispatch or ROM function bypass is used.

## Deterministic native ROM result and next boundary

Inputs remain the public esp-rom-elfs 20260528 rev0 ELF, SHA256
 a52ad7513deb656a910a5740125f1cce2c7941f11ce57213b7b43aea93d5ab87,
and the M1c supplement, SHA256
 45376d77d7c7d0e9c3359a0d6eb2aa1e369aecb181fee32abc16f93b431c002a.

Reproduce (both scripts authenticate inputs and compare two independent runs):

    S31_ROM=PUBLIC_ELF S31_ROM_DATA=M1C_DUMP CARGO_TARGET_DIR=BUILD_DIR bash s31/reproduce-m1d.sh
    S31_ROM=PUBLIC_ELF S31_ROM_DATA=M1C_DUMP CARGO_TARGET_DIR=BUILD_DIR bash s31/reproduce-m1c.sh

Expected:

    [s31] chip=esp32s31 pc=0x2f8046a4 opcode=0x7c17a7f3 retired=6955 stop=Csr(1985)

The ROM now executes PMA initialization and its readback checks, advancing beyond
M1d's 6334 retired instructions. At the new boundary main contains:

    2f80469e: 6785      lui a5,0x1
    2f8046a0: 03078793  addi a5,a5,48
    2f8046a4: 7c17a7f3  csrrs a5,0x7c1,a5

Reproduce with riscv32-esp-elf-objdump -d --start-address=0x2f80469e
--stop-address=0x2f8046a8 PUBLIC_ELF. IDF's riscv/include/riscv/rv_utils.h:52-57
identifies MHCR=0x7c1, return-stack bit 4, predictive-jump bit 5 and BTB bit 12;
:557-570 sets/clears their union 0x1030. The S31 sleep_cpu.c:69,87,200 retains
MHCR across sleep. Neither source supplies the old/reset value needed by this
CSRRs. The preliminary TRM has no MHCR definition; its branch-prediction search
hits concern trace encoding, not a CPU reset value. Thus MHCR is deliberately
NOT modeled; no generic zero reset is inferred. Read-only esptool read-mem or
dump-mem cannot read a CPU CSR without downloaded code, which this card forbids.
No device port was opened and no hardware read or write was performed.

No Zb/Zc opcode or second-stage entry was reached. No banner or console FIFO
output is claimed. The bounded slice ends at the unsourced MHCR read, not at
PMA configuration; further progress needs a sourced MHCR old value/contract.

## Verification contract

Four PMA unit tests replace the two narrower M1d tests: all 32 mapping/readback
pairs and values, all six operations, zero sources, aliasing, atomic unknown
reads, locked read-only operations and attempted locked writes. M1d's actual
0xbdc=0x0b800fff then 0xbcc=0xc0000019 sequence stays explicitly tested.

    python3 s31/mutate_registers.py --output OUTPUT/registers
    python3 s31/mutate_rom_data.py --output OUTPUT/data
    python3 s31/mutate_map.py --output OUTPUT/map
    python3 s31/mutate_isa_zb.py --output OUTPUT/zb

The register harness mutates mapping and retained value separately for EACH of
32 CSR addresses, plus Zicsr/locking/counter rules and inherited reset/CLIC
rules. Removed M1d value/form/rewrite rejections are intentionally no longer
rules: generalized storage and forms supersede them. Each harness requires a
passing baseline/restoration and assertion-killed mutations.

Exact-SHA handoff supplies full retire-common statuses and logs (native/wasm
Clippy, plain/ROM workspace tests, empty-HOME variants, wasm build/eight demos,
evidence privacy, two native reproductions and all mutations). Isolated non-LTO
RV32/C3/C6 objects and disassembly are compared against ce16eb7b. JIT-only checks
are not applicable: no JIT edits. Existing goldens stay unchanged.

## CPU comparison

PENDING. No CPU timing run is claimed; S31-local state cannot add shared CPU
fields or shared execution work.
