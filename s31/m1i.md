# S31 M1i: CLIC register storage and native ROM UART setup

Base: d17667987207cff3190793ba93a7fd12604b9445. Bounded local/fork-only slice.
No shared-crate or other-chip source changes. No banner, boot or delivery claim.

## Model and sources

All IDF citations use 9a97f6c54ec638111ce55cd36581b3c192f15207:
https://github.com/espressif/esp-idf/blob/9a97f6c54ec638111ce55cd36581b3c192f15207/ .
Paths below are relative to components/soc/esp32s31/.

- CLIC entries at 0x10801000 + 4*i, i < 48: include/soc/clic_reg.h:14-18,
  40-60,69-160. Each entry is IP, IE, ATTR, CTL in byte address order; aligned
  read/write32 is the little-endian composite of the SAME bytes. Reset word
  0x1fc00000 (IP0, IE0, ATTR0xc0, CTL0x1f). The byte MODE comment at147 says
  default00, but the description at148 says fixed11 and the word comment at78
  says default11. Fixed decision3 resolves the conflict to machine-only11.
  CTLBITS=3 (:41-46), CTL R/W bits7:5 (:154-160); low five bits read1, as in
  the CLIC clicintctl unimplemented-low-bit rule and word reset0x1f (:70).
  Low CTL bits written0 are accepted and ignored; this does not request a
  priority below the hardwired bits. Byte and word writes keep IE bit0 and
  ATTR SHV/TRIG bits2:0. Reserved IE7:1, ATTR5:3, MODE!=11 and nonzero IP
  stop Prohibited; pending assertion has no delivery semantics in this slice.
  Rejected words validate all lanes before committing any lane.
  Index48+, dual-core alias at+0x10000, 16-bit, unaligned32 and fetch stop.
  CLIC_INFO/THRESH and CLIC CSRs remain unmodelled: none is reached.
- HP_SYS_CLKRST_UART0_CTRL0 0x20587088: register/soc/reg_base.h:79,
  hp_sys_clkrst_reg.h:1369-1442. Header reset0x83. Retain reached R/W
  clock enables/reset fields bits0-3,7; untouched force/source/divider fields
  stay at reset0, otherwise Prohibited. Native uart_hal_init_reset performs
  the RMW sequence at 0x2f83f1a0-0x2f83f1e2. This is clock/reset configuration,
  not a clock-frequency or reset-latency model; there is no UART FIFO yet.
- HP_SYSTEM_UART0_MEM_LP_CTRL 0x2058628c: reg_base.h:78,
  hp_system_reg.h:1741-1769. All fields reset0. Retain reached LP_EN bit2;
  untouched mode bits1:0, force bit3 and reserved bits must remain0 or stop.
  ROM reads/clears LP_EN at 0x2f83f1b8-0x2f83f1be. No low-power timing model.

The two UART configuration registers implement standing rule(b): R/W header
reset with ROM RMW, preserving untouched fields at their reset. They do not
map the UART peripheral itself. Only aligned data32 accesses are supported;
other widths, fetch, unaligned and neighboring registers remain typed stops.

M1 limitation: CLIC registers stored, no interrupt delivery. No interrupt source
can assert; IE1 with mstatus.MIE enabled remains storage. No mintstatus change,
mtvt read, vector dispatch, secondary core, systimer or interrupt arbitration.
Delivery belongs to a later systimer slice. No generic unknown-MMIO readback.

## Reproduction and new boundary

    S31_ROM=PUBLIC_ELF S31_ROM_DATA=M1C_DUMP CARGO_TARGET_DIR=BUILD_DIR bash s31/reproduce-m1i.sh

Public esp-rom-elfs 20260528 rev0 ELF SHA256:
a52ad7513deb656a910a5740125f1cce2c7941f11ce57213b7b43aea93d5ab87.
Unchanged M1c dump SHA256:
45376d77d7c7d0e9c3359a0d6eb2aa1e369aecb181fee32abc16f93b431c002a.
Two independent runs compare byte-for-byte to:

    [s31] chip=esp32s31 pc=0x2f812022 opcode=0x00004310 retired=7463 stop=access address=0x205820e8 width=4 write=false fault=Unmapped

The stopping instruction is c.lw a2,0(a4), in uart_serial_init. Register a4
addresses GPIO58 IO mux: register/soc/io_mux_reg.h:485-487 (UART0_TXD function0).
The following ROM instructions clear MCU_SEL bits14:12, clear FUN_PD bit7,
then set FUN_PU bit8 (0x2f812022-0x2f812044). Field definitions are in that
header :10-65; neither those fields nor the pin entry specifies reset values.
Pinned S31 TRM pre-release v0.1 release-progress table (PDF page3) lists GPIO
Matrix and IO MUX as "to be added later"; no pin reset table is published there.

Stop here: rule(b) requires a header reset; rule(a) WT and rule(c) completed
RO status do not apply to pad R/W configuration. Decision3 covers only CLIC.
The read preserves untouched sleep/drive/input bits into subsequent stores,
so simply returning0 or importing another chip's pad default would invent
state. A read-only sample from the running reference board would be POST-BOOT
state, not reset state, and could already include this very ROM RMW; it cannot
establish the missing pre-ROM reset value. No device access was performed.
A next slice needs a sourced pad reset or a separately justified initialization/
known-mask policy; this slice does not silently authorize either.

No Zb/Zc opcode was dynamically reached; probe_wired=false. No F/Zcf/XespLoop
boundary. Console bytes=0 (neither output FIFO mapped or written).
Historical receipts keep their original observations; inherited reproduction
scripts and external tests now expect this stop. New unit tests exercise all
48 entries, byte/word coherence, reset values, exhaustive byte rejections,
atomic word rejection, widths/fetch/bounds/alias, enabled-without-delivery and
the two UART RMW configurations. External native-ROM tests still assert the
pad-release/timing and stack-guard behavior before the new stop.

## Verification contract

    python3 s31/mutate_m1i.py --output OUTPUT/m1i

New mutations cover mapping/wiring, MODE/CTL resets, CTL low-bit forcing,
reserved IE/ATTR, MODE change, IP assertion, index48, byte/word lane swaps,
non-atomic word rejection, UART mapping/reset/retention/untouched fields.
M1f restoring harness requires passing baseline/restoration; only assertion
failures count, not build errors. Run inherited registers, rom_data, map,
isa_zb, isa_zc, m1f, m1g and m1h suites serially as well.

Exact-SHA handoff records native/wasm Clippy, plain/ROM workspace tests and
empty-HOME variants, WASM build/eight demos, privacy, all mutations, and isolated
RV32/C3/C6 object/disassembly comparison against ce16eb7b. JIT-only checks do
not apply (no JIT edits). Existing firmware goldens unchanged. Stop at this
unsourced-state boundary within the three-hour budget; no timing run here.

## CPU comparison

PENDING. No CPU timing measured; shared crates and other chips unchanged.
