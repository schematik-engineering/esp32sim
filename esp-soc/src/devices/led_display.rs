use esp_periph::i2c::I2cDevice;
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug)]
pub struct LedDisplayConfig {
    pub id: u8,
    pub controller: u8,
    pub layout: u8,
    pub a: u8,
    pub b: u8,
    pub address: u8,
    pub digits: u8,
    pub colon: bool,
}
impl LedDisplayConfig {
    pub fn valid(self) -> bool {
        self.id < 16
            && self.a < 49
            && self.b < 49
            && self.a != self.b
            && match self.controller {
                1 => {
                    (0x70..=0x77).contains(&self.address)
                        && (1..=4).contains(&self.layout)
                        && self.digits == 4
                        && !self.colon
                }
                2 => {
                    self.address == 0
                        && self.layout == 1
                        && matches!(self.digits, 4 | 6)
                        && (!self.colon || self.digits == 4)
                }
                3 => {
                    (5..=8).contains(&self.layout)
                        && self.address < 49
                        && self.address != self.a
                        && self.address != self.b
                        && (1..=64).contains(&self.digits)
                        && !self.colon
                }
                _ => false,
            }
    }
    pub fn pins(self) -> Vec<u8> {
        let mut pins = vec![self.a, self.b];
        if self.controller == 3 {
            pins.push(self.address);
        }
        pins
    }
    pub fn dimensions(self) -> (u16, u16) {
        if self.controller == 3 {
            return (self.digits as u16 * 8, 8);
        }
        match self.layout {
            3 | 4 => (8, 8),
            2 => (48, 20),
            _ => (self.digits as u16 * 12 + 4, 20),
        }
    }
}

