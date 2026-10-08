//! Inferred analog I2C control words shared by chip adapters; no hardware validation.
//! Bits 23:16 carry data, bit 24 selects a write and bit 25 is busy.
#[derive(Default)]
pub struct Regi2c(std::collections::HashMap<u32, u8>);
impl Regi2c {
    pub fn read(&self, host: u32, control: u32, default: u8) -> u32 {
        let control = control & !(1 << 25);
        if control & (1 << 24) != 0 { return control; }
        let data = self.0.get(&((host << 16) | (control & 0xffff))).copied().unwrap_or(default);
        (control & !(0xff << 16)) | (u32::from(data) << 16)
    }
    pub fn write(&mut self, host: u32, control: u32) {
        if control & (1 << 24) != 0 {
            self.0.insert((host << 16) | (control & 0xffff), (control >> 16) as u8);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn controls_preserve_hosts_defaults_and_read_write_direction() {
        let mut r = Regi2c::default();
        r.write(4, (1 << 24) | (0xa5 << 16) | 0x0762);
        assert_eq!(r.read(0, 0x0762 | (1 << 25), 0x5b), 0x005b0762);
        assert_eq!(r.read(4, 0x0762 | (1 << 25), 0), 0x00a50762);
        r.write(4, 0x00ff0762);
        assert_eq!(r.read(4, 0x0762, 0), 0x00a50762);
        assert_eq!(r.read(4, 0x03ff0762, 0), 0x01ff0762);
    }
}
