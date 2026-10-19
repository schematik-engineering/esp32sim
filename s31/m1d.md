# S31 M1d: PMA initialization prefix and explicit configuration barrier

Base: ce16eb7b9ce65b87c110f2207931130c8c44ef11. Local/fork-only bounded M1
slice. This is deliberately small progress, not ROM-banner or hello support.

## Sources and implemented transaction

ESP-IDF 9a97f6c54ec638111ce55cd36581b3c192f15207,
components/riscv/include/riscv/csr.h:52-85 defines PMAADDR0 = 0xbd0,
PMAADDR(i) = PMAADDR0+i and the NAPOT address encoding. Thus entry 12 is
0xbdc and (0x2e000000 | ((0x8000 >> 1)-1)) >> 2 = 0x0b800fff.
The LP SRAM aperture is from the same IDF's soc/esp32s31/include/soc/soc.h.

The pinned public rev0 ROM ELF (esp-rom-elfs 20260528, SHA256
 a52ad7513deb656a910a5740125f1cce2c7941f11ce57213b7b43aea93d5ab87)
contains this actual initialization sequence in riscv_pma_init_cfg:

    2f83e832: 0b8017b7  lui a5,0xb801
    2f83e836: 17fd      addi a5,a5,-1
    2f83e838: bdc79073  csrw 0xbdc,a5
    2f83e83c: c0000437  lui s0,0xc0000
    2f83e840: 01940493  addi s1,s0,25
    2f83e844: bcc49073  csrw 0xbcc,s1

Reproduce with riscv32-esp-elf-objdump -d --start-address=0x2f83e82e
--stop-address=0x2f83e848 PUBLIC_ELF. The ROM's check_condCOUNTER.0 at
0x2f83e6d0 subsequently checks both CSRs; it is not intercepted or executed
past the unsupported configuration write in this slice.

The implemented subset is exactly one first CSRRW rd=x0, CSR 0xbdc, with
source value 0x0b800fff. It retains that address in S31-owned optional state,
advances the PC and all three instruction/retirement/cycle counters once,
and preserves the source. No PC-specific dispatch, ROM-function bypass,
unknown-CSR fallback, or alteration to the firmware is used. This is a
partial CSR transaction implementation, NOT a complete PMA model. Reads,
set/clear/immediate forms, other values, neighboring CSRs and rewrites
remain typed Csr stops without mutation. None means unmodeled state, NOT
a silicon reset value. Generic RV32 CSR storage is not used for this entry.

## Why stop at the next configuration write

0xbcc is PMACFG12. The intended value 0xc0000019 has NAPOT, enable, read,
and write bits per csr.h:58-72. Bits alone do not establish the complete
HP-core behavior of overlapping PMA regions, reset enables, priorities,
locks and fault attribution. The preliminary TRM v0.1 omits the HP CPU
chapter; its text has no PMA register description. The pinned IDF
components/esp_hw_support/port/esp32s31/cpu_region_protect.c uses PMA, but
software configuration is not a specification for all those hardware rules.
In particular, do not enable this region merely by storing 0xc0000019 and
then continue with unconstrained memory accesses. No such value is accepted
here. No new reserved-bit defaults are guessed. First-address-write
writability/encoding is evidenced by the ROM's own write/read-check sequence;
no claim of arbitrary PMA state or attribute enforcement is made.

The authorized read-only esptool memory/register commands cannot read
CPU CSRs or execute a PMA access/fault experiment without downloading code.
No port was opened, no hardware state was changed, and no experiment is
invented. A later PMA slice needs a published HP-core specification or a
separately approved CPU-CSR/access experiment before enabling attributes.

## Deterministic execution

Inputs are the unchanged pinned ELF and M1c ROM supplement SHA256
45376d77d7c7d0e9c3359a0d6eb2aa1e369aecb181fee32abc16f93b431c002a.
Both are authenticated by reproduce-m1d.sh before execution:

    S31_ROM=PUBLIC_ELF S31_ROM_DATA=LOCAL_M1C_DUMP CARGO_TARGET_DIR=BUILD_DIR bash s31/reproduce-m1d.sh

Two independent runs produce byte-identical output:

    [s31] chip=esp32s31 pc=0x2f83e844 opcode=0xbcc49073 retired=6334 stop=Csr(3020)

This is three more retired instructions than the base's 6331. The native
ROM executes the address write and following two arithmetic instructions;
it does not retire the PMA enable. No extension opcode was reached, no ISA
extension added, no console FIFO or fabricated banner bytes added, and no
flash/bootloader/app entry is claimed. The inherited reproduce-m1c.sh uses
the updated expected stop on this revision; the historical M1c receipt
continues to describe its own base revision.

## Verification

14 ordinary S31 tests and both explicit external ROM tests pass; seven
Python audit tests pass. Two new unit tests cover mapping, exact stored
value, first-write state, counters/PC/source, unknown reset reads, all six
CSR operations, source x0, rejected values, rewrites, neighbors and the
unretired configuration barrier. Eleven new PMA mutations and all nineteen
inherited mutations are assertion-killed (20 register/CSR, 6 loader,
4 map); every harness checks baseline and exact-source restoration.

    python3 s31/mutate_registers.py --output OUTPUT/registers
    python3 s31/mutate_rom_data.py --output OUTPUT/data
    python3 s31/mutate_map.py --output OUTPUT/map

The exact-SHA handoff records the full retire-common command statuses and
logs: native/wasm Clippy, ROM/plain workspace tests, both empty-HOME variants,
wasm build/eight demos, evidence privacy and isolated non-LTO RV32/C3/C6
object/disassembly comparison against the base. All shared sources and
existing console goldens remain unchanged. No JIT changes, so the JIT-only
gate is not applicable. CPU comparison: PENDING; no timing run is claimed.
