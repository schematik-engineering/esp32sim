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

pub(crate) struct BootRegs {
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
