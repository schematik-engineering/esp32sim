# S31 M1f: retained MHCR and the first ROM entropy boundary

Base: 342f47574e9d1cf09a29e444c23d566f9b80276b. Fork/local-only bounded M1 slice.
No shared crate source changes. No banner, second-stage entry or app boot claimed.

## MHCR source decision: case 2, sibling-sourced

The S31 preliminary TRM v0.1, SHA256
 d6a63c70c46a45e9b44239f591e383efd1a82ce3a0bfd8154d709dc25d87f6ef,
has no MHCR definition. The pinned S31 sleep context saves/restores MHCR, but
states no reset value: ESP-IDF 9a97f6c54ec638111ce55cd36581b3c192f15207,
components/esp_hw_support/lowpower/port/esp32s31/sleep_cpu.c:69,87,200.
The ROM's first reached MHCR instruction is a set, not a reset-value declaration.

Use the sibling-sourced reset, zero, from ESP32-C5 TRM v1.1, page 122
(PDF page 122), register 2.97, MHCR at 0x7c1. All fields including reserved fields
are explicitly zero in the reset row. URL:
https://documentation.espressif.com/esp32-c5_technical_reference_manual_en.pdf
SHA256: b2e7944dec0ee8e933042de69eb418da28a959d75afddde660775e9395739240.
This is NOT an independently measured S31 silicon reset value.

Layout compatibility: same pinned IDF components/riscv/include/riscv/rv_utils.h:
52-57 identifies 0x7c1, RS bit4, BFE bit5, BTB bit12 for both S31 and C5;
557-570 sets/clears their union 0x1030. C5 TRM's BPE name corresponds to IDF's
BFE bit5. Source permalink prefix for all IDF citations:
https://github.com/espressif/esp-idf/blob/9a97f6c54ec638111ce55cd36581b3c192f15207/ .

All six Zicsr forms retain the full written value in S31-local state, preserving
source-before-destination aliasing, x0, immediate operands and single retirement.
No unknown-CSR fallback is enabled. Reset is fully known in this sibling-derived
model; unknown-bit reads = 0. No case-3 unknown-bit substitution is used.
M1 limitation: these branch-prediction hints are retained, not timing/microarchitectural
prediction behavior. Reserved-bit WARL behavior is not claimed.

## ROM-touched MMIO controls

All addresses and defaults below are from the same pinned IDF,
components/soc/esp32s31/register/soc/:

- reg_base.h:92,107, trng_reg.h:367-384: TRNG_DATE 0x208140fc resets
  to 2426672, R/W date bits27:0 and CLK_EN bit28, other bits ignored.
- reg_base.h:92, lp_peri_clkrst_reg.h:321-338: RNG_CTRL 0x2071003c,
  clock-enable bit30 reset1, reset-enable bit31 reset0; reserved bits ignored.
  Reset holds modeled sampling configuration at its reset value.
- trng_reg.h:13-72: TRNG_CONF 0x20814000 resets to 0x40000000;
  configuration bits retained except FIFO_RESET bit28 (WT trigger, not storage).
  No FIFO/entropy backing is present. Clock/reset configuration is functional
  state only: no sampling-rate, random distribution, analog noise or latency claim.

Only 32-bit aligned data accesses are supported. Byte/halfword/fetch accesses,
neighbors and entropy/status registers fail closed. Register tests pin defaults,
mappings, retained values, masks, FIFO trigger non-retention and held reset.
No hardware port was opened; no board reads, writes or code downloads occurred.

## Deterministic result and stop rationale

Inputs unchanged: public esp-rom-elfs 20260528 rev0 ELF SHA256
 a52ad7513deb656a910a5740125f1cce2c7941f11ce57213b7b43aea93d5ab87;
M1c ROM-data supplement SHA256
 45376d77d7c7d0e9c3359a0d6eb2aa1e369aecb181fee32abc16f93b431c002a.

    S31_ROM=PUBLIC_ELF S31_ROM_DATA=M1C_DUMP CARGO_TARGET_DIR=BUILD_DIR bash s31/reproduce-m1f.sh

Expected, two independent runs compared byte-for-byte:

    [s31] chip=esp32s31 pc=0x2f80d052 opcode=0x00004064 retired=7049 stop=access address=0x20814044 width=4 write=false fault=Unmapped

The native ROM executes 94 more instructions than M1e (6955 -> 7049). It enables
branch prediction, enters ets_stack_guard_setup, enables the TRNG register clock,
asserts/deasserts its reset, enables sampling and reads CRC random data.

Reproduce disassembly using riscv32-esp-elf-objdump -d
--start-address=0x2f80cfd8 --stop-address=0x2f80d080 PUBLIC_ELF.
trng_reg.h:343-353 names the stopped register TRNG_CRC_DATA (RO random data).
Its documented default zero is NOT a valid deterministic sample after sampling
has been enabled. Neither a single board value nor a fabricated zero/PRNG stream
establishes this run's hardware entropy. Stop explicitly rather than invent data
or bypass stack-canary setup. The slice ends at this semantic boundary within the
three-hour budget, not at an artificial instruction budget or a banner shortcut.

No Zba/Zbb/Zbs/Zcb/Zcmp or other extension instruction is dynamically reached
before this stop. Probe extension wiring remains absent (probe_wired=false).
No console FIFO byte is emitted, and there is no banner claim.

## Verification contract

Three added unit tests cover retained MHCR and control registers. Run:

    python3 s31/mutate_m1f.py --output OUTPUT/m1f
    python3 s31/mutate_registers.py --output OUTPUT/registers
    python3 s31/mutate_rom_data.py --output OUTPUT/data
    python3 s31/mutate_map.py --output OUTPUT/map
    python3 s31/mutate_isa_zb.py --output OUTPUT/zb
    python3 s31/mutate_isa_zc.py --output OUTPUT/zc

Each harness requires passing baseline/restoration and assertion-killed mutants.
M1f mutates each new mapping/reset/retained value, Zicsr arithmetic/accounting,
reset handling, FIFO trigger masking and MHCR dispatch wiring.
Older reproduction scripts and the external ROM test now expect the M1f stop;
older receipts remain historical.

Exact-SHA handoff records native/wasm Clippy, plain/ROM workspace tests, both
empty-HOME variants, WASM build and eight demos, privacy, all mutations and
isolated non-LTO RV32/C3/C6 object plus disassembly comparisons to ce16eb7b.
No JIT code changes, so JIT-only gates are not applicable. Existing goldens unchanged.

## CPU comparison

PENDING. No CPU timing measured. Shared CPU, bus and peripheral crates unchanged.
