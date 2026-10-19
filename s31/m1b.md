# S31 M1b: reset prefix through the first ROM data hole

Base: `9e2bc8a406e6e9aa7693a740cd34f58dc7b8ddd2` (M1a).
Offline only; no device access, publication, direct application entry, printf
interception, synthesized ROM contents or instruction skipping.

## Observed execution

The pinned public rev0 ROM SHA256 is
`a52ad7513deb656a910a5740125f1cce2c7941f11ce57213b7b43aea93d5ab87`,
from esp-rom-elfs release `20260528`. Two independent runs with a 1,000-instruction
budget produce the identical first-stop line:

    [s31] chip=esp32s31 pc=0x2f801812 opcode=0x0007a283 retired=44 stop=access address=0x2f83f700 width=4 write=false fault=Unmapped

M1a retired six instructions before the reset-cause read. M1b executes that read,
selects normal boot, initializes mcounteren, reads/writes CLIC configuration,
stores mtvec and mstatus, sets the stack, and enters the ROM's data-copy loop.
The first attempted source load in that loop is `lw t0,0(a5)` at 0x2f801812.
No banner, second-stage entry or application execution is reached.

The ELF's executable PT_LOAD starts at 0x2f800000 and has file/memory size
0x3f700: its exclusive end is exactly 0x2f83f700. Subsequent zero-file-size
segments have physical addresses starting there but virtual address zero and no
bytes. The next nonempty ROM segment starts at 0x2f848000 (size 0x6408).
Thus the demanded source word is absent from this ELF; the hole is not evidence
that actual silicon ROM reads zero. Filling it with zeros, copying host metadata
or initializing RAM directly would conceal the input limitation. This slice
stops explicitly. A complete ROM data source is a prerequisite for continuing
this exact native boot path, not another inferred register value.

## Audited state and limits

All target header references below use ESP-IDF commit
`9a97f6c54ec638111ce55cd36581b3c192f15207`, the snapshot used by the M0 IDF
reference, not a released v6.1 tag. Source root:
https://github.com/espressif/esp-idf/tree/9a97f6c54ec638111ce55cd36581b3c192f15207
The preliminary S31 TRM does not publish all boot/CPU chapters; these are header
semantics, not hardware differential measurements.

| Register | Source | Model |
|---|---|---|
| 0x20701030, HPCORE0_RESET_CAUSE | `components/soc/esp32s31/register/soc/reg_base.h:84`; `lp_clkrst_reg.h:291-339` | Cause [6:1], RO; explicitly choose POR cause 1, so initial word is 2. Flag [0] defaults to 0. Write triggers [30]/[31] clear cause/flag independently. Other write bits ignored. |
| 0x10800000, CLIC configuration | `components/soc/esp32s31/include/soc/clic_reg.h:17-38` | NVBITS [0] RO 1; NMBITS [6:5] RO 0; NLBITS [4:1] RW reset 0, maximum 8. Initial word 1; ROM writes 3. Values above eight explicitly fault because the header does not define WARL behavior. No interrupt delivery. |

POR cause 1 is a selected boot condition, NOT the register's documented reset
field default (zero), and not the USB-reset cause 0x17 captured in M0. No reset
cycling, watchdog or deep-sleep behavior is implemented. Only aligned 32-bit
accesses to the two registers are accepted; unsupported widths, neighbors and
instruction fetches fail closed. Reserved write bits are not retained.

The probe additionally allows existing Zicsr execution for mcounteren (0x306)
and mtvec (0x305). mcounteren retains only standard CY/TM/IR bits [2:0]; no
lower-privilege counter access is implemented. This is a deliberately limited
standard-CSR storage model, not a claim about every S31 HPM counter bit. mtvec
retains the ROM's base 0x2f801700/mode 1 via the unmodified RV32 executor; no
CLIC vectoring is claimed. Unknown CSRs still stop before retiring. All six
Zicsr forms have explicit old-value/new-value cases including reserved-bit
masking, and mtvec has a storage case. No new ISA decoding or extension was
needed on the 44-instruction path; RV32/C3/C6 sources are unchanged.

## Reproduction and assertion mutations

    export S31_ROM=<esp-rom-elfs-20260528>/esp32s31_rev0_rom.elf
    export CARGO_TARGET_DIR=<notes>/retire/<job>/target
    bash s31/reproduce-m1b.sh
    python3 s31/mutate_registers.py --output <notes>/retire/<job>/register-mutations
    python3 s31/mutate_map.py --output <notes>/retire/<job>/map-mutations

The first script checks the ROM hash, builds/runs the real executable, executes
11 ordinary tests plus the explicit pinned-ROM external test, compares two
independent stop outputs and runs seven static-audit regressions. It preserves
the historical M1a inventory rather than overwriting it with a new dynamic count.
The register mutations move each register mapping, remove POR cause, independently
remove both reset clears, remove CLIC's RO bit, truncate the level mask, remove
the counter mask, and allow an unknown CSR. Each must fail a named test assertion,
not compilation. Baseline/restored baseline must pass and the exact source bytes
are restored in finally. The original four aperture/alias mutations remain.

Full retire-common verification (native/WASM Clippy, ROM-enabled/plain and
empty-HOME workspace suites, eight WASM demos, privacy) and isolated emitted-code
comparison against M1a belong in the exact-SHA handoff receipt. No JIT code
changed; wasm-jit-test is not applicable. Existing goldens must remain unchanged.

## CPU comparison

PENDING. No timing benchmark. All changes are in the isolated S31 probe and its
evidence/scripts; existing chip runtime paths are not modified. Emitted-code
identity is a compatibility check, not a CPU speed measurement. Full M1 remains
incomplete (ROM banner, flash/MMU, CLIC delivery, dual-core boot, hello/reboot).
