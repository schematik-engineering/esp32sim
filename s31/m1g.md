# S31 M1g: deterministic emulator entropy and native stack-guard setup

Base: 0e5f264189757828f14c069c1aeae47e76c0f724. Local/fork-only bounded M1 slice.
Shared CPU, bus and peripheral sources are unchanged. No banner, bootloader or app boot claimed.

## Entropy model and sources

S31 owns an unchanged `esp_periph::Rng::new()` instance by composition. This is
exactly the shared xorshift32 emulator model already used by C3/C6 for stack-canary
setup, initialized with DEFAULT_SEED 0x2545f491. Each read advances the state and
returns the word plus `now`, wrapping at 32 bits. `now` is the low 32 bits of the
CPU retired-instruction count BEFORE the accessing instruction retires. Probe
refreshes it before executing each ordinary instruction; no shared-crate edit is
needed. Direct bus unit tests supply `now` explicitly (initially zero).

M1 limitation: deterministic emulator entropy, NOT silicon entropy. No analog
noise, CRC/conditioning, sampling latency, FIFO depth or statistical quality claim.
It is not suitable for cryptography. The ROM executes normally, including both
entropy loads, integrity checks, XOR and stack-guard store; no ROM-function bypass
or injected console/golden bytes. The historical M1f receipt's stop-on-entropy
policy is superseded by this explicit emulator-model decision.

All IDF citations below use 9a97f6c54ec638111ce55cd36581b3c192f15207:
https://github.com/espressif/esp-idf/blob/9a97f6c54ec638111ce55cd36581b3c192f15207/ .

- components/soc/esp32s31/register/soc/reg_base.h:107 and trng_reg.h:343-353:
  TRNG_CRC_DATA, 0x20814044, read-only full-width random data. Only aligned 32-bit
  data reads are supported. Writes, narrower reads, instruction fetch and neighbors
  stop explicitly. CRC_SYNC_DATA and SW_READ/SW_READ_ST are NOT reached and remain
  unmapped; no unused handshake is implemented.
- trng_reg.h:66-72,367-384; lp_peri_clkrst_reg.h:321-338: a data read requires
  TRNG_DATE.CLK_EN bit28, RNG_CTRL.CLK_EN bit30, reset bit31 clear and
  TRNG_CONF.SAMPLE_ENABLE bit31 set. Otherwise it is a typed Prohibited access stop
  and consumes no generator state. Existing control reset/masking rules are unchanged.

## Additional ROM-touched controls

Only the following reached subset is modeled; unsupported operations fail closed.
Same IDF components/soc/esp32s31/register/soc/:

- reg_base.h:98, rtc_wdt_reg.h:167-212: SWD_CONFIG 0x2080101c. Reset =
  300<<20 (signal width), reset flag RO bit0 = 0, reset-clear WT bit19 = 0.
  Support the exact read/OR-bit19/write used by clear_super_wdt_reset_flag. Clear
  the flag without retaining the trigger or altering other fields. Other writes
  are typed Prohibited stops, including watchdog feed, disable, width changes,
  reserved bits and writes without the clear trigger. Watchdog timing/protection
  and non-power-on reset causes are NOT implemented. Tests inject an asserted flag
  solely to make clearing observable; there is no production flag injection API.
- reg_base.h:114, cache_reg.h:13-109: L1_ICACHE_CTRL 0x2c000000 and
  L1_DCACHE_CTRL 0x2c000004 reset to zero. Retain HP bus0/1 shutter bits0/1,
  as used by Cache_Mask_All. Any write setting another bit stops Prohibited,
  rather than pretending ROM-bus shutter/debug/DMA semantics are supported.
  Cache-backed flash accesses already fail closed regardless of these shutters;
  uncached SRAM and native mask ROM remain accessible. No flash backing, MMU,
  cache fill/writeback, coherency or cache timing is introduced by these controls.

## Reproduction and stopping boundary

