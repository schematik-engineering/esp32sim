# ESP32-S31 M1a: offline reset-path slice

Base: upstream `f535f3b42293a76ab64c231a0425f35857ab6cf1`.
Reference: accepted M0 in the separate evidence repository at `89bcd093039a7049d4e94aa3380572b410c9dc23`.
No hardware access, firmware modification, ROM interception, app-entry bypass or publication.

## What this implements

An isolated `esp32s31` crate and `esp32sim-s31 --chip esp32s31` probe, reusing the unmodified C3/C6 RV32 decoder/executor and the shared ELF parser. This is deliberately NOT registration of a supported chip in the general CLI, WASM build or board catalog. It does not advertise S31 firmware compatibility.

The bus declares these half-open regions, from ESP-IDF `9a97f6c54ec638111ce55cd36581b3c192f15207`, `components/soc/esp32s31/include/soc/soc.h:138-217`:

| Region | Range | Implemented behavior |
|---|---|---|
| Mask ROM | 0x2f800000..0x2f850000 | Read-only, only supplied PT_LOAD file bytes present |
| HP SRAM | 0x2f000000..0x2f080000 | Unified instruction/data, zero-initialized functional RAM |
| LP SRAM | 0x2e000000..0x2e008000 | Functional RAM, no LP CPU |
| Flash cache aperture | 0x40000000..0x44000000 | Classified, access faults: no MMU/controller/backing yet |
| PSRAM cache aperture | 0x50000000..0x54000000 | Classified, access faults: no MMU/controller/backing yet |
| Flash non-cacheable alias | 0xa0000000..0xa4000000 | Same aperture offsets; access faults |
| PSRAM non-cacheable alias | 0xc0000000..0xc4000000 | Same aperture offsets; access faults |
| CPU subsystem | 0x10000000..0x20000000 | Unimplemented MMIO, not zero-filled register RAM |
| On-chip peripherals (includes CPU peripheral aperture 0x2c000000..0x2c020000) | 0x20100000..0x2e000000 | Unimplemented MMIO, not a claim every address is a register |

Non-cacheable constants are ADDITIVE offsets: flash +0x60000000, PSRAM +0x70000000. They are not alias bases. Also checked against pinned `components/esp_hw_support/port/esp32s31/cpu_region_protect.c:90-101`. There is no flat identity mapping of flash/PSRAM and no cache/MMU-visibility claim. Boundary tests check offsets, exclusive ends, RAM access widths and rejected cross-boundary writes.

The loader requires RISC-V ELF32 with entry 0x2f800000. It ignores host metadata at virtual zero and empty segments, rejects non-ROM/overflowing load segments, preserves holes, and does not preinitialize SRAM from ELF sections. The ROM must execute its own unpack/clear loops later. A failed load leaves the previous ROM image intact.

Only HP core 0 is instantiated. `mstatus` and `mhartid` suffice for the observed reset prefix; any other CSR is explicitly stopped rather than inheriting the existing generic unknown-CSR readback. CLIC, Sv32, second core, FPU, Zc/Zb additions, vendor loops/vector state, peripherals, clocks and reset-cause semantics are not implemented.

## Reproduce

Supply the accepted M0 artifact directory and the public ROM (not redistributed here). Hashes are enforced by the audit and recorded in [m1a-isa.md](m1a-isa.md). ROM release: https://github.com/espressif/esp-rom-elfs/releases/tag/20260528 ; archive SHA256 `caa463d3cbef2430a5a35847c1d9f2f152403b17a802050927ff60c8da54fe46`.

From the worktree root, with caller-selected absolute paths:

    export S31_ROM=<esp-rom-elfs-20260528>/esp32s31_rev0_rom.elf
    export S31_REF_DIR=<accepted-M0>/s31
    export RISCV_BIN=<riscv32-esp-elf-esp-15.2.0_20251204>/riscv32-esp-elf/bin
    export CARGO_TARGET_DIR=<notes>/retire/t_6f9e8459/target
    bash s31/reproduce.sh

This builds the actual executable, runs the unit tests and pinned-ROM external test, compares two independent CLI runs with the exact expected stop, runs seven static-audit regressions and regenerates both inventory files. To obtain accepted M0 inputs from the evidence repository, extract `git archive 89bcd093039a7049d4e94aa3380572b410c9dc23 s31` into a fresh caller-selected directory. The ISA audit hashes ensure an accidental rebuild or stale input is rejected.

Standalone deterministic invocation:

    nice -n 10 cargo +1.99.0 run --release -p esp32s31 --bin esp32sim-s31 -- --chip esp32s31 --rom "$S31_ROM" --max-instructions 1000

Observed twice, byte-identical:

    [s31] chip=esp32s31 pc=0x2f801724 opcode=0x00032303 retired=6 stop=access address=0x20701030 width=4 write=false fault=Unmapped

The access is `lw t1,0(t1)` in `_init`, before stack setup and before the ROM banner. The pinned `components/soc/esp32s31/register/soc/reg_base.h:84` names 0x20701000 as LP_AONCLKRST. This is offset 0x30 in that aperture; this slice does not infer its bit/reset semantics. The CLI exits zero when it reports a probe result (including unsupported/budget stops); invalid arguments or ROM inputs exit nonzero. Consumers must inspect the stop reason, not treat exit zero as a successful boot.

## ISA measurement limits

[m1a-isa.md](m1a-isa.md) distinguishes three denominators:

1. Six actually retired reset-prefix instructions, plus the first faulting load attempt.
2. Static candidate addresses reached from explicit reset/banner/flash-boot/second-stage/application/task roots, traversing both branch arms and direct calls.
3. Full executable-section linear inventory, including unused routines, context-save code and possible data/padding.

The raw instruction words are checked through the actual unchanged C3/C6 decoder. Counts are unique static sites, NOT dynamic frequencies. Indirect transfers and scheduler/interrupt reachability remain unresolved, so these are not a complete minimum-executed-ISA set. F, Zcf, vendor vector and loop-CSR code is present in full images but is not asserted executed on hello boot. The second-stage entry itself is `cm.push {ra},-64` (0xb84e), rejected by the current decoder. Zcmp/Zcb and Zba/Zbb/Zbs therefore have concrete static sites to address in M1; CSR encoding acceptance is not evidence of correct S31 CSR semantics.

Raw bootloader disassembly without ELF ISA attributes misreads Zcmp as D/RV64 instructions. The audit reconstructs an analysis-only ELF from the exact two boot code segments and copies the pinned application ISA attributes; it does not execute or alter those bytes. The rodata segment is excluded. The full ROM and reference firmware are not copied into this branch.

## Map mutations

After other builds/checks in this worktree have finished:

    python3 s31/mutate_map.py --output <notes>/retire/t_6f9e8459/mutations

The script checks baseline, alters one rule at a time, requires an assertion failure in `tests::map_pins_every_aperture_and_boundary` (not a build failure), restores the exact source in `finally`, and checks the restored baseline. Mutations: wrong HP SRAM base, shortened ROM aperture, wrong flash alias offset, wrong PSRAM alias offset. Results and source hash are retained in the receipt.

## CPU comparison

PENDING. No CPU benchmark run. Existing chip hot paths, decoder, scheduler, CLI dispatch and goldens are unchanged; the new probe is a separate binary/crate and adds no fields, branches or per-tick work to C3/C6.