pub struct LedDisplay {
    pub config: LedDisplayConfig,
    pub generation: u64,
    ram: [u8; 16],
    pointer: u8,
    oscillator: bool,
    control: u8,
    dimming: u8,
    hz: u64,
    cycle: u64,
    blink_epoch: u64,
    blink_on: bool,
    clk: bool,
    dio: bool,
    active: bool,
    first: bool,
    bit: u8,
    byte: u8,
    ack: bool,
    data_command: u8,
    write_ram: bool,
}
impl LedDisplay {
    pub fn new(config: LedDisplayConfig, hz: u64) -> Result<Self, String> {
        if config.controller == 3 || !config.valid() || hz == 0 {
            return Err("invalid LED display wiring or layout".into());
        }
        Ok(Self {
            config,
            generation: 0,
            ram: [0; 16],
            pointer: 0,
            oscillator: false,
            control: 0,
            dimming: 0,
            hz,
            cycle: 0,
            blink_epoch: 0,
            blink_on: true,
            clk: true,
            dio: true,
            active: false,
            first: true,
            bit: 0,
            byte: 0,
            ack: false,
            data_command: 0x40,
            write_ram: false,
        })
    }
    fn changed(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }
    pub fn advance(&mut self, cycle: u64) {
        self.cycle = cycle;
        let blink = (self.control >> 1) & 3;
        let on = self.config.controller != 1
            || blink == 0
            || ((cycle.saturating_sub(self.blink_epoch) / (self.hz >> (3 - blink)).max(1))
                .is_multiple_of(2));
        if on != self.blink_on {
            self.blink_on = on;
            self.changed();
        }
    }
    fn ht_command(&mut self, byte: u8) {
        match byte >> 4 {
            0 => self.pointer = byte & 15,
            2 => {
                self.oscillator = byte & 1 != 0;
                self.changed();
            }
            8 => {
                self.control = byte & 7;
                self.blink_epoch = self.cycle;
                self.blink_on = true;
                self.changed();
            }
            0xe => {
                self.dimming = byte & 15;
                self.changed();
            }
            _ => {}
        }
    }
    fn ram_write(&mut self, byte: u8) {
        if self.ram[self.pointer as usize] != byte {
            self.ram[self.pointer as usize] = byte;
            self.changed();
        }
    }
    fn tm_byte(&mut self, byte: u8) {
        if self.first {
            self.first = false;
            self.write_ram = false;
            match byte & 0xc0 {
                0x40 => self.data_command = byte,
                0x80 => {
                    self.control = byte & 15;
                    self.changed();
                }
                0xc0 => {
                    self.pointer = byte & 7;
                    self.write_ram = self.pointer < 6 && self.data_command & 2 == 0;
                }
                _ => {}
            }
        } else if self.write_ram {
            if self.pointer < 6 {
                self.ram_write(byte);
            }
            if self.data_command & 4 == 0 {
                self.pointer = self.pointer.saturating_add(1);
            }
        }
    }
    pub fn drive(&mut self, enabled: u64, output: u64) {
        if self.config.controller != 2 {
            return;
        }
        let level = |pin| enabled & (1u64 << pin) == 0 || output & (1u64 << pin) != 0;
        let clk = level(self.config.a);
        let dio = level(self.config.b);
        if clk && self.clk && dio != self.dio && !self.ack {
            self.active = !dio;
            self.first = true;
            self.bit = 0;
            self.byte = 0;
            self.ack = false;
        } else if self.active && clk != self.clk {
            if clk {
                if self.bit < 8 {
                    self.byte |= (dio as u8) << self.bit;
                    self.bit += 1;
                    if self.bit == 8 {
                        self.tm_byte(self.byte);
                    }
                } else {
                    self.bit = 9;
                }
            } else if self.bit == 8 {
                self.ack = true;
            } else if self.bit == 9 {
                self.ack = false;
                self.bit = 0;
                self.byte = 0;
            }
        }
        self.clk = clk;
        self.dio = dio;
    }
    pub fn ack_pin(&self) -> Option<u8> {
        (self.config.controller == 2 && self.ack).then_some(self.config.b)
    }
    pub fn frame(&self) -> Vec<u8> {
        let (w, h) = self.config.dimensions();
        let mut pixels = vec![0u16; w as usize * h as usize];
        let visible = if self.config.controller == 1 {
            self.oscillator && self.control & 1 != 0 && self.blink_on
        } else {
            self.control & 8 != 0
        };
        if visible {
            let duty = if self.config.controller == 1 {
                self.dimming + 1
            } else {
                [1, 2, 4, 10, 11, 12, 13, 14][(self.control & 7) as usize]
            };
            let red = ((31 * duty as u16 / 16) << 11) as u16;
            let green = (63 * duty as u16 / 16) << 5;
            let mut put = |x: usize, y: usize, color: u16| {
                pixels[y * w as usize + x] = color;
            };
            if self.config.layout >= 3 {
                for y in 0..8 {
                    for x in 0..8 {
                        let bit = if self.config.layout == 3 {
                            (x + 7) % 8
                        } else {
                            x
                        };
                        let lo = self.ram[2 * y] & (1 << bit) != 0;
                        let hi = self.ram[2 * y + 1] & (1 << bit) != 0;
                        put(
                            x,
                            y,
                            if self.config.layout == 3 {
                                if lo {
                                    green
                                } else {
                                    0
                                }
                            } else {
                                (if lo { green } else { 0 }) | (if hi { red } else { 0 })
                            },
                        );
                    }
                }
            } else {
                for digit in 0..self.config.digits as usize {
                    let word = if self.config.controller == 2 {
                        self.ram[digit] as u16
                    } else {
                        let row = if self.config.layout == 1 && digit >= 2 {
                            digit + 1
                        } else {
                            digit
                        };
                        u16::from_le_bytes([self.ram[2 * row], self.ram[2 * row + 1]])
                    };
                    let x = digit * 12
                        + if self.config.layout == 1 && digit >= 2 {
                            4
                        } else {
                            0
                        };
                    let segments = if self.config.layout == 2 { 14 } else { 7 };
                    for bit in 0..segments {
                        if word & (1 << bit) == 0 {
                            continue;
                        }
                        let line = match bit {
                            0 => (2, 1, 8, 1),
                            1 => (9, 2, 9, 8),
                            2 => (9, 10, 9, 16),
                            3 => (2, 17, 8, 17),
                            4 => (1, 10, 1, 16),
                            5 => (1, 2, 1, 8),
                            6 => (2, 9, if segments == 14 { 4 } else { 8 }, 9),
                            7 => (6, 9, 8, 9),
                            8 => (2, 2, 4, 7),
                            9 => (5, 2, 5, 7),
                            10 => (8, 2, 6, 7),
                            11 => (4, 11, 2, 16),
                            12 => (5, 11, 5, 16),
                            _ => (6, 11, 8, 16),
                        };
                        let (x0, y0, x1, y1) = line;
                        let steps = i32::max(i32::abs(x1 - x0), i32::abs(y1 - y0));
                        for step in 0..=steps {
                            put(
                                x + (x0 + (x1 - x0) * step / steps) as usize,
                                (y0 + (y1 - y0) * step / steps) as usize,
                                red,
                            );
                        }
                    }
                    if !self.config.colon && word & (1 << if segments == 14 { 14 } else { 7 }) != 0
                    {
                        put(x + 10, 18, red);
                    }
                }
                if (self.config.controller == 1 && self.config.layout == 1 && self.ram[4] & 2 != 0)
                    || (self.config.colon && self.ram[1] & 0x80 != 0)
                {
                    put(25, 6, red);
                    put(25, 12, red);
                }
            }
        }
        pixels.into_iter().flat_map(u16::to_le_bytes).collect()
    }
}

