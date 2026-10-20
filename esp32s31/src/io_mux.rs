//! Stored-only pad configuration: no routing, input sampling or electrical effect.
//! ESP-IDF 9a97f6c54ec638111ce55cd36581b3c192f15207,
//! components/soc/esp32s31/register/soc/reg_base.h:73;
//! io_mux_struct.h:14-112 (R/W bits17:0, generic GPIOn reset), :131-135 (63 pins).
//! Pin-specific reset overrides are unpublished; see s31/m1j.md decision4 caveat.
use emu_core::Fault;
const BASE: u32 = 0x2058_2000;
const COUNT: usize = 63;

pub(crate) struct IoMux {
    pins: [u32; COUNT],
}
impl Default for IoMux {
    fn default() -> Self {
        Self {
            pins: [0x1800; COUNT],
        }
    }
}
impl IoMux {
    fn index(a: u32) -> Option<usize> {
        let offset = a.checked_sub(BASE)? as usize;
        (offset < COUNT * 4 && offset.is_multiple_of(4)).then_some(offset / 4)
    }
    pub(crate) fn read32(&self, a: u32) -> Option<u32> {
        Self::index(a).map(|n| self.pins[n])
    }
    pub(crate) fn write32(&mut self, a: u32, v: u32) -> Option<Result<(), Fault>> {
        let n = Self::index(a)?;
        Some(if v & !0x3ffff != 0 {
            Err(Fault::Prohibited)
        } else {
            self.pins[n] = v;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::{AccessFault, S31Bus};
    use emu_core::{Bus, Fault};
    #[test]
    fn io_mux_reset_retention_and_pin_isolation() {
        let mut b = S31Bus::new();
        for n in 0..63 {
            assert_eq!(b.read32(0x20582000 + n * 4), Ok(0x1800));
        }
        for bit in 0..18 {
            for n in 0..63 {
                let a = 0x20582000 + n * 4;
                b.write32(a, 1 << bit).unwrap();
                assert_eq!(b.read32(a), Ok(1 << bit));
                assert_eq!(
                    b.read32(0x20582000 + ((n + 1) % 63) * 4),
                    Ok(if n == 62 {
                        1 << bit
                    } else if bit == 0 {
                        0x1800
                    } else {
                        1 << (bit - 1)
                    })
                );
            }
        }
        for n in 0..63 {
            let a = 0x20582000 + n * 4;
            for v in [0x3ffff, 0, 0x1800, n, 0x900] {
                b.write32(a, v).unwrap();
                assert_eq!(b.read32(a), Ok(v));
            }
        }
    }
    #[test]
    fn io_mux_reserved_bits_reject_atomically() {
        let mut b = S31Bus::new();
        for n in 0..63 {
            let a = 0x20582000 + n * 4;
            b.write32(a, 0x1800).unwrap();
            for bit in 18..32 {
                b.fault = None;
                assert_eq!(b.write32(a, (1 << bit) | 0x15555), Err(Fault::Prohibited));
                assert_eq!(
                    b.fault,
                    Some(AccessFault {
                        address: a,
                        width: 4,
                        write: true,
                        fault: Fault::Prohibited
                    })
                );
                assert_eq!(b.read32(a), Ok(0x1800));
            }
        }
    }
    #[test]
    fn io_mux_width_fetch_bounds_and_reserved_offsets() {
        let mut b = S31Bus::new();
        for n in 0..63 {
            let a = 0x20582000 + n * 4;
            for d in 0..4 {
                assert_eq!(b.read8(a + d), Err(Fault::Unmapped));
                assert_eq!(b.read16(a + d), Err(Fault::Unmapped));
                assert_eq!(b.write8(a + d, 0), Err(Fault::Unmapped));
                assert_eq!(b.write16(a + d, 0), Err(Fault::Unmapped));
                assert_eq!(b.fetch(a + d), Err(Fault::Unmapped));
                if d != 0 {
                    assert_eq!(b.read32(a + d), Err(Fault::Unmapped));
                    assert_eq!(b.write32(a + d, 0), Err(Fault::Unmapped));
                }
            }
        }
        for a in (0x205820fc..=0x205821fc)
            .step_by(4)
            .chain([0x20581ffc, 0x20582200])
        {
            assert_eq!(b.read32(a), Err(Fault::Unmapped));
            assert_eq!(b.write32(a, 0), Err(Fault::Unmapped));
        }
    }
}
