use super::led_display::LedDisplayConfig;
use crate::SpiPins;

#[derive(Clone, Default)]
struct Registers {
    digits: [u8; 8],
    decode: u8,
    intensity: u8,
    scan: u8,
    on: bool,
    test: bool,
}

pub struct Max7219 {
    pub config: LedDisplayConfig,
    pub generation: u64,
    chips: Vec<Registers>,
    shift: Vec<u16>,
    clk: bool,
    data: bool,
    load: bool,
}
impl Max7219 {
    pub fn new(config: LedDisplayConfig) -> Result<Self, String> {
        if config.controller != 3 || !config.valid() {
            return Err("invalid MAX7219 module chain".into());
        }
        Ok(Self {
            config,
            generation: 0,
            chips: vec![Registers::default(); config.digits as usize],
            shift: vec![0; config.digits as usize],
            clk: false,
            data: false,
            load: true,
        })
    }
    fn bit(&mut self, mut carry: bool) {
        for word in &mut self.shift {
            let next = *word & 0x8000 != 0;
            *word = (*word << 1) | u16::from(carry);
            carry = next;
        }
    }
    fn latch(&mut self) {
        for (chip, word) in self.chips.iter_mut().zip(&self.shift) {
            let value = *word as u8;
            match (word >> 8) & 15 {
                1..=8 => chip.digits[(((word >> 8) & 15) - 1) as usize] = value,
                9 => chip.decode = value,
                10 => chip.intensity = value & 15,
                11 => chip.scan = value & 7,
                12 => chip.on = value & 1 != 0,
                15 => chip.test = value & 1 != 0,
                _ => {}
            }
        }
        self.generation = self.generation.wrapping_add(1);
    }
    pub fn gpio(&mut self, pin: u8, high: bool) {
        if pin == self.config.b {
            self.data = high;
        }
        if pin == self.config.a {
            if high && !self.clk {
                self.bit(self.data);
            }
            self.clk = high;
        }
        if pin == self.config.address {
            if high && !self.load {
                self.latch();
            }
            self.load = high;
        }
    }
    pub fn transfer(&mut self, pins: SpiPins, bytes: &[u8]) {
        if pins.sclk & (1u64 << self.config.a) == 0 {
            return;
        }
        for byte in bytes {
            for bit in (0..8).rev() {
                if pins.mosi & (1u64 << self.config.b) != 0 {
                    self.data = byte & (1 << bit) != 0;
                }
                self.bit(self.data);
            }
        }
        if pins.cs & (1u64 << self.config.address) != 0 {
            self.latch();
        }
    }
    pub fn frame(&self) -> Vec<u8> {
        let width = self.chips.len() * 8;
        let mut pixels = vec![0u16; width * 8];
        let digit_rows = self.config.layout != 6;
        let reverse_columns = self.config.layout != 5;
        let reverse_rows = self.config.layout == 8;
        let code_b = [
            0x7e, 0x30, 0x6d, 0x79, 0x33, 0x5b, 0x5f, 0x70, 0x7f, 0x7b, 0x01, 0x4f, 0x37, 0x0e,
            0x67, 0,
        ];
        for (index, chip) in self.chips.iter().enumerate() {
            if !chip.on && !chip.test {
                continue;
            }
            let duty = if chip.test {
                31
            } else {
                chip.intensity as u16 * 2 + 1
            };
            let red = ((31 * duty + 16) / 32) << 11;
            for row in 0..8 {
                for column in 0..8 {
                    let digit = if digit_rows { row } else { column };
                    let digit = if reverse_rows { 7 - digit } else { digit };
                    let segment = if digit_rows { column } else { row };
                    let segment = if reverse_columns {
                        7 - segment
                    } else {
                        segment
                    };
                    let raw = chip.digits[digit];
                    let value = if chip.decode & (1 << digit) != 0 {
                        code_b[(raw & 15) as usize] | (raw & 0x80)
                    } else {
                        raw
                    };
                    let on =
                        chip.test || (digit <= chip.scan as usize && value & (1 << segment) != 0);
                    if on {
                        pixels[row * width + width - 1 - (index * 8 + column)] = red;
                    }
                }
            }
        }
        pixels.into_iter().flat_map(u16::to_le_bytes).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> LedDisplayConfig {
        LedDisplayConfig {
            id: 0,
            controller: 3,
            layout: 5,
            a: 4,
            b: 5,
            address: 6,
            digits: 2,
            colon: false,
        }
    }
    fn pins(cs: u64) -> SpiPins {
        SpiPins {
            sclk: 1 << 4,
            mosi: 1 << 5,
            cs,
            miso: None,
        }
    }
    fn write(d: &mut Max7219, bytes: &[u8]) {
        d.gpio(6, false);
        d.transfer(pins(0), bytes);
        d.gpio(6, true);
    }
    #[test]
    fn shift_chain_latches_at_load_edge_and_observes_shared_clock_when_not_selected() {
        let mut d = Max7219::new(config()).unwrap();
        write(&mut d, &[12, 1, 12, 1]);
        write(&mut d, &[11, 7, 11, 7]);
        write(&mut d, &[10, 15, 10, 15]);
        d.gpio(6, false);
        d.transfer(pins(0), &[1, 0x80, 1, 1]);
        assert!(d.frame().iter().all(|p| *p == 0));
        d.gpio(6, true);
        assert_eq!(d.chips[0].digits[0], 1);
        assert_eq!(d.chips[1].digits[0], 0x80);
        let p = d.frame();
        assert_eq!(&p[30..32], &0xf000u16.to_le_bytes());
        assert_eq!(&p[..2], &0xf000u16.to_le_bytes());
        d.transfer(pins(0), &[1, 2, 1, 4]);
        assert_eq!(d.chips[0].digits[0], 1);
        d.gpio(6, false);
        d.gpio(6, true);
        assert_eq!(d.chips[0].digits[0], 4);
        write(&mut d, &[0, 0, 12, 0]);
        assert!(!d.chips[0].on);
        assert!(d.chips[1].on);
        write(&mut d, &[0, 0, 15, 1]);
        assert!(d.frame()[16..32].iter().any(|p| *p != 0));
        write(&mut d, &[0, 0, 15, 0]);
        assert_eq!(d.chips[0].digits[0], 4);
    }
    #[test]
    fn wrong_clock_does_not_shift_and_other_mosi_samples_held_din() {
        let mut d = Max7219::new(config()).unwrap();
        d.transfer(
            SpiPins {
                sclk: 1 << 20,
                ..pins(0)
            },
            &[0xff; 4],
        );
        assert_eq!(d.shift, [0, 0]);
        d.gpio(5, true);
        d.transfer(
            SpiPins {
                mosi: 1 << 21,
                ..pins(0)
            },
            &[0; 4],
        );
        assert_eq!(d.shift, [0xffff, 0xffff]);
        assert!(!d.chips[0].test);
        d.gpio(6, false);
        d.gpio(6, true);
        assert!(d.chips[0].test);
        d.transfer(pins(0), &[0; 4]);
        assert!(!d.data);
        d.transfer(
            SpiPins {
                mosi: 1 << 21,
                ..pins(0)
            },
            &[0xff; 4],
        );
        assert_eq!(d.shift, [0, 0]);
    }
    #[test]
    fn gpio_bitbang_decode_scan_limit_and_bounded_chain() {
        let mut d = Max7219::new(LedDisplayConfig {
            digits: 1,
            ..config()
        })
        .unwrap();
        for word in [0x0c01u16, 0x0a00, 0x0901, 0x0100] {
            d.gpio(6, false);
            for bit in (0..16).rev() {
                d.gpio(4, false);
                d.gpio(5, word & (1 << bit) != 0);
                d.gpio(4, true);
            }
            d.gpio(6, true);
        }
        let p = d.frame();
        assert_eq!(p.iter().filter(|v| **v != 0).count(), 6);
        assert!(p[16..].iter().all(|p| *p == 0));
        assert!(Max7219::new(LedDisplayConfig {
            digits: 0,
            ..config()
        })
        .is_err());
        assert!(Max7219::new(LedDisplayConfig {
            digits: 65,
            ..config()
        })
        .is_err());
        assert_eq!(
            Max7219::new(LedDisplayConfig {
                digits: 64,
                ..config()
            })
            .unwrap()
            .frame()
            .len(),
            512 * 8 * 2
        );
    }
}
