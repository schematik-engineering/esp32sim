//! ROM-touched controls and the shared deterministic emulator entropy model.
//! ESP-IDF 9a97f6c54ec638111ce55cd36581b3c192f15207:
//! components/soc/esp32s31/register/soc/reg_base.h:92,107;
//! trng_reg.h:13-72,367-384; lp_peri_clkrst_reg.h:321-338.
use emu_core::Fault;
use esp_periph::Device;
const TRNG_DATE: u32 = 0x2081_40fc;
const RNG_CTRL: u32 = 0x2071_003c;
const TRNG_CONF: u32 = 0x2081_4000;
// Same IDF reg_base.h:98, rtc_wdt_reg.h:167-212: reset flag RO bit0,
// clear trigger bit19, signal width reset300. Only the ROM flag-clear is supported.
const SWD_CONFIG: u32 = 0x2080_101c;
// Same IDF reg_base.h:114; cache_reg.h:13-109. Only HP bus0/1 shutters
// are supported: flash/cache accesses remain unmapped, ROM buses2/3 unchanged.
const ICACHE_CTRL: u32 = 0x2c00_0000;
const DCACHE_CTRL: u32 = 0x2c00_0004;

// Same IDF reg_base.h:87; pmu_reg.h:2223-2254: all fields WT, reset0.
// Only LP/HP release triggers are supported; no pad electrical model.
const PAD_HOLD: u32 = 0x2070_410c;
// Same IDF reg_base.h:96; efuse_reg.h:3564-3583. Timing config only,
// NOT eFuse data/programming: ROM updates TPGM, preserves PWR_OFF_NUM.
const EFUSE_WR_TIM_CONF2: u32 = 0x2071_51f8;
// Same IDF reg_base.h:79; hp_sys_clkrst_reg.h:13-52. Both reset0.
// Only reset reads are reached; clock-source/divider changes stay prohibited.
const SOC_CLK_SEL: u32 = 0x2058_7000;
const CPU_FREQ_CTRL0: u32 = 0x2058_7004;