Pinned public esp-rom-elfs 20260528 rev0 ELF SHA256:
 a52ad7513deb656a910a5740125f1cce2c7941f11ce57213b7b43aea93d5ab87.
Unchanged M1c data supplement SHA256:
 45376d77d7c7d0e9c3359a0d6eb2aa1e369aecb181fee32abc16f93b431c002a.

    S31_ROM=PUBLIC_ELF S31_ROM_DATA=M1C_DUMP CARGO_TARGET_DIR=BUILD_DIR bash s31/reproduce-m1g.sh

Two independent native runs are compared byte-for-byte against:

    [s31] chip=esp32s31 pc=0x2f805a5c opcode=0x00004398 retired=7254 stop=access address=0x2070410c width=4 write=false fault=Unmapped

The ROM advances from 7049 to 7254 retired instructions, finishes stack-guard
setup (two generated words), clears the super-WDT flag, masks the two HP cache
buses and enters pmu_enable_unhold_pads. The new boundary is PMU_IMM_PAD_HOLD_ALL:
reg_base.h:87 and pmu_reg.h:2223-2254 identify the address and four WT triggers.
Its pad-hold behavior and WT readback are not implemented by this bounded slice.
The register header's reset zeros alone do not establish a pad-state/trigger model;
no generic zero-read/no-op-write fallback is added. This is an explicit next
peripheral boundary, not an instruction-budget exhaustion or a console shortcut.
No hardware port was opened, and no device memory, flash, eFuse or RAM was written.

Disassembly:

    riscv32-esp-elf-objdump -d --start-address=0x2f80cfd8 --stop-address=0x2f80d120 PUBLIC_ELF
    riscv32-esp-elf-objdump -d --start-address=0x2f805a0e --stop-address=0x2f805a70 PUBLIC_ELF
    riscv32-esp-elf-objdump -d --start-address=0x2f802076 --stop-address=0x2f80208c PUBLIC_ELF

No extension opcode is dynamically reached before this stop; probe_wired=false.
Zb/Zc wiring remains deferred until the first reached extension. No F/Zcf/XespLoop
stop and no lane-B reopening is needed. Console FIFO bytes = 0; no banner claim.
Older reproduction scripts and the external ROM-data stop test expect this stop;
older receipts remain historical. The slice ends within the three-hour bound.

## Verification contract

Eight added Rust tests (one external/ignored) cover independent wide-arithmetic
xorshift32 reference words, wraparound and low32 retired-time folding through real
loads, all clock/reset/sampling gates without state consumption, unsupported
widths/directions/neighbors, SWD defaults/clear-only semantics and both cache
registers' mappings/values/rejection rules. The external test single-steps the
native ROM and checks both entropy results, the XOR stack-guard store and sampling
shutdown before the new stop.

    python3 s31/mutate_m1g.py --output OUTPUT/m1g
    python3 s31/mutate_m1f.py --output OUTPUT/m1f
    python3 s31/mutate_registers.py --output OUTPUT/registers
    python3 s31/mutate_rom_data.py --output OUTPUT/data
    python3 s31/mutate_map.py --output OUTPUT/map
    python3 s31/mutate_isa_zb.py --output OUTPUT/zb
    python3 s31/mutate_isa_zc.py --output OUTPUT/zc

Harnesses require passing baseline/restoration and assertion-killed mutants.
M1g mutations cover the entropy mapping, seed, value, time fold/source/pre-access
ordering, bus wiring, every gate, and each added control mapping/reset/value plus
clear-only and unsupported-field stops. Shared esp-periph code is never mutated.

Exact-SHA handoff records full retire-common native/wasm Clippy, plain/ROM workspace
and both empty-HOME test variants, WASM build and eight demos, privacy, inherited
and new mutations and isolated non-LTO RV32/C3/C6 object/disassembly comparisons
to ce16eb7b. JIT-only checks do not apply (no JIT changes). Existing goldens unchanged.

## CPU comparison

PENDING. No CPU timing measured. Shared CPU, bus and peripheral crates unchanged.
