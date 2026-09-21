use esp_periph::i2c::I2cDevice;
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy)]
pub struct OledConfig {
    pub id: u8,
    pub sda: u8,
    pub scl: u8,
    pub address: u8,
    pub width: u8,
    pub height: u8,
}

pub struct Ssd1306 {
    pub config: OledConfig,
    ram: [u8; 1024],
    column: u8,
    page: u8,
    columns: (u8, u8),
    pages: (u8, u8),
    mode: u8,
    command: u8,
    arguments: Vec<u8>,
    remaining: usize,
    on: bool,
    inverse: bool,
    all_on: bool,
    start_line: u8,
    pub version: u64,
}
impl Ssd1306 {
    pub fn new(config: OledConfig) -> Result<Self, String> {
        if config.sda >= 49 || config.scl >= 49 || config.sda == config.scl
            || ![0x3c, 0x3d].contains(&config.address)
            || config.width == 0 || config.width > 128 || ![32, 64].contains(&config.height) {
            return Err("invalid SSD1306 configuration".into());
        }
        Ok(Self { config, ram: [0; 1024], column: 0, page: 0, columns: (0, 127), pages: (0, 7),
            mode: 2, command: 0, arguments: Vec::new(), remaining: 0, on: false,
            inverse: false, all_on: false, start_line: 0, version: 0 })
    }
    // Solomon Systech SSD1306 rev1.1, command table and GDDRAM addressing.
    fn command(&mut self, byte: u8) {
        if self.remaining != 0 {
            self.arguments.push(byte);
            self.remaining -= 1;
            if self.remaining != 0 { return; }
            match self.command {
                0x20 => self.mode = self.arguments[0] & 3,
                0x21 => { self.columns = (self.arguments[0] & 127, self.arguments[1] & 127); self.column = self.columns.0; }
                0x22 => { self.pages = (self.arguments[0] & 7, self.arguments[1] & 7); self.page = self.pages.0; }
                _ => {}
            }
            return;
        }
        self.command = byte;
        self.arguments.clear();
        match byte {
            0x00..=0x0f => self.column = (self.column & 0x70) | byte,
            0x10..=0x1f => self.column = (self.column & 15) | ((byte & 7) << 4),
            0x20 | 0x81 | 0x8d | 0xa8 | 0xd3 | 0xd5 | 0xd9 | 0xda | 0xdb => self.remaining = 1,
            0x21 | 0x22 | 0xa3 => self.remaining = 2,
            0x26 | 0x27 => self.remaining = 6,
            0x29 | 0x2a => self.remaining = 5,
            0x40..=0x7f => { self.start_line = byte & 63; self.version += 1; }
            0xa4 | 0xa5 => { self.all_on = byte == 0xa5; self.version += 1; }
            0xa6 | 0xa7 => { self.inverse = byte == 0xa7; self.version += 1; }
            0xae | 0xaf => { self.on = byte == 0xaf; self.version += 1; }
            0xb0..=0xb7 => self.page = byte & 7,
            _ => {}
        }
    }
    fn data(&mut self, byte: u8) {
        let index = self.page as usize * 128 + self.column as usize;
        if self.ram[index] != byte { self.ram[index] = byte; self.version += 1; }
        if self.mode == 1 {
            if self.page >= self.pages.1 {
                self.page = self.pages.0;
                self.column = if self.column >= self.columns.1 { self.columns.0 } else { self.column + 1 };
            } else { self.page += 1; }
        } else if self.mode == 0 {
            if self.column >= self.columns.1 {
                self.column = self.columns.0;
                self.page = if self.page >= self.pages.1 { self.pages.0 } else { self.page + 1 };
            } else { self.column += 1; }
        } else { self.column = (self.column + 1) & 127; }
    }
    pub fn frame(&self) -> Vec<u8> {
        let width = self.config.width as usize;
        let height = self.config.height as usize;
        let mut bits = vec![0; width * height / 8];
        if !self.on { return bits; }
        // ponytail: logical GDDRAM orientation matches the app framebuffer; physical SEG/COM mounting and scrolling need panel metadata and timed scanout.
        for y in 0..height {
            let row = (y + self.start_line as usize) % 64;
            for x in 0..width {
                let lit = self.all_on || ((self.ram[row / 8 * 128 + x] >> (row % 8) & 1 != 0) ^ self.inverse);
                if lit { bits[y / 8 * width + x] |= 1 << (y % 8); }
            }
        }
        bits
    }
}

pub struct Ssd1306I2c {
    pub state: Arc<Mutex<Ssd1306>>,
    control: bool,
    data: bool,
    continuation: bool,
}
impl Ssd1306I2c {
    pub fn new(state: Arc<Mutex<Ssd1306>>) -> Self { Self { state, control: true, data: false, continuation: false } }
}
impl I2cDevice for Ssd1306I2c {
    fn pins(&self) -> Option<(u8, u8)> { let state = self.state.lock().unwrap(); Some((state.config.sda, state.config.scl)) }
    fn start(&mut self, read: bool) -> bool { self.control = true; !read }
    fn write(&mut self, byte: u8) -> bool {
        if self.control {
            if byte & 0x3f != 0 { return false; }
            self.data = byte & 0x40 != 0;
            self.continuation = byte & 0x80 != 0;
            self.control = false;
        } else {
            let mut state = self.state.lock().unwrap();
            if self.data { state.data(byte); } else { state.command(byte); }
            self.control = self.continuation;
        }
        true
    }
    fn read(&mut self) -> u8 { 0xff }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn commands_and_wire_data_change_pixels_and_wrap_in_both_address_modes() {
        let state = Arc::new(Mutex::new(Ssd1306::new(OledConfig { id: 0, sda: 4, scl: 5, address: 0x3c, width: 128, height: 64 }).unwrap()));
        let mut device = Ssd1306I2c::new(state.clone());
        assert!(!device.start(true));
        assert!(device.start(false));
        for byte in [0, 0xaf, 0x20, 0, 0x21, 4, 5, 0x22, 1, 2] { assert!(device.write(byte)); }
        device.start(false);
        for byte in [0x40, 1, 2, 4, 8] { assert!(device.write(byte)); }
        let frame = state.lock().unwrap().frame();
        assert_eq!(&frame[132..134], &[1, 2]);
        assert_eq!(&frame[260..262], &[4, 8]);
        device.start(false);
        for byte in [0, 0x20, 1, 0x21, 4, 5, 0x22, 1, 2] { device.write(byte); }
        device.start(false);
        for byte in [0x40, 16, 32, 64, 128] { device.write(byte); }
        let frame = state.lock().unwrap().frame();
        assert_eq!(&frame[132..134], &[16, 64]);
        assert_eq!(&frame[260..262], &[32, 128]);
        device.start(false);
        for byte in [0x80, 0xa7, 0x80, 0xaf] { device.write(byte); }
        assert_eq!(state.lock().unwrap().frame()[132], !16);
        device.start(false); device.write(0); device.write(0xae);
        assert!(state.lock().unwrap().frame().iter().all(|byte| *byte == 0));
    }
}
