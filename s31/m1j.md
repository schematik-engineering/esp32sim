# S31 M1j: stored IO MUX configuration and native ROM UART setup

Base: 44f982ff0ae02aaae807fbd46623322de8f32b57. Bounded local/fork-only slice.
No shared-crate or other-chip changes. No banner, UART output or boot claim.

## Sources and model

All IDF citations use 9a97f6c54ec638111ce55cd36581b3c192f15207:
https://github.com/espressif/esp-idf/blob/9a97f6c54ec638111ce55cd36581b3c192f15207/ .
Paths below are relative to components/soc/esp32s31/register/soc/.

- IO MUX GPIOn: reg_base.h:73, io_mux_struct.h:14-112,131-135.
  The 63-entry file at 0x20582000 + 4*n (n<63) resets to 0x1800:
  FUN_DRV=2, MCU_SEL=1, every other field zero. Bits17:0 retained;
  any reserved bit31:18 stops Prohibited before updating any state.
  Only aligned data32 accesses; narrower, unaligned and fetch accesses stop.
  Offsets0x0fc..0x1f8 are reserved and stop; date+0x1fc is not reached or mapped.
  IO MUX pad configuration stored, not routed / no electrical effect.
  There is no input sampling, pad model, MCU_SEL routing or GPIO matrix.
- LP_SYSTEM_REG_HP_GPIO_O_HOLD_CTRL1 at 0x20700270: reg_base.h:83,
  lp_system_reg.h:1509-1519, lp_system_struct.h:1044-1057.
  R/W GPIO62..40 hold field resets0. Rule(b): retain reached GPIO58/59
  bits18/19; untouched/reserved fields must remain at reset0. No electrical effect.
  ROM uart_serial_init reads/clears bit18 at 0x2f81204e-52 and bit19 at
  0x2f812072-7c. This is not the separate LP IO MUX aperture.
- UART0 CLK_CONF at 0x2038a088: reg_base.h:31, uart_reg.h:1544-1579.
  TX/RX clock enables24/25 reset1; core resets26/27 reset0. Rule(b): retain
  reached enable fields, untouched reset/reserved bits must stay0. No UART
  reset/FIFO/clock-timing behavior. ROM uart_hal_clk_enable uses RMW.
- UART0 CLKDIV_SYNC at 0x2038a014: uart_reg.h:606-623,
  uart_struct.h:436-453. Integer11:0 resets694, fraction23:20 resets0.
  Retain those reached divider fields, reject reserved bits atomically.
  Stored configuration only: no baud-rate, transfer or synchronization model.
  ROM writes 0x0030015b before calling uart_ll_update.

The three individual registers support only aligned data32. Widths, fetch,
unaligned and unmapped neighbors stop. The shared Uart/UsbSerialJtag source
is unchanged; no FIFO is reached and no console peripheral is instantiated.

## Fixed decision4 caveat

io_mux_struct.h gives one generic GPIOn type default, not pin-specific reset
values. S31 TRM pre-release v0.1 release-progress table (PDF page3) lists
GPIO Matrix and IO MUX as "to be added later". Pin-specific overrides such
as JTAG/UART defaults on other chips are unpublished for S31. Accepted for M1
because the ROM overwrites MCU_SEL and FUN_PD/FUN_PU itself, the remaining
preserved fields have no emulated effect, and console output cannot depend
on them. A later ROM/bootloader branch on a preserved pad field of a specific
pin requires a typed stop and a new decision, not extension of this rule.
No such branch occurs on this slice's reached path (uart_serial_init
0x2f81201a-7c only masks/sets those fields; GPIO59 also sets FUN_IE).
LP IO MUX 0x20712000 and CNNT IO MUX remain unmapped.

Standing rule(b) includes default annotations in *_struct.h when *_reg.h
omits them; both headers were checked at the new stop below.

## Reproduction and next boundary

    S31_ROM=PUBLIC_ELF S31_ROM_DATA=M1C_DUMP CARGO_TARGET_DIR=BUILD_DIR bash s31/reproduce-m1j.sh

Public esp-rom-elfs 20260528 rev0 ELF SHA256:
a52ad7513deb656a910a5740125f1cce2c7941f11ce57213b7b43aea93d5ab87.
Unchanged M1c dump SHA256:
45376d77d7c7d0e9c3359a0d6eb2aa1e369aecb181fee32abc16f93b431c002a.
Two independent native ROM executions compare byte-for-byte to:

    [s31] chip=esp32s31 pc=0x2f83ee28 opcode=0x0000411c retired=7598 stop=access address=0x2038a098 width=4 write=false fault=Unmapped

Stop instruction c.lw a5,0(a0) is in uart_ll_update, polling UART_REG_UPDATE.
The loop at 0x2f83ee28-2c waits for bit0 clear, then 0x2f83ee2e-34 reads,
sets bit0 and writes it. uart_reg.h:1634-1646 and uart_struct.h:1272-1286
both give R/W/SC bit0 default0: 0 not synchronize, 1 synchronize.
This is a self-clearing register synchronization handshake, not all-WT rule(a),
retained R/W config rule(b), or polled RO status rule(c). CLIC decision3 and
pad-storage decision4 do not authorize synchronization semantics either.
A sourced reset exists; the missing decision is update/completion behavior,
not an unsourced reset. Do not implement an always-zero synchronization
register or pretend stored divider configuration has been applied.
Stop here at the first different semantic category within the three-hour bound.

GPIO58/59 finish at 0x900/0xb00. Hold register remains0, UART clock enables
remain0x03000000, divider is0x0030015b; external tests assert these values
and the exact stop. Console bytes=0. No Zb/Zc opcode dynamically reached,
probe_wired=false; no F/Zcf/XespLoop boundary. No hardware access or board
reads, RAM downloads, flash/eFuse writes, or publication.
Historical receipts retain their observations; inherited reproduction scripts
and external tests consistently expect the current stop.

## Verification contract

Unit tests cover all 63 pins, every retained and reserved bit, atomic rejection,
isolation, widths/fetch/alignment/bounds, plus reset/retention/untouched fields
for each individual register. Five ignored external tests cover native ROM
execution, including pad/UART configuration and inherited entropy/pad release.

    python3 s31/mutate_m1j.py --output OUTPUT/m1j

Mutants cover mapping/read-write wiring, both reset fields, index63,
reserved offset0x0fc, reserved bits, non-atomic rejection, dropped retained
bit, alignment, byte/halfword reads and writes; individual register mapping,
reset/value/untouched fields. M1f harness requires passing baseline and
restoration; compile failures do not count as assertion kills.
Inherited registers, rom_data, map, isa_zb, isa_zc, m1f, m1g, m1h, m1i
suites run serially, nice10, with one cargo build at a time.

Exact-SHA handoff records full retire-common native/wasm Clippy, plain/ROM
workspace and empty-HOME variants, WASM build/eight demos, privacy, all
mutation suites, RV32/C3/C6 isolated objects/disassembly vs ce16eb7b.
No existing firmware golden changes; no JIT edits (JIT-only gate inapplicable).

## CPU comparison

PENDING. No timing run; shared crates and other chips unchanged.
