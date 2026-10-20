//! S31-local CLIC storage only. No source assertion, arbitration or delivery.
//! ESP-IDF 9a97f6c54ec638111ce55cd36581b3c192f15207,
//! components/soc/esp32s31/include/soc/clic_reg.h:14-18,40-160.
//! MODE conflict: word reset (78) and byte description (148) say 11;
//! byte reset (147) says 00. Fixed machine-mode decision uses 11.
//! CTLBITS=3 (41-46), CTL reset=1f (70), implemented byte bits7:5
//! (154-160); unimplemented low bits read as ones (CLIC clicintctl rule).
use emu_core::Fault;

const BASE: u32 = 0x1080_1000;
const COUNT: usize = 48;

pub(crate) struct Clic {
    entries: [[u8; 4]; COUNT],
}
impl Default for Clic {
    fn default() -> Self {
        Self {
            entries: [[0, 0, 0xc0, 0x1f]; COUNT],
        }
    }
}
impl Clic {
    fn offset(a: u32) -> Option<usize> {
        a.checked_sub(BASE)
            .filter(|&o| o < COUNT as u32 * 4)
            .map(|o| o as usize)
    }
    pub fn read8(&self, a: u32) -> Option<u8> {
        Self::offset(a).map(|o| self.entries[o / 4][o % 4])
    }
    pub fn read32(&self, a: u32) -> Option<u32> {
        if a & 3 != 0 {
            return None;
        }
        Self::offset(a).map(|o| u32::from_le_bytes(self.entries[o / 4]))
    }
    fn validate(lane: usize, v: u8) -> Result<u8, Fault> {
        match lane {
            // Pending requires delivery semantics; reserved bits also fail closed.
            0 if v != 0 => Err(Fault::Prohibited),
            1 if v & 0xfe != 0 => Err(Fault::Prohibited),
            2 if v & 0x38 != 0 || v & 0xc0 != 0xc0 => Err(Fault::Prohibited),
            3 => Ok(v | 0x1f), // low-bit writes ignored, including zeros
            _ => Ok(v),
        }
    }
    pub fn write8(&mut self, a: u32, v: u8) -> Option<Result<(), Fault>> {
        let o = Self::offset(a)?;
        Some(Self::validate(o % 4, v).map(|v| self.entries[o / 4][o % 4] = v))
    }
    pub fn write32(&mut self, a: u32, v: u32) -> Option<Result<(), Fault>> {
        if a & 3 != 0 {
            return None;
        }
        let o = Self::offset(a)?;
        Some((|| {
            let mut next = v.to_le_bytes();
            for (lane, byte) in next.iter_mut().enumerate() {
                *byte = Self::validate(lane, *byte)?;
            }
            self.entries[o / 4] = next; // commit only after all lanes validate
            Ok(())
        })())
    }
}

