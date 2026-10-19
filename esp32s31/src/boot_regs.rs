//! ROM-touched control registers only, not an entropy generator.
//! ESP-IDF 9a97f6c54ec638111ce55cd36581b3c192f15207:
//! components/soc/esp32s31/register/soc/reg_base.h:92,107;
//! trng_reg.h:13-72,367-384; lp_peri_clkrst_reg.h:321-338.
use emu_core::Fault;
const TRNG_DATE: u32 = 0x2081_40fc;
const RNG_CTRL: u32 = 0x2071_003c;
const TRNG_CONF: u32 = 0x2081_4000;

pub(crate) struct BootRegs {
    date: u32,
    rng: u32,
    conf: u32,
}
impl Default for BootRegs {
    fn default() -> Self {
        Self {
            date: 2426672,
            rng: 0x4000_0000,
            conf: 1 << 30,
        }
    }
}
impl BootRegs {
    pub(crate) fn read(&self, address: u32) -> Option<u32> {
        match address {
            TRNG_DATE => Some(self.date),
            RNG_CTRL => Some(self.rng),
            TRNG_CONF => Some(self.conf),
            _ => None,
        }
    }
    pub(crate) fn write(&mut self, address: u32, value: u32) -> Option<Result<(), Fault>> {
        match address {
            TRNG_DATE => self.date = value & 0x1fff_ffff,
            RNG_CTRL => {
                self.rng = value & 0xc000_0000;
                if self.rng & (1 << 31) != 0 {
                    // Held in reset; clear the modeled sampling configuration.
                    self.conf = 1 << 30;
                }
            }
            TRNG_CONF => {
                // FIFO-reset is a trigger, never retained. There is no generated
                // entropy/FIFO backing: any data/status access still faults.
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
            0x20814004, 0x2081403c, 0x20814040, 0x20814044, 0x20814048, 0x2070103c, 0x20710040,
        ] {
            assert_eq!(b.read32(address), Err(Fault::Unmapped));
            assert_eq!(b.write32(address, 0), Err(Fault::Unmapped));
        }
    }
}
