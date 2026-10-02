use super::Gdma;

// C3 combines IN/OUT interrupt words and places the channel blocks at 0x70.
const IN: [u32; 7] = [0, 1, 2, 5, 7, 9, 10];
const OUT: [u32; 6] = [3, 4, 6, 8, 11, 12];
fn pack(value: u32, bits: &[u32]) -> u32 {
    bits.iter()
        .enumerate()
        .fold(0, |v, (i, b)| v | (((value >> i) & 1) << b))
}
fn unpack(value: u32, bits: &[u32]) -> u32 {
    bits.iter()
        .enumerate()
        .fold(0, |v, (i, b)| v | (((value >> b) & 1) << i))
}
fn map(off: u32) -> Option<u32> {
    const REG: [u32; 13] = [
        0, 4, 0x18, 0x1c, 0x20, 0x24, 0x28, 0x2c, 0x30, 0x34, 0x38, 0x44, 0x48,
    ];
    if off == 0x44 {
        return Some(0x3c8);
    }
    if (0x70..0x2b0).contains(&off) {
        let rel = off - 0x70;
        let (ch, reg) = (rel / 0xc0, rel % 0xc0);
        if reg <= 0x30 {
            return Some(ch * 0xc0 + REG[(reg / 4) as usize]);
        }
        if (0x60..=0x90).contains(&reg) {
            return Some(ch * 0xc0 + 0x60 + REG[((reg - 0x60) / 4) as usize]);
        }
    }
    None
}
impl Gdma {
    pub fn new_c3() -> Self {
        Self {
            c3_layout: true,
            ..Self::new()
        }
    }
    pub(super) fn read_c3(&self, off: u32) -> u32 {
        if off < 0x30 {
            let ch = (off / 0x10) as usize;
            let raw = pack(self.inp[ch].int_raw, &IN) | pack(self.out[ch].int_raw, &OUT);
            let ena = pack(self.inp[ch].int_ena, &IN) | pack(self.out[ch].int_ena, &OUT);
            return match off % 0x10 {
                0 => raw,
                4 => raw & ena,
                8 => ena,
                _ => 0,
            };
        }
        match map(off) {
            Some(o) => self.read_s3(o),
            None => self.ram.read(off),
        }
    }
    pub(super) fn write_c3(&mut self, off: u32, v: u32) {
        if off < 0x30 {
            let ch = (off / 0x10) as usize;
            let (input, output) = (unpack(v, &IN), unpack(v, &OUT));
            match off % 0x10 {
                8 => {
                    self.inp[ch].int_ena = input;
                    self.out[ch].int_ena = output;
                }
                0 | 12 => {
                    self.inp[ch].int_raw &= !input;
                    self.out[ch].int_raw &= !output;
                }
                _ => {}
            }
        } else {
            match map(off) {
                Some(o) => self.write_s3(o, v),
                None => self.ram.write(off, v),
            }
        }
    }
}