#[cfg(test)]
mod tests {
    use crate::{AccessFault, S31Bus};
    use emu_core::{Bus, Fault};
    #[test]
    fn clic_enable_is_storage_without_delivery() {
        let mut p = crate::Probe::new();
        p.cpu.pc = crate::HP_SRAM.0;
        p.bus.write32(0x10801054, 0xffc70100).unwrap();
        p.cpu.mstatus |= 8;
        for i in 0..4 {
            p.bus.write32(crate::HP_SRAM.0 + i * 4, 0x00000013).unwrap();
        }
        assert_eq!(p.run(4).reason, crate::StopReason::Budget);
        assert_eq!(p.cpu.pc, crate::HP_SRAM.0 + 16);
        assert_eq!(p.cpu.retired_count, 4);
        assert_eq!(p.bus.read32(0x10801054), Ok(0xffc70100));
        assert!(!p.cpu.csr_other.contains_key(&0xfb1));
        assert!(!p.cpu.csr_other.contains_key(&0x307));
    }
    #[test]
    fn clic_byte_word_reset_retention_and_isolation() {
        let mut b = S31Bus::new();
        for i in 0..48 {
            let a = 0x10801000 + 4 * i;
            assert_eq!(b.read32(a), Ok(0x1fc00000));
            for (lane, value) in [0, 0, 0xc0, 0x1f].into_iter().enumerate() {
                assert_eq!(b.read8(a + lane as u32), Ok(value));
            }
            b.write8(a, 0).unwrap();
            b.write8(a + 1, 1).unwrap();
            b.write8(a + 2, 0xc0 | (i as u8 & 7)).unwrap();
            b.write8(a + 3, (i as u8) << 5).unwrap();
            let word = ((((i as u8) << 5) | 0x1f) as u32) << 24 | ((0xc0 | (i & 7)) << 16) | 0x100;
            assert_eq!(b.read32(a), Ok(word));
            if i < 47 {
                assert_eq!(b.read32(a + 4), Ok(0x1fc00000));
            }
            assert_eq!(b.write32(a, 0xa0c70100), Ok(()));
            for (lane, value) in [0, 1, 0xc7, 0xbf].into_iter().enumerate() {
                assert_eq!(b.read8(a + lane as u32), Ok(value));
            }
            b.write32(a, 0x00c00000).unwrap();
            assert_eq!(b.read32(a), Ok(0x1fc00000));
        }
    }
    #[test]
    fn clic_rejected_bytes_and_atomic_words() {
        let mut b = S31Bus::new();
        let a = 0x10801054;
        for lane in 0..4 {
            for value in 0..=255u32 {
                let valid = match lane {
                    0 => value == 0,
                    1 => value <= 1,
                    2 => value & 0xf8 == 0xc0,
                    _ => true,
                };
                b.write32(a, 0x5fc00100).unwrap();
                b.fault = None;
                let r = b.write8(a + lane, value as u8);
                if valid {
                    assert_eq!(r, Ok(()));
                } else {
                    assert_eq!(r, Err(Fault::Prohibited));
                    assert_eq!(
                        b.fault,
                        Some(AccessFault {
                            address: a + lane,
                            width: 1,
                            write: true,
                            fault: Fault::Prohibited
                        })
                    );
                    assert_eq!(b.read32(a), Ok(0x5fc00100));
                }
            }
        }
        // Change earlier lanes, then reject ATTR: no partial IE/IP/CTL update.
        for word in [0xa0c20101, 0xa0c20200, 0xa0c80000, 0xa0000000] {
            b.write32(a, 0x5fc00100).unwrap();
            b.fault = None;
            assert_eq!(b.write32(a, word), Err(Fault::Prohibited));
            assert_eq!(
                b.fault,
                Some(AccessFault {
                    address: a,
                    width: 4,
                    write: true,
                    fault: Fault::Prohibited
                })
            );
            assert_eq!(b.read32(a), Ok(0x5fc00100));
        }
    }
    #[test]
    fn clic_width_fetch_bounds_alias_and_neighbors() {
        let mut b = S31Bus::new();
        for a in [0x10800fff, 0x108010c0, 0x108010c1, 0x10811000, 0x10811055] {
            assert_eq!(b.read8(a), Err(Fault::Unmapped));
            assert_eq!(b.write8(a, 0), Err(Fault::Unmapped));
            assert_eq!(b.read32(a), Err(Fault::Unmapped));
            assert_eq!(b.write32(a, 0x1fc00000), Err(Fault::Unmapped));
        }
        for i in 0..48 {
            for lane in 0..4 {
                let a = 0x10801000 + i * 4 + lane;
                assert_eq!(b.read16(a), Err(Fault::Unmapped));
                assert_eq!(b.write16(a, 0), Err(Fault::Unmapped));
                assert_eq!(b.fetch(a), Err(Fault::Unmapped));
                if lane != 0 {
                    assert_eq!(b.read32(a), Err(Fault::Unmapped));
                    assert_eq!(b.write32(a, 0), Err(Fault::Unmapped));
                }
            }
        }
    }
}
