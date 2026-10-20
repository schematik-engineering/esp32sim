#!/usr/bin/env python3
"""Assertion-killed M1j pad/UART configuration mutations; restoring M1f harness."""
import mutate_m1f as harness

RW = 'io_mux::tests::io_mux_reset_retention_and_pin_isolation'
BAD = 'io_mux::tests::io_mux_reserved_bits_reject_atomically'
EDGE = 'io_mux::tests::io_mux_width_fetch_bounds_and_reserved_offsets'
CONFIG = 'm1j_tests::pad_hold_uart_config_resets_retention_and_rejections'
harness.MUTANTS = [
    ('iomux-map', 'io_mux.rs', '0x2058_2000', '0x2058_2400', RW),
    ('iomux-reset-drive', 'io_mux.rs', 'pins: [0x1800; COUNT]', 'pins: [0x1000; COUNT]', RW),
    ('iomux-reset-select', 'io_mux.rs', 'pins: [0x1800; COUNT]', 'pins: [0x0800; COUNT]', RW),
    ('iomux-index63', 'io_mux.rs', 'const COUNT: usize = 63;', 'const COUNT: usize = 64;', EDGE),
    ('iomux-reserved-offset', 'io_mux.rs', 'let offset = a.checked_sub(BASE)? as usize;', 'let offset = a.checked_sub(BASE)? as usize; let offset = if offset == 0xfc { 0xf8 } else { offset };', EDGE),
    ('iomux-reserved-bits', 'io_mux.rs', 'if v & !0x3ffff != 0', 'if false', BAD),
    ('iomux-nonatomic', 'io_mux.rs', '            Err(Fault::Prohibited)\n        } else {', '            self.pins[n] = v & 0x3ffff; Err(Fault::Prohibited)\n        } else {', BAD),
    ('iomux-retained-bit', 'io_mux.rs', 'self.pins[n] = v;', 'self.pins[n] = v & !(1 << 17);', RW),
    ('iomux-alignment', 'io_mux.rs', 'offset.is_multiple_of(4)', 'true', EDGE),
    ('iomux-read-wiring', 'lib.rs', 'self.io_mux.read32(a)', 'self.io_mux.read32(a.wrapping_add(0x400))', RW),
    ('iomux-write-wiring', 'lib.rs', 'self.io_mux.write32(a, v)', 'self.io_mux.write32(a.wrapping_add(4), v)', RW),
    ('iomux-byte-read', 'lib.rs', 'fn read8(&mut self, a: u32) -> Result<u8, Fault> {', 'fn read8(&mut self, a: u32) -> Result<u8, Fault> { if let Some(v) = self.io_mux.read32(a) { return Ok(v as u8); }', EDGE),
    ('iomux-half-read', 'lib.rs', 'fn read16(&mut self, a: u32) -> Result<u16, Fault> {', 'fn read16(&mut self, a: u32) -> Result<u16, Fault> { if let Some(v) = self.io_mux.read32(a) { return Ok(v as u16); }', EDGE),
    ('iomux-byte-write', 'lib.rs', 'fn write8(&mut self, a: u32, v: u8) -> Result<(), Fault> {', 'fn write8(&mut self, a: u32, v: u8) -> Result<(), Fault> { if self.io_mux.read32(a).is_some() { return Ok(()); }', EDGE),
    ('iomux-half-write', 'lib.rs', 'fn write16(&mut self, a: u32, v: u16) -> Result<(), Fault> {', 'fn write16(&mut self, a: u32, v: u16) -> Result<(), Fault> { if self.io_mux.read32(a).is_some() { return Ok(()); }', EDGE),
]
for label, before, after in [
    ('hold-map', '0x2070_0270', '0x2070_0274'),
    ('hold-reset', 'gpio_hold1: 0,', 'gpio_hold1: 1,'),
    ('hold-value', 'self.gpio_hold1 = value;', 'self.gpio_hold1 = 0;'),
    ('hold-untouched', 'if value & !0x000c_0000 != 0', 'if false'),
    ('core-clock-map', '0x2038_a088', '0x2038_a08c'),
    ('core-clock-reset', 'uart0_core_clock: 0x0300_0000,', 'uart0_core_clock: 0,'),
    ('core-clock-value', 'self.uart0_core_clock = value;', 'self.uart0_core_clock = 0;'),
    ('core-clock-untouched', 'if value & !0x0300_0000 != 0', 'if false'),
    ('divider-map', '0x2038_a014', '0x2038_a018'),
    ('divider-reset', 'uart0_divider: 694,', 'uart0_divider: 0,'),
    ('divider-value', 'self.uart0_divider = value;', 'self.uart0_divider = value & 0xfff;'),
    ('divider-untouched', 'if value & !0x00f0_0fff != 0', 'if false'),
]:
    harness.MUTANTS.append((label, 'boot_regs.rs', before, after, CONFIG))

if __name__ == '__main__':
    harness.main()
