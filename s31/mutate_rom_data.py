#!/usr/bin/env python3
"""Assertion-killed loader mutations, sharing the existing serial harness."""
import mutate_registers as harness

TEST = 'rom_data_exact_range_readonly_and_atomic_validation'
harness.MUTANTS = [
    ('data-base', 'const START: usize = 0x3f700;', 'const START: usize = 0x3f704;', TEST),
    ('data-end', 'const END: usize = 0x48000;', 'const END: usize = 0x47ffc;', TEST),
    ('data-presence', 'self.rom_present[START..END].fill(true);', 'self.rom_present[START..END].fill(false);', TEST),
    ('data-copy', 'self.rom[START..END].copy_from_slice(bytes);', 'self.rom[START..END].fill(0);', TEST),
    ('data-overwrite', 'if self.rom_present[START..END].iter().any(|&b| b) {', 'if false {', TEST),
    ('data-needs-elf', 'if !self.rom_present[..4].iter().all(|&b| b) {', 'if false {', TEST),
]

if __name__ == '__main__':
    harness.main()
