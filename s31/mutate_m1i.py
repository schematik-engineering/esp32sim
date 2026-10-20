#!/usr/bin/env python3
"""Assertion-killed M1i CLIC/UART config mutations; restoring M1f harness."""
import mutate_m1f as harness

RW = 'clic::tests::clic_byte_word_reset_retention_and_isolation'
BAD = 'clic::tests::clic_rejected_bytes_and_atomic_words'
EDGE = 'clic::tests::clic_width_fetch_bounds_alias_and_neighbors'
UART = 'boot_regs::tests::uart_clock_memory_resets_rmw_retention_and_rejections'
harness.MUTANTS = [
    ('clic-map', 'clic.rs', '0x1080_1000', '0x1080_2000', RW),
    ('clic-attr-reset', 'clic.rs', '[[0, 0, 0xc0, 0x1f]; COUNT]', '[[0, 0, 0, 0x1f]; COUNT]', RW),
    ('clic-ctl-reset', 'clic.rs', '[[0, 0, 0xc0, 0x1f]; COUNT]', '[[0, 0, 0xc0, 0]; COUNT]', RW),
    ('clic-ctl-low-bits', 'clic.rs', '3 => Ok(v | 0x1f)', '3 => Ok(v)', RW),
    ('clic-ie-reserved', 'clic.rs', '1 if v & 0xfe != 0', '1 if false', BAD),
    ('clic-attr-reserved', 'clic.rs', 'v & 0x38 != 0 || v & 0xc0 != 0xc0', 'v & 0xc0 != 0xc0', BAD),
    ('clic-mode-change', 'clic.rs', 'v & 0x38 != 0 || v & 0xc0 != 0xc0', 'v & 0x38 != 0', BAD),
    ('clic-pending', 'clic.rs', '0 if v != 0', '0 if false', BAD),
    ('clic-bound-48', 'clic.rs', 'const COUNT: usize = 48;', 'const COUNT: usize = 49;', EDGE),
    ('clic-byte-lane', 'clic.rs', 'self.entries[o / 4][o % 4])', 'self.entries[o / 4][(o + 1) % 4])', RW),
    ('clic-word-lanes', 'clic.rs', 'u32::from_le_bytes(self.entries[o / 4])', 'u32::from_be_bytes(self.entries[o / 4])', RW),
    ('clic-word-write-lanes', 'clic.rs', 'let mut next = v.to_le_bytes();', 'let mut next = v.to_be_bytes();', RW),
    ('clic-nonatomic-word', 'clic.rs', '*byte = Self::validate(lane, *byte)?;', '*byte = Self::validate(lane, *byte)?; self.entries[o / 4][lane] = *byte;', BAD),
    ('clic-byte-read-wiring', 'lib.rs', 'self.clic.read8(a)', 'self.clic.read8(a.wrapping_add(0x10000))', RW),
    ('clic-word-read-wiring', 'lib.rs', 'self.clic.read32(a)', 'self.clic.read32(a.wrapping_add(0x10000))', RW),
    ('clic-byte-write-wiring', 'lib.rs', 'self.clic.write8(a, v)', 'self.clic.write8(a.wrapping_add(0x10000), v)', BAD),
    ('clic-word-write-wiring', 'lib.rs', 'self.clic.write32(a, v)', 'self.clic.write32(a.wrapping_add(0x10000), v)', RW),
    ('uart-clock-map', 'boot_regs.rs', '0x2058_7088', '0x2058_708c', UART),
    ('uart-clock-reset', 'boot_regs.rs', 'uart0_clock: 0x83', 'uart0_clock: 0', UART),
    ('uart-clock-value', 'boot_regs.rs', 'self.uart0_clock = value;', 'self.uart0_clock = 0;', UART),
    ('uart-clock-untouched', 'boot_regs.rs', 'if value & !0x8f != 0', 'if false', UART),
    ('uart-memory-map', 'boot_regs.rs', '0x2058_628c', '0x2058_6290', UART),
    ('uart-memory-reset', 'boot_regs.rs', 'uart0_memory: 0,', 'uart0_memory: 4,', UART),
    ('uart-memory-value', 'boot_regs.rs', 'self.uart0_memory = value;', 'self.uart0_memory = 0;', UART),
    ('uart-memory-untouched', 'boot_regs.rs', 'if value & !4 != 0', 'if false', UART),
]

if __name__ == '__main__':
    harness.main()
