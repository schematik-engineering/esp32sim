#!/usr/bin/env python3
"""Assertion-killed M1g mutations; shared peripheral source is never mutated."""
import mutate_m1f as harness

SEQ = 'entropy_tests::entropy_sequence_and_now_fold'
GATE = 'entropy_tests::entropy_gates_do_not_consume_and_invalid_accesses_stop'
NOW = 'entropy_tests::entropy_probe_refreshes_low_retired_before_each_read'
SWD = 'entropy_tests::swd_flag_clear_only_and_typed_stop'
CACHE = 'entropy_tests::cache_shutter_mapping_retention_and_unsupported_fields'
harness.MUTANTS = [
    ('entropy-map', 'boot_regs.rs', 'address != 0x2081_4044', 'address != 0x2081_4048', SEQ),
    ('entropy-seed', 'boot_regs.rs', 'esp_periph::Rng::new()', 'esp_periph::Rng::with_seed(1)', SEQ),
    ('entropy-value', 'boot_regs.rs', 'Ok(self.entropy.read(0))', 'Ok(0)', SEQ),
    ('entropy-now-fold', 'lib.rs', 'self.cpu.retired_count as u32;', '0;', NOW),
    ('entropy-now-source', 'lib.rs', 'self.cpu.retired_count as u32;', 'self.cpu.cycle_count as u32;', NOW),
    ('entropy-now-post-access', 'lib.rs', 'self.cpu.retired_count as u32;', 'self.cpu.retired_count.wrapping_add(1) as u32;', NOW),
    ('entropy-gate', 'boot_regs.rs', 'Some(if running {', 'Some(if true {', GATE),
    ('entropy-register-clock', 'boot_regs.rs', 'self.date & (1 << 28) != 0', 'true', GATE),
    ('entropy-clock', 'boot_regs.rs', 'self.rng & 0xc000_0000 == 0x4000_0000', 'self.rng & (1 << 31) == 0', GATE),
    ('entropy-reset', 'boot_regs.rs', 'self.rng & 0xc000_0000 == 0x4000_0000', 'self.rng & (1 << 30) != 0', 'boot_regs::tests::entropy_reset_gate_is_independent_of_sampling_reset'),
    ('entropy-sampling', 'boot_regs.rs', 'self.conf & (1 << 31) != 0', 'true', GATE),
    ('entropy-bus-wire', 'lib.rs', 'self.boot_regs.read_entropy(a)', 'self.boot_regs.read_entropy(0)', SEQ),
    ('swd-map', 'boot_regs.rs', '0x2080_101c', '0x2080_1018', SWD),
    ('swd-reset', 'boot_regs.rs', 'swd: 300 << 20', 'swd: 0', SWD),
    ('swd-value', 'boot_regs.rs', 'SWD_CONFIG => Some(self.swd)', 'SWD_CONFIG => Some(0)', SWD),
    ('swd-clear', 'boot_regs.rs', 'self.swd &= !1;', 'self.swd &= !0;', SWD),
    ('swd-clear-only', 'boot_regs.rs', 'if value != self.swd | (1 << 19)', 'if false', SWD),
    ('icache-map', 'boot_regs.rs', '0x2c00_0000', '0x2c00_0010', CACHE),
    ('dcache-map', 'boot_regs.rs', '0x2c00_0004', '0x2c00_0014', CACHE),
    ('icache-reset', 'boot_regs.rs', 'icache: 0', 'icache: 1', CACHE),
    ('dcache-reset', 'boot_regs.rs', 'dcache: 0', 'dcache: 1', CACHE),
    ('icache-value', 'boot_regs.rs', 'self.icache = value;', 'self.icache = 0;', CACHE),
    ('dcache-value', 'boot_regs.rs', 'self.dcache = value;', 'self.dcache = 0;', CACHE),
    ('cache-unsupported', 'boot_regs.rs', 'if value & !3 != 0', 'if false', CACHE),
]

if __name__ == '__main__':
    harness.main()
