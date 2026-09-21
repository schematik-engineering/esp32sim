use esp_periph::i2c::I2cDevice;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};

const ROM: &[u8; 2048] = include_bytes!("hd44780-a00.bin");
#[derive(Clone, Copy)]
pub struct LcdConfig {
    pub id: u8,
    pub sda: u8,
    pub scl: u8,
    pub address: u8,
    pub columns: u8,
    pub rows: u8,
}
impl LcdConfig {
    pub fn valid(&self) -> bool {
        self.id < 16
            && self.sda < 49
            && self.scl < 49
            && self.sda != self.scl
            && ((0x20..=0x27).contains(&self.address) || (0x38..=0x3f).contains(&self.address))
            && (1..=40).contains(&self.columns)
            && (1..=4).contains(&self.rows)
            && (self.rows <= 2 || self.columns <= 20)
    }
    pub fn dimensions(&self) -> (u16, u16) {
        (self.columns as u16 * 6, self.rows as u16 * 9)
    }
}
pub struct Lcd {
    pub config: LcdConfig,
    clock: Arc<AtomicU64>,
    hz: u64,
    port: u8,
    four_bit: bool,
    pending: Option<(u8, bool)>,
    read_low: bool,
    read_value: u8,
    ddram: [u8; 128],
    cgram: [u8; 64],
    address: u8,
    cgram_selected: bool,
    increment: bool,
    entry_shift: bool,
    two_lines: bool,
    display: bool,
    cursor: bool,
    blink: bool,
    shift: u8,
    busy_until: u64,
    blink_phase: bool,
    pub generation: u64,
}
impl Lcd {
    pub fn new(config: LcdConfig, clock: Arc<AtomicU64>, hz: u64) -> Result<Self, String> {
        if !config.valid() || hz == 0 {
            return Err("invalid PCF8574 LCD geometry or wiring".into());
        }
        let busy_until = clock.load(Ordering::Relaxed) + hz * 15 / 1000;
        Ok(Self {
            config,
            clock,
            hz,
            port: 0xff,
            four_bit: false,
            pending: None,
            read_low: false,
            read_value: 0,
            ddram: [b' '; 128],
            cgram: [0; 64],
            address: 0,
            cgram_selected: false,
            increment: true,
            entry_shift: false,
            two_lines: false,
            display: false,
            cursor: false,
            blink: false,
            shift: 0,
            busy_until,
            blink_phase: false,
            generation: 1,
        })
    }
    fn now(&self) -> u64 {
        self.clock.load(Ordering::Relaxed)
    }
    fn busy(&self) -> bool {
        self.now() < self.busy_until
    }
    fn wait(&mut self, micros: u64) {
        self.busy_until = self.now() + (self.hz * micros).div_ceil(1_000_000);
    }
    fn step_address(&mut self, right: bool) {
        if self.cgram_selected {
            self.address = self.address.wrapping_add(if right { 1 } else { 255 }) & 63;
            return;
        }
        self.address = if self.two_lines {
            match (self.address, right) {
                (0x27, true) => 0x40,
                (0x67, true) => 0,
                (0, false) => 0x67,
                (0x40, false) => 0x27,
                (_, true) => self.address.wrapping_add(1) & 127,
                (_, false) => self.address.wrapping_sub(1) & 127,
            }
        } else if right {
            (self.address + 1) % 80
        } else {
            (self.address + 79) % 80
        };
    }
    fn data(&mut self, value: u8) {
        if self.cgram_selected {
            self.cgram[(self.address & 63) as usize] = value & 31;
        } else {
            self.ddram[(self.address & 127) as usize] = value;
        }
        self.step_address(self.increment);
        if self.entry_shift && !self.cgram_selected {
            self.shift = (self.shift + if self.increment { 1 } else { 39 }) % 40;
        }
        self.generation = self.generation.wrapping_add(1);
        self.wait(37);
    }
    fn command(&mut self, value: u8) {
        let mut delay = 37;
        if value & 0x80 != 0 {
            self.address = value & 127;
            self.cgram_selected = false;
        } else if value & 0x40 != 0 {
            self.address = value & 63;
            self.cgram_selected = true;
        } else if value & 0x20 != 0 {
            self.four_bit = value & 0x10 == 0;
            self.two_lines = value & 8 != 0;
            self.pending = None;
            self.read_low = false;
        } else if value & 0x10 != 0 {
            if value & 8 != 0 {
                self.shift = (self.shift + if value & 4 != 0 { 39 } else { 1 }) % 40;
            } else {
                self.step_address(value & 4 != 0);
            }
        } else if value & 8 != 0 {
            self.display = value & 4 != 0;
            self.cursor = value & 2 != 0;
            self.blink = value & 1 != 0;
        } else if value & 4 != 0 {
            self.increment = value & 2 != 0;
            self.entry_shift = value & 1 != 0;
        } else if value & 2 != 0 {
            self.address = 0;
            self.cgram_selected = false;
            self.shift = 0;
            delay = 1520;
        } else if value & 1 != 0 {
            self.ddram.fill(b' ');
            self.address = 0;
            self.cgram_selected = false;
            self.shift = 0;
            self.increment = true;
            delay = 1520;
        }
        self.generation = self.generation.wrapping_add(1);
        self.wait(delay);
    }
    pub fn write_port(&mut self, value: u8) {
        let old = self.port;
        self.port = value;
        if (old ^ value) & 8 != 0 {
            self.generation = self.generation.wrapping_add(1);
        }
        if old & 4 == 0 && value & 4 != 0 && value & 2 != 0 {
            if !self.read_low {
                self.read_value = if value & 1 == 0 {
                    (if self.busy() { 128 } else { 0 }) | self.address
                } else if self.cgram_selected {
                    self.cgram[(self.address & 63) as usize]
                } else {
                    self.ddram[(self.address & 127) as usize]
                };
            }
        }
        if old & 4 == 0 || value & 4 != 0 {
            return;
        }
        if old & 2 != 0 {
            if self.four_bit {
                self.read_low = !self.read_low;
            }
            if (!self.four_bit || !self.read_low) && old & 1 != 0 {
                self.step_address(self.increment);
                self.wait(37);
            }
            return;
        }
        if self.busy() {
            return;
        }
        let rs = old & 1 != 0;
        let nibble = old & 0xf0;
        if !self.four_bit {
            if rs {
                self.data(nibble)
            } else {
                self.command(nibble)
            };
            return;
        }
        if let Some((high, previous_rs)) = self.pending.take() {
            if rs != previous_rs {
                return;
            }
            let byte = high | (nibble >> 4);
            if rs {
                self.data(byte)
            } else {
                self.command(byte)
            }
        } else {
            self.pending = Some((nibble, rs));
        }
    }
    pub fn read_port(&self) -> u8 {
        if self.port & 6 != 6 {
            return self.port;
        }
        let data = if self.four_bit && self.read_low {
            self.read_value << 4
        } else {
            self.read_value & 0xf0
        };
        (self.port & 15) | (self.port & data & 0xf0)
    }
    pub fn advance(&mut self) {
        let phase = (self.now() / (self.hz * 409_600 / 1_000_000).max(1)) & 1 != 0;
        if phase != self.blink_phase {
            self.blink_phase = phase;
            if self.blink {
                self.generation = self.generation.wrapping_add(1);
            }
        }
    }
    pub fn frame(&self) -> Vec<u8> {
        let (width, height) = self.config.dimensions();
        let mut frame = vec![0; width as usize * (height as usize).div_ceil(8)];
        if !self.display || self.port & 8 == 0 {
            return frame;
        }
        for row in 0..self.config.rows as usize {
            for col in 0..self.config.columns as usize {
                let row_start = [
                    0,
                    0x40,
                    self.config.columns as usize,
                    0x40 + self.config.columns as usize,
                ][row];
                let bank = row_start & 0x40;
                let address = bank + ((row_start & 63) + col + self.shift as usize) % 40;
                let code = self.ddram[address];
                for y in 0..8 {
                    let mut pixels = if code < 16 {
                        self.cgram[(code as usize & 7) * 8 + y]
                    } else {
                        ROM[code as usize * 8 + y]
                    };
                    if !self.cgram_selected && address == self.address as usize {
                        if self.cursor && y == 7 {
                            pixels = 31;
                        }
                        if self.blink && self.blink_phase {
                            pixels = 31;
                        }
                    }
                    for x in 0..5 {
                        if pixels & (1 << (4 - x)) != 0 {
                            let px = col * 6 + x;
                            let py = row * 9 + y;
                            frame[px + (py / 8) * width as usize] |= 1 << (py & 7);
                        }
                    }
                }
            }
        }
        frame
    }
}
pub struct LcdI2c(pub Arc<Mutex<Lcd>>);
impl I2cDevice for LcdI2c {
    fn pins(&self) -> Option<(u8, u8)> {
        let c = self.0.lock().unwrap().config;
        Some((c.sda, c.scl))
    }
    fn write(&mut self, value: u8) -> bool {
        self.0.lock().unwrap().write_port(value);
        true
    }
    fn read(&mut self) -> u8 {
        self.0.lock().unwrap().read_port()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn device() -> Lcd {
        Lcd::new(
            LcdConfig {
                id: 0,
                sda: 4,
                scl: 5,
                address: 0x27,
                columns: 20,
                rows: 4,
            },
            Arc::new(AtomicU64::new(0)),
            1_000_000,
        )
        .unwrap()
    }
    fn delay(d: &mut Lcd, n: u64) {
        d.clock.fetch_add(n, Ordering::Relaxed);
        d.advance();
    }
    fn nibble(d: &mut Lcd, n: u8, rs: bool) {
        let port = (n << 4) | 8 | u8::from(rs);
        d.write_port(port);
        d.write_port(port | 4);
        d.write_port(port);
        delay(d, 50);
    }
    fn byte(d: &mut Lcd, b: u8, rs: bool) {
        nibble(d, b >> 4, rs);
        nibble(d, b & 15, rs);
    }
    fn initialize(d: &mut Lcd) {
        delay(d, 50000);
        for n in [3, 3, 3, 2] {
            nibble(d, n, false);
            delay(d, 5000);
        }
        byte(d, 0x28, false);
        byte(d, 0x0c, false);
        byte(d, 0x06, false);
    }
    #[test]
    fn nibble_initialization_ddram_rows_and_real_rom_pixels() {
        let mut d = device();
        initialize(&mut d);
        byte(&mut d, 0x94, false);
        byte(&mut d, b'A', true);
        assert_eq!(d.ddram[20], b'A');
        assert_eq!(d.address, 21);
        let frame = d.frame();
        for (y, expected) in [14u8, 17, 17, 17, 31, 17, 17, 0].iter().enumerate() {
            for x in 0..5 {
                let py = 18 + y;
                let pixel = frame[x + (py / 8) * 120] >> (py & 7) & 1;
                assert_eq!(pixel, (expected >> (4 - x)) & 1);
            }
        }
    }
    #[test]
    fn busy_clear_cgram_shift_display_and_backlight() {
        let mut d = device();
        initialize(&mut d);
        byte(&mut d, 0x40, false);
        for r in [1, 2, 4, 8, 16, 31, 0, 0] {
            byte(&mut d, r, true);
        }
        byte(&mut d, 0x80, false);
        byte(&mut d, 0, true);
        assert!(d.frame().iter().any(|b| *b != 0));
        byte(&mut d, 0x18, false);
        assert_eq!(d.shift, 1);
        byte(&mut d, 0x02, false);
        assert_eq!(d.shift, 0);
        delay(&mut d, 1520);
        byte(&mut d, 0x08, false);
        assert!(d.frame().iter().all(|b| *b == 0));
        byte(&mut d, 0x0c, false);
        d.write_port(0);
        assert!(d.frame().iter().all(|b| *b == 0));
        d.write_port(8);
        byte(&mut d, 1, false);
        assert!(d.busy());
        let address = d.address;
        byte(&mut d, b'X', true);
        assert_eq!(d.address, address);
        delay(&mut d, 1520);
        assert!(d.ddram.iter().all(|b| *b == b' '));
        assert_eq!(d.cgram[5], 31);
    }
    #[test]
    fn quasi_bidirectional_read_exposes_busy_flag_and_address() {
        let mut d = device();
        initialize(&mut d);
        byte(&mut d, 1, false);
        d.write_port(0xfa);
        d.write_port(0xfe);
        assert_ne!(d.read_port() & 0x80, 0);
        d.write_port(0xfa);
        d.write_port(0xfe);
        assert_eq!(d.read_port() & 0xf0, 0);
        d.write_port(0xfa);
        delay(&mut d, 1600);
        d.write_port(0xfe);
        assert_eq!(d.read_port() & 0x80, 0);
        d.write_port(0x7e);
        assert_eq!(d.read_port() & 0x80, 0);
    }
}
