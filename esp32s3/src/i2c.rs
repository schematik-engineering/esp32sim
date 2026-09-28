//! The I2C bus devices the boards hang on the controller (`esp_periph::i2c::I2c`).
pub use esp_periph::i2c::*;

pub struct Ch32v003 { pub regs: [u8; 8], ptr: u8, first: bool, pub writes: u64 }
impl Default for Ch32v003 { fn default() -> Self { Self::new() } }

impl Ch32v003 {
    pub fn new() -> Self { let mut r = [0u8; 8]; r[2] = 0xff; r[4] = 0xff; Ch32v003 { regs: r, ptr: 0, first: true, writes: 0 } }
}
impl I2cDevice for Ch32v003 {
    fn start(&mut self, read: bool) -> bool { if !read { self.first = true; } true }
    fn write(&mut self, b: u8) -> bool { if self.first { self.ptr = b & 7; self.first = false; } else { self.regs[self.ptr as usize] = b; self.writes += 1; } true }
    fn read(&mut self) -> u8 { self.regs[self.ptr as usize] }
}

pub use esp_soc::devices::camera::{SensorState, Ov5640};

/// State of an ST7701S panel controller as seen through its 9-bit init SPI (D/C bit + 8 data bits).
#[derive(Default, Debug)]
pub struct St7701State { pub words: u64, pub last_cmd: u8, pub sleep_out: bool, pub display_on: bool, pub cmds: Vec<u8> }

/// TCA9554 / PCA9554 8-bit IO expander (regs: 0 input, 1 output, 2 polarity, 3 config). On the
/// Waveshare Touch-LCD-4B the panel's init SPI hangs off EXIO0 (CS), EXIO1 (MOSI), EXIO2 (CLK); the
/// device decodes that bit-banged stream into `St7701State`.
pub struct Tca9554 { pub regs: [u8; 4], ptr: u8, first: bool, panel: Option<std::sync::Arc<std::sync::Mutex<St7701State>>>, shift: u16, nbits: u8 }
impl Tca9554 {
    pub fn new(panel: std::sync::Arc<std::sync::Mutex<St7701State>>) -> Self { Tca9554 { regs: [0xff, 0xff, 0x00, 0xff], ptr: 0, first: true, panel: Some(panel), shift: 0, nbits: 0 } }
    /// Disconnected register-RAM initialization stub. External output effects are not modeled.
    pub fn register_ram_stub() -> Self { Tca9554 { regs: [0xff, 0xff, 0x00, 0xff], ptr: 0, first: true, panel: None, shift: 0, nbits: 0 } }
    fn input_port(&self) -> u8 { ((self.regs[1] & !self.regs[3]) | self.regs[3]) ^ self.regs[2] }
    fn output(&mut self, old: u8, new: u8) {
        let Some(panel) = self.panel.as_ref() else { return };
        let cs = new & 1 != 0; let mosi = (new >> 1) & 1; let clk_rise = new & 4 != 0 && old & 4 == 0;
        if cs { self.nbits = 0; self.shift = 0; return; }
        if clk_rise {
            self.shift = (self.shift << 1) | mosi as u16; self.nbits += 1;
            if self.nbits == 9 {
                let dc = self.shift & 0x100 != 0; let b = self.shift as u8; self.nbits = 0; self.shift = 0;
                let mut st = panel.lock().expect("ST7701 panel state mutex poisoned"); st.words += 1;
                if !dc { st.last_cmd = b; st.cmds.push(b); match b { 0x11 => st.sleep_out = true, 0x10 => st.sleep_out = false, 0x29 => st.display_on = true, 0x28 => st.display_on = false, _ => {} } }
            }
        }
    }
}
impl I2cDevice for Tca9554 {
    fn start(&mut self, read: bool) -> bool { if !read { self.first = true; } true }
    fn write(&mut self, b: u8) -> bool {
        if self.first { self.ptr = b & 3; self.first = false; }
        else if self.ptr != 0 { let old = self.regs[1]; self.regs[self.ptr as usize] = b; if self.ptr == 1 { self.output(old, b); } }
        true
    }
    fn read(&mut self) -> u8 { if self.ptr == 0 { self.input_port() } else { self.regs[self.ptr as usize] } }
}

pub use esp_soc::devices::touch::{TouchState, Gt911, Cst820};

#[cfg(test)]
mod tests {
    use super::*;

    fn read_registers(device: &mut Cst820, first: u8, count: usize) -> Vec<u8> {
        assert!(device.start(false));
        assert!(device.write(first));
        assert!(device.start(true));
        (0..count).map(|_| device.read()).collect()
    }

    #[test]
    fn cst820_reports_board_identity() {
        let mut device = Cst820::new(Default::default());
        assert_eq!(read_registers(&mut device, 0xa7, 3), [0xb7, 0x41, 0x02]);
    }

    #[test]
    fn cst820_reports_touch_coordinates() {
        let touch = std::sync::Arc::new(std::sync::Mutex::new(TouchState { down: true, x: 0x167, y: 0x1bf, ..Default::default() }));
        let mut device = Cst820::new(touch);
        assert_eq!(read_registers(&mut device, 0x02, 5), [1, 0x01, 0x67, 0x01, 0xbf]);
    }

    #[test]
    fn tca9554_register_stub_keeps_written_outputs() {
        let mut device = Tca9554::register_ram_stub();
        assert!(device.start(false));
        assert!(device.write(1));
        assert!(device.write(0xa5));
        assert!(device.start(false));
        assert!(device.write(1));
        assert!(device.start(true));
        assert_eq!(device.read(), 0xa5);
    }

    #[test]
    fn tca9554_input_port_is_read_only_and_reflects_pin_levels() {
        let mut device = Tca9554::register_ram_stub();
        assert!(device.start(false));
        assert!(device.write(0));
        assert!(device.write(0));
        assert_eq!(device.regs[0], 0xff);

        assert!(device.start(false));
        assert!(device.write(3));
        assert!(device.write(0xf0));
        assert!(device.start(false));
        assert!(device.write(1));
        assert!(device.write(0x05));
        assert!(device.start(false));
        assert!(device.write(0));
        assert!(device.start(true));
        assert_eq!(device.read(), 0xf5);
    }
}
