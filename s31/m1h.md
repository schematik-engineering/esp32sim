# S31 M1h: pad-hold release and native ROM progress to CLIC disable

Base: 3d6641ed29711a420e11623ae2bf8b06c6357b73. Local/fork-only bounded slice.
No shared CPU, bus, peripheral or other-chip source changes. No banner/boot claim.

## Model and sources

All IDF citations use 9a97f6c54ec638111ce55cd36581b3c192f15207:
https://github.com/espressif/esp-idf/blob/9a97f6c54ec638111ce55cd36581b3c192f15207/ .
Paths below are relative to components/soc/esp32s31/register/soc/.

- PMU_IMM_PAD_HOLD_ALL 0x2070410c: reg_base.h:87, pmu_reg.h:2223-2254.
  Every documented field is WT, reset0. Aligned read32 returns zero, including
  after writes. Accept only release bits29/31, OR-record LP/HP release state;
  zero is harmless and repeated releases are idempotent. Tie-high bits28/30 and
  any reserved bit0-27 stop Prohibited without changing state. This implements
  the fixed release-trigger decision, not readable GPIO state. M1 limitation:
  pad hold/isolation has no modeled electrical effect.
- EFUSE_WR_TIM_CONF2 0x207151f8: reg_base.h:96, efuse_reg.h:3564-3583.
  Both fields R/W: TPGM reset240, PWR_OFF_NUM reset480, combined 0x00f001e0.
  Retain TPGM bits31:16; unchanged lower field must remain480 or stop Prohibited.
  ROM ets_efuse_config_wr_tim at 0x2f808182-0x2f8081a8 reads, preserves lower16,
  replaces upper16 with200, then writes 0x00c801e0. This is only timing config;
  no eFuse programming, data, read controller or silicon-content model.
- HP_SYS_CLKRST_SOC_CLK_SEL 0x20587000 and CPU_FREQ_CTRL0 0x20587004:
  reg_base.h:79, hp_sys_clkrst_reg.h:13-52. All documented fields reset0;
  source0 explicitly means XTAL_CLK. Support only reset-state aligned read32;
  all writes stop Prohibited (even zero). These source-backed initial config
  reads are not polled completion status or arbitrary zero-MMIO fallback.
  ROM ets_clk_get_cpu_freq at 0x2f806ba2-0x2f806bcc reads these defaults and
  computes 40 MHz / (0+1) using its own constant; no injected frequency result.
  No CPU/clock timing claim; no PLL, oscillator or clock-update handshake model.

For all four registers, narrower accesses, fetch, unaligned accesses and
unmapped neighbors remain typed stops. No per-tick work is added outside S31.

## Standing ROM-lane rules (fixed)

(a) All reached fields WT/WT-trigger: read0, accept only reached trigger bits,
record the triggers; other bits stop. (b) Header-reset R/W config reached by RMW:
retain at header reset, untouched fields must stay at reset on write.
(c) Polled RO busy/done/valid: completed state, zero latency ONLY when a pinned
source states it, cited. Otherwise stop. Silicon-dependent values (eFuse data,
analog calibration, boot straps) need pinned data or a recorded permitted read;
otherwise stop. No generic unknown read, ROM-function bypass or console injection.

## Reproduction and stopping boundary

    S31_ROM=PUBLIC_ELF S31_ROM_DATA=M1C_DUMP CARGO_TARGET_DIR=BUILD_DIR bash s31/reproduce-m1h.sh

Public esp-rom-elfs 20260528 rev0 ELF SHA256:
a52ad7513deb656a910a5740125f1cce2c7941f11ce57213b7b43aea93d5ab87.
Unchanged M1c dump SHA256:
45376d77d7c7d0e9c3359a0d6eb2aa1e369aecb181fee32abc16f93b431c002a.
Two independent runs compare byte-for-byte against:

    [s31] chip=esp32s31 pc=0x2f8019a4 opcode=0x00050023 retired=7384 stop=access address=0x10801055 width=1 write=true fault=Unmapped

ROM completes both pad-release writes, configures TPGM, computes the default
CPU frequency and advances through interrupt setup to esprv_intc_int_disable.
The stopping instruction is sb zero,0(a0): CLIC external line5, controller index21,
BYTE_CLIC_INT_IE_REG(21) = 0x10801000 + 1 + 21*4 = 0x10801055.
This is a direct byte write, not WT, RMW or a polled completed-state read, so the
standing rules do not authorize its implementation. CLIC byte configuration and
interrupt-delivery semantics need a separately scoped next slice, not a silent
no-op disable. Pinned include/soc/clic_reg.h:14,17-18,99-104,113-129 identifies
this register, R/W enable bit0 default0. No missing reset value requires hardware.
Stop here at the first new semantic category, within the three-hour bound.

Disassemble the public ELF with riscv32-esp-elf-objdump -d using ranges:
0x2f805a54-0x2f805a70 (release), 0x2f808182-0x2f8081aa (eFuse config),
0x2f806ba2-0x2f806c32 (frequency), 0x2f801982-0x2f8019ac (CLIC disable).

No Zb/Zc opcode is dynamically reached; probe_wired=false. No F/Zcf/XespLoop
boundary and no lane-B reopening. Console bytes=0. No device port was opened;
no board read, RAM write, flash or eFuse operation was performed.
Historical receipts keep their original stop; older reproduction scripts and
external tests now consistently expect the new stop.

## Verification contract

Four local unit tests cover release defaults/triggers/persistence, every rejected
bit, atomic rejection, timing reset/retention/untouched fields, clock reset reads
and rejected writes, widths/fetch/unaligned/neighbors. An ignored external test
executes the native ROM and asserts both recorded releases, self-cleared trigger
readback, final TPGM and exact typed stop. Existing stack-guard test still verifies
the ROM's two entropy loads and XOR-store before this boundary.

    python3 s31/mutate_m1h.py --output OUTPUT/m1h

Twenty new mapping/value/rejection/state mutants must be assertion-killed;
M1f restoring harness requires passing baseline/restoration (compile errors do not
count). Also run inherited mutate_registers.py, mutate_rom_data.py, mutate_map.py,
mutate_isa_zb.py, mutate_isa_zc.py, mutate_m1f.py and mutate_m1g.py with --output.

Exact-SHA handoff records full retire-common native/wasm Clippy, plain/ROM workspace
and empty-HOME variants, WASM build/eight demos, privacy, all mutations and isolated
RV32/C3/C6 objects/disassembly vs ce16eb7b. JIT-only checks do not apply (unchanged).
Existing firmware goldens unchanged. CPU timing is deliberately not run here.

## CPU comparison

PENDING. No CPU timing measured; shared crates and other chips unchanged.