pub struct Ht16k33 {
    pub state: Arc<Mutex<LedDisplay>>,
    first: bool,
    ram: bool,
}
impl Ht16k33 {
    pub fn new(state: Arc<Mutex<LedDisplay>>) -> Self {
        Self {
            state,
            first: true,
            ram: false,
        }
    }
}
impl I2cDevice for Ht16k33 {
    fn pins(&self) -> Option<(u8, u8)> {
        let c = self.state.lock().unwrap().config;
        Some((c.a, c.b))
    }
    fn start(&mut self, _read: bool) -> bool {
        self.first = true;
        true
    }
    fn write(&mut self, byte: u8) -> bool {
        let mut d = self.state.lock().unwrap();
        if self.first {
            self.first = false;
            self.ram = byte < 16;
            d.ht_command(byte);
        } else if self.ram {
            d.ram_write(byte);
            d.pointer = (d.pointer + 1) & 15;
        }
        true
    }
    fn read(&mut self) -> u8 {
        let mut d = self.state.lock().unwrap();
        let byte = d.ram[d.pointer as usize];
        d.pointer = (d.pointer + 1) & 15;
        byte
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config(controller: u8, layout: u8) -> LedDisplayConfig {
        LedDisplayConfig {
            id: 0,
            controller,
            layout,
            a: 4,
            b: 5,
            address: if controller == 1 { 0x70 } else { 0 },
            digits: 4,
            colon: false,
        }
    }
    #[test]
    fn ht_ram_wrap_readback_dimming_blink_and_matrix_wiring() {
        let state = Arc::new(Mutex::new(LedDisplay::new(config(1, 3), 1000).unwrap()));
        let mut i = Ht16k33::new(state.clone());
        for b in [0x21, 0x81, 0xef] {
            i.start(false);
            i.write(b);
        }
        i.start(false);
        for b in [0x0f, 0xff, 1] {
            i.write(b);
        }
        i.start(false);
        i.write(0);
        i.start(true);
        assert_eq!(i.read(), 1);
        let p = state.lock().unwrap().frame();
        assert_eq!(&p[2..4], &0x07e0u16.to_le_bytes());
        i.start(false);
        i.write(0xe0);
        assert_eq!(
            &state.lock().unwrap().frame()[2..4],
            &0x0060u16.to_le_bytes()
        );
        i.start(false);
        i.write(0x83);
        state.lock().unwrap().advance(250);
        assert!(state.lock().unwrap().frame().iter().all(|b| *b == 0));
        state.lock().unwrap().advance(500);
        assert!(state.lock().unwrap().frame().iter().any(|b| *b != 0));
        i.start(false);
        i.write(0x20);
        assert!(state.lock().unwrap().frame().iter().all(|b| *b == 0));
    }
    fn levels(d: &mut LedDisplay, clk: bool, dio: bool) {
        d.drive(
            (if clk { 0 } else { 1 << 4 }) | (if dio { 0 } else { 1 << 5 }),
            0,
        );
    }
    fn send(d: &mut LedDisplay, b: u8) {
        for bit in 0..8 {
            levels(d, false, b & (1 << bit) != 0);
            levels(d, true, b & (1 << bit) != 0);
        }
        levels(d, false, true);
        assert_eq!(d.ack_pin(), Some(5));
        levels(d, true, true);
        levels(d, false, true);
        assert_eq!(d.ack_pin(), None);
    }
    fn tx(d: &mut LedDisplay, bytes: &[u8]) {
        levels(d, true, true);
        levels(d, true, false);
        for b in bytes {
            send(d, *b);
        }
        levels(d, false, false);
        levels(d, true, false);
        levels(d, true, true);
    }
    #[test]
    fn tm_colon_is_a_physical_wiring_option_and_six_digit_ram_is_bounded() {
        let mut c = config(2, 1);
        c.colon = true;
        let mut d = LedDisplay::new(c, 1000).unwrap();
        tx(&mut d, &[0x40]);
        tx(&mut d, &[0xc0, 0, 0x80]);
        tx(&mut d, &[0x8f]);
        let p = d.frame();
        assert_eq!(
            &p[(6 * 52 + 25) * 2..(6 * 52 + 25) * 2 + 2],
            &0xd800u16.to_le_bytes()
        );
        assert_eq!(&p[(18 * 52 + 22) * 2..(18 * 52 + 22) * 2 + 2], &[0, 0]);
        c.digits = 6;
        assert!(LedDisplay::new(c, 1000).is_err());
        c.colon = false;
        let mut d = LedDisplay::new(c, 1000).unwrap();
        tx(&mut d, &[0x40]);
        tx(&mut d, &[0xc0, 1, 2, 4, 8, 16, 32, 255]);
        tx(&mut d, &[0x8f]);
        assert_eq!(&d.ram[..7], &[1, 2, 4, 8, 16, 32, 0]);
        assert_eq!(d.frame().len(), 76 * 20 * 2);
    }
    #[test]
    fn tm_lsb_ack_fixed_and_increment_address_brightness_and_bounds() {
        let mut d = LedDisplay::new(config(2, 1), 1000).unwrap();
        tx(&mut d, &[0x40]);
        tx(&mut d, &[0xc0, 0x3f, 6, 0x5b, 0x4f]);
        tx(&mut d, &[0x8f]);
        assert_eq!(&d.ram[..4], &[0x3f, 6, 0x5b, 0x4f]);
        assert!(d.frame().iter().any(|b| *b != 0));
        tx(&mut d, &[0x44]);
        tx(&mut d, &[0xc2, 1, 2]);
        assert_eq!(d.ram[2], 2);
        assert_eq!(d.ram[3], 0x4f);
        tx(&mut d, &[0xc7, 255]);
        assert_eq!(d.ram[7], 0);
        tx(&mut d, &[0x80]);
        assert!(d.frame().iter().all(|b| *b == 0));
    }
}