pub(crate) struct BootRegs {
    efuse_wr_tim_conf2: u32,
    lp_hold_released: bool,
    hp_hold_released: bool,
    date: u32,
    rng: u32,
    conf: u32,
    swd: u32,
    icache: u32,
    dcache: u32,
    pub(crate) entropy: esp_periph::Rng,
}
impl Default for BootRegs {
    fn default() -> Self {
        Self {
            efuse_wr_tim_conf2: (240 << 16) | 480,
            lp_hold_released: false,
            hp_hold_released: false,
            date: 2426672,
            rng: 0x4000_0000,
            conf: 1 << 30,
            swd: 300 << 20,
            icache: 0,
            dcache: 0,
            entropy: esp_periph::Rng::new(),
        }
    }
}
impl BootRegs {
    #[cfg(test)]
    pub(crate) fn set_swd_flag_for_test(&mut self) {
        self.swd |= 1;
    }
    /// IDF trng_reg.h:343-353 (CRC_DATA), :66-72 (sampling), :367-384
    /// (register clock); lp_peri_clkrst_reg.h:321-338 (RNG clock/reset).
    /// Functional model only: no silicon entropy, CRC conditioning or latency.
    pub(crate) fn read_entropy(&mut self, address: u32) -> Option<Result<u32, Fault>> {
        if address != 0x2081_4044 {
            return None;
        }
        let running = self.date & (1 << 28) != 0
            && self.rng & 0xc000_0000 == 0x4000_0000
            && self.conf & (1 << 31) != 0;
        Some(if running {
            Ok(self.entropy.read(0))
        } else {
            Err(Fault::Prohibited)
        })
    }
    pub(crate) fn read(&self, address: u32) -> Option<u32> {
        match address {
            PAD_HOLD => Some(0),
            SOC_CLK_SEL | CPU_FREQ_CTRL0 => Some(0),
            EFUSE_WR_TIM_CONF2 => Some(self.efuse_wr_tim_conf2),
            TRNG_DATE => Some(self.date),
            RNG_CTRL => Some(self.rng),
            TRNG_CONF => Some(self.conf),
            SWD_CONFIG => Some(self.swd),
            ICACHE_CTRL => Some(self.icache),
            DCACHE_CTRL => Some(self.dcache),
            _ => None,
        }
    }
    pub(crate) fn write(&mut self, address: u32, value: u32) -> Option<Result<(), Fault>> {
        match address {
            SOC_CLK_SEL | CPU_FREQ_CTRL0 => return Some(Err(Fault::Prohibited)),
            EFUSE_WR_TIM_CONF2 => {
                if value & 0xffff != 480 {
                    return Some(Err(Fault::Prohibited));
                }
                self.efuse_wr_tim_conf2 = value;
            }
            PAD_HOLD => {
                if value & !0xa000_0000 != 0 {
                    return Some(Err(Fault::Prohibited));
                }
                self.lp_hold_released |= value & (1 << 29) != 0;
                self.hp_hold_released |= value & (1 << 31) != 0;
            }
            ICACHE_CTRL | DCACHE_CTRL => {
                if value & !3 != 0 {
                    return Some(Err(Fault::Prohibited));
                }
                if address == ICACHE_CTRL {
                    self.icache = value;
                } else {
                    self.dcache = value;
                }
            }
            SWD_CONFIG => {
                // No watchdog clock, feeding, disabling or timing model. Refuse
                // writes other than the reached read/OR/clear-trigger sequence.
                if value != self.swd | (1 << 19) {
                    return Some(Err(Fault::Prohibited));
                }
                self.swd &= !1;
            }
            TRNG_DATE => self.date = value & 0x1fff_ffff,
            RNG_CTRL => {
                self.rng = value & 0xc000_0000;
                if self.rng & (1 << 31) != 0 {
                    // Held in reset; clear the modeled sampling configuration.
                    self.conf = 1 << 30;
                }
            }
            TRNG_CONF => {
                // FIFO-reset is a trigger, never retained. The functional RNG
                // model has no FIFO or sampling latency.
                if self.rng & (1 << 31) == 0 {
                    self.conf = value & !(1 << 28);
                }
            }
            _ => return None,
        }
        Some(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use crate::S31Bus;
    use emu_core::{Bus, Fault};
    #[test]
    #[ignore = "set S31_ROM and S31_ROM_DATA to pinned public ELF and M1c dump"]
    fn external_native_rom_releases_pads_and_sets_timing() {
        let mut p = crate::Probe::new();
        p.bus
            .load_rom(&std::fs::read(std::env::var_os("S31_ROM").expect("set S31_ROM")).unwrap())
            .unwrap();
        p.bus
            .load_rom_data(
                &std::fs::read(std::env::var_os("S31_ROM_DATA").expect("set S31_ROM_DATA"))
                    .unwrap(),
            )
            .unwrap();
        let stop = p.run(1_000_000);
        assert_eq!(stop.pc, 0x2f8019a4);
        assert_eq!(stop.raw, 0x00050023);
        assert_eq!(stop.retired, 7384);
        assert_eq!(
            stop.reason,
            crate::StopReason::Access(crate::AccessFault {
                address: 0x10801055,
                width: 1,
                write: true,
                fault: Fault::Unmapped,
            })
        );
        assert!(p.bus.boot_regs.lp_hold_released && p.bus.boot_regs.hp_hold_released);
        assert_eq!(p.bus.read32(0x2070410c), Ok(0));
        assert_eq!(p.bus.read32(0x207151f8), Ok(0x00c801e0));
    }
    #[test]
    fn pad_hold_release_triggers_and_rejections() {
        let mut b = S31Bus::new();
        assert_eq!(b.read32(0x2070410c), Ok(0));
        assert!(!b.boot_regs.lp_hold_released);
        assert!(!b.boot_regs.hp_hold_released);
        b.write32(0x2070410c, 0).unwrap();
        assert!(!b.boot_regs.lp_hold_released && !b.boot_regs.hp_hold_released);
        b.write32(0x2070410c, 1 << 29).unwrap();
        assert!(b.boot_regs.lp_hold_released);
        assert!(!b.boot_regs.hp_hold_released);
        assert_eq!(b.read32(0x2070410c), Ok(0));
        b.write32(0x2070410c, 1 << 31).unwrap();
        b.write32(0x2070410c, 0).unwrap();
        assert!(b.boot_regs.lp_hold_released && b.boot_regs.hp_hold_released);
        assert_eq!(b.read32(0x2070410c), Ok(0));
        let mut b = S31Bus::new();
        b.write32(0x2070410c, 0xa0000000).unwrap();
        assert!(b.boot_regs.lp_hold_released && b.boot_regs.hp_hold_released);
        for bit in (0..=28).chain([30]) {
            let mut b = S31Bus::new();
            assert_eq!(
                b.write32(0x2070410c, (1 << bit) | 0xa0000000),
                Err(Fault::Prohibited)
            );
            assert!(!b.boot_regs.lp_hold_released && !b.boot_regs.hp_hold_released);
            assert_eq!(
                b.fault.unwrap(),
                crate::AccessFault {
                    address: 0x2070410c,
                    width: 4,
                    write: true,
                    fault: Fault::Prohibited,
                }
            );
        }
    }
    #[test]
    fn efuse_timing_config_reset_retained_and_untouched_field() {
        let mut b = S31Bus::new();
        assert_eq!(b.read32(0x207151f8), Ok(0x00f001e0));
        for value in [0x00c801e0, 0xffff01e0, 0x000001e0, 0x00f001e0] {
            b.write32(0x207151f8, value).unwrap();
            assert_eq!(b.read32(0x207151f8), Ok(value));
        }
        for bit in 0..16 {
            assert_eq!(
                b.write32(0x207151f8, 0x00c801e0 ^ (1 << bit)),
                Err(Fault::Prohibited)
            );
            assert_eq!(b.read32(0x207151f8), Ok(0x00f001e0));
        }
    }
    #[test]
    fn clock_reset_reads_only() {
        let mut b = S31Bus::new();
        for a in [0x20587000, 0x20587004] {
            assert_eq!(b.read32(a), Ok(0));
            for value in [0, 1, 0xffffffff] {
                assert_eq!(b.write32(a, value), Err(Fault::Prohibited));
                assert_eq!(b.read32(a), Ok(0));
            }
        }
    }
    #[test]
    fn m1h_widths_fetch_neighbors_fail_closed() {
        let mut b = S31Bus::new();
        for a in [0x2070410c, 0x207151f8, 0x20587000, 0x20587004] {
            assert_eq!(b.read8(a), Err(Fault::Unmapped));
            assert_eq!(b.read16(a), Err(Fault::Unmapped));
            assert_eq!(b.write8(a, 0), Err(Fault::Unmapped));
            assert_eq!(b.write16(a, 0), Err(Fault::Unmapped));
            assert_eq!(b.fetch(a), Err(Fault::Unmapped));
            for offset in 1..4 {
                assert_eq!(b.read32(a + offset), Err(Fault::Unmapped));
                assert_eq!(b.write32(a + offset, 0), Err(Fault::Unmapped));
            }
        }
        for a in [
            0x20704108, 0x20704110, 0x207151f4, 0x207151fc, 0x20586ffc, 0x20587008, 0x10801055,
        ] {
            assert_eq!(b.read32(a), Err(Fault::Unmapped));
            assert_eq!(b.write32(a, 0), Err(Fault::Unmapped));
        }
        assert_eq!(b.write8(0x10801055, 0), Err(Fault::Unmapped));
    }
    #[test]
    fn entropy_reset_gate_is_independent_of_sampling_reset() {
        // Isolate the reset gate from the separate reset-clears-sampling rule.
        // Direct internal state is intentional: normal MMIO preserves that invariant.
        let mut regs = super::BootRegs {
            date: (1 << 28) | 2426672,
            rng: 0xc000_0000,
            conf: 1 << 31,
            ..super::BootRegs::default()
        };
        assert_eq!(regs.read_entropy(0x20814044), Some(Err(Fault::Prohibited)));
    }
    #[test]
    fn boot_regs_mapping_reset_values_and_retention() {
        let mut b = S31Bus::new();
        assert_eq!(b.read32(0x208140fc), Ok(2426672));
        assert_eq!(b.read32(0x2071003c), Ok(0x4000_0000));
        assert_eq!(b.read32(0x20814000), Ok(1 << 30));
        for (address, value, expected) in [
            (0x208140fc, 0xffff_ffff, 0x1fff_ffff),
            (0x2071003c, 0x7fff_ffff, 0x4000_0000),
            (0x20814000, 0xffff_ffff, 0xefff_ffff),
            (0x20814000, 0x8000_0000, 0x8000_0000),
        ] {
            b.write32(address, value).unwrap();
            assert_eq!(b.read32(address), Ok(expected));
        }
        b.write32(0x2071003c, 0xffff_ffff).unwrap();
        assert_eq!(b.read32(0x2071003c), Ok(0xc000_0000));
        assert_eq!(b.read32(0x20814000), Ok(1 << 30));
        b.write32(0x20814000, 0xffff_ffff).unwrap();
        assert_eq!(b.read32(0x20814000), Ok(1 << 30));
        b.write32(0x2071003c, 1 << 30).unwrap();
        b.write32(0x20814000, 0x8000_0000).unwrap();
        assert_eq!(b.read32(0x20814000), Ok(0x8000_0000));
    }
    #[test]
    fn boot_regs_unknown_widths_and_entropy_fail_closed() {
        let mut b = S31Bus::new();
        for address in [0x208140fc, 0x2071003c, 0x20814000] {
            assert_eq!(b.read8(address), Err(Fault::Unmapped));
            assert_eq!(b.read16(address), Err(Fault::Unmapped));
            assert_eq!(b.write8(address, 0), Err(Fault::Unmapped));
            assert_eq!(b.write16(address, 0), Err(Fault::Unmapped));
            assert_eq!(b.fetch(address), Err(Fault::Unmapped));
            assert_eq!(b.read32(address + 1), Err(Fault::Unmapped));
        }
        for address in [
            0x20814004, 0x2081403c, 0x20814040, 0x20814048, 0x2070103c, 0x20710040,
        ] {
            assert_eq!(b.read32(address), Err(Fault::Unmapped));
            assert_eq!(b.write32(address, 0), Err(Fault::Unmapped));
        }
    }
}
