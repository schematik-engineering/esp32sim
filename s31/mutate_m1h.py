#!/usr/bin/env python3
"""Assertion-killed M1h register mutations, using the restoring M1f harness."""
import mutate_m1f as harness

PAD = 'boot_regs::tests::pad_hold_release_triggers_and_rejections'
TIM = 'boot_regs::tests::efuse_timing_config_reset_retained_and_untouched_field'
CLK = 'boot_regs::tests::clock_reset_reads_only'
harness.MUTANTS = [
    ('pad-map', 'boot_regs.rs', '0x2070_410c', '0x2070_4110', PAD),
    ('pad-read', 'boot_regs.rs', 'PAD_HOLD => Some(0)', 'PAD_HOLD => Some(1)', PAD),
    ('pad-tie-high', 'boot_regs.rs', '!0xa000_0000', '!0xf000_0000', PAD),
    ('pad-reserved', 'boot_regs.rs', '!0xa000_0000', '!0xafff_ffff', PAD),
    ('pad-lp-record', 'boot_regs.rs', 'value & (1 << 29) != 0', 'false', PAD),
    ('pad-hp-record', 'boot_regs.rs', 'value & (1 << 31) != 0;', 'false;', PAD),
    ('pad-lp-reset', 'boot_regs.rs', 'lp_hold_released: false', 'lp_hold_released: true', PAD),
    ('pad-hp-reset', 'boot_regs.rs', 'hp_hold_released: false', 'hp_hold_released: true', PAD),
    ('pad-lp-persistence', 'boot_regs.rs', 'self.lp_hold_released |=', 'self.lp_hold_released =', PAD),
    ('pad-hp-persistence', 'boot_regs.rs', 'self.hp_hold_released |=', 'self.hp_hold_released =', PAD),
    ('timing-map', 'boot_regs.rs', '0x2071_51f8', '0x2071_51f4', TIM),
    ('timing-reset', 'boot_regs.rs', '(240 << 16) | 480', '(241 << 16) | 480', TIM),
    ('timing-read', 'boot_regs.rs', 'EFUSE_WR_TIM_CONF2 => Some(self.efuse_wr_tim_conf2)', 'EFUSE_WR_TIM_CONF2 => Some(0)', TIM),
    ('timing-value', 'boot_regs.rs', 'self.efuse_wr_tim_conf2 = value;', 'self.efuse_wr_tim_conf2 = 0;', TIM),
    ('timing-untouched', 'boot_regs.rs', 'if value & 0xffff != 480', 'if false', TIM),
    ('clock-source-map', 'boot_regs.rs', '0x2058_7000', '0x2058_7010', CLK),
    ('clock-divider-map', 'boot_regs.rs', '0x2058_7004', '0x2058_7014', CLK),
    ('clock-read', 'boot_regs.rs', 'SOC_CLK_SEL | CPU_FREQ_CTRL0 => Some(0)', 'SOC_CLK_SEL | CPU_FREQ_CTRL0 => Some(1)', CLK),
    ('clock-source-write', 'boot_regs.rs', 'SOC_CLK_SEL | CPU_FREQ_CTRL0 => return Some(Err(Fault::Prohibited)),', 'SOC_CLK_SEL => return Some(Ok(())),\n            CPU_FREQ_CTRL0 => return Some(Err(Fault::Prohibited)),', CLK),
    ('clock-divider-write', 'boot_regs.rs', 'SOC_CLK_SEL | CPU_FREQ_CTRL0 => return Some(Err(Fault::Prohibited)),', 'CPU_FREQ_CTRL0 => return Some(Ok(())),\n            SOC_CLK_SEL => return Some(Err(Fault::Prohibited)),', CLK),
]

if __name__ == '__main__':
    harness.main()
