//! Analog register I2C master used by the S3 and C3 PLL/RF blocks.
use crate::{RegRam, Device, WriteEffect};
pub struct I2cMst { pub ram: RegRam, pub ana: std::collections::HashMap<u32, u8> }
impl Default for I2cMst { fn default() -> Self { Self::new() } }

impl I2cMst {
    pub fn new() -> Self { I2cMst { ram: RegRam::new(), ana: Default::default() } }
    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            0x0 | 0x4 => {   // I2C0_CTRL: [7:0] slave, [15:8] reg, [23:16] data, [24] write, [25] busy
                let c = self.ram.read(off);
                if c & (1 << 24) == 0 { let key = c & 0xffff; let d = *self.ana.get(&key).unwrap_or(&0) as u32; (c & !(0xff << 16) & !(1 << 25)) | (d << 16) } else { c & !(1 << 25) }
            }
            // analog-block handshakes (BBPLL cal, pkdet, txdc/rxdc comparators...): the blob writes a start bit and
            // polls a done bit in 26:24; comparator sign bits 31:30 read as 0 — enough for its search loops to run
            0x40..=0x5c => (self.ram.read(off) & 0x3fff_ffff) | (7 << 24),
            _ => self.ram.read(off),
        }
    }
    pub fn write(&mut self, off: u32, v: u32) {
        if (off == 0 || off == 4) && v & (1 << 24) != 0 { self.ana.insert(v & 0xffff, (v >> 16) as u8); }
        self.ram.write(off, v);
    }
}


impl Device for I2cMst {
    fn read(&mut self, off: u32) -> u32 { I2cMst::read(self, off) }
    fn write(&mut self, off: u32, v: u32) -> WriteEffect { I2cMst::write(self, off, v); WriteEffect::NONE }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn either_host_reads_the_same_analog_register_bank() {
        let mut master = I2cMst::new();
        for host in [0, 4] {
            master.write(host, (1 << 24) | (0xa5 << 16) | 0x0362);
            master.write(4 - host, 0x0362);
            assert_eq!(master.read(4 - host), (0xa5 << 16) | 0x0362);
            master.write(host, 0x0462);
            assert_eq!(master.read(host), 0x0462);
        }
    }

    #[test]
    fn calibration_status_preserves_control_and_comparator_defaults() {
        let mut master = I2cMst::new();
        // The existing S3 ideal-radio calibration approximation, also used by C3 libphy.
        master.write(0x50, 2);
        assert_eq!(master.read(0x50), (7 << 24) | 2);
        master.write(0x60, 0x12345678);
        assert_eq!(master.read(0x60), 0x12345678);
    }
}
