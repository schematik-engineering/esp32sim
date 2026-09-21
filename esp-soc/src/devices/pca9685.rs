use esp_periph::i2c::I2cDevice;
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub id: u8,
    pub sda: u8,
    pub scl: u8,
    pub address: u8,
    pub oe: u8,
    pub oscillator_hz: u32,
    pub external_hz: u32,
}
impl Config {
    pub fn valid(&self) -> bool {
        self.id < 4
            && self.sda < 49
            && self.scl < 49
            && self.sda != self.scl
            && (self.oe == 255 || self.oe < 49 && self.oe != self.sda && self.oe != self.scl)
            && (0x40..=0x7f).contains(&self.address)
            && self.address != 0x70
            && (10_000_000..=50_000_000).contains(&self.oscillator_hz)
            && self.external_hz <= 50_000_000
    }
}
pub struct Pca9685 {
    pub config: Config,
    hz: u64,
    now: u64,
    ready: u64,
    epoch: u64,
    regs: [u8; 256],
    active: [[u8; 4]; 16],
    dirty: [u8; 16],
    pending: [Option<(u64, [u8; 4])>; 16],
    oe_high: bool,
    pointer: u8,
}
impl Pca9685 {
    pub fn new(config: Config, hz: u64) -> Result<Self, String> {
        if !config.valid() || hz == 0 {
            return Err("invalid PCA9685 wiring or clock".into());
        }
        let mut s = Self {
            config,
            hz,
            now: 0,
            ready: 0,
            epoch: 0,
            regs: [0; 256],
            active: [[0; 4]; 16],
            dirty: [0; 16],
            pending: [None; 16],
            oe_high: false,
            pointer: 0,
        };
        s.reset();
        Ok(s)
    }
    fn reset(&mut self) {
        self.regs = [0; 256];
        self.regs[0] = 0x11;
        self.regs[1] = 4;
        self.regs[2..6].copy_from_slice(&[0xe2, 0xe4, 0xe8, 0xe0]);
        self.regs[0xfe] = 30;
        for channel in 0..16 {
            self.regs[9 + channel * 4] = 0x10;
            self.active[channel] = [0, 0, 0, 0x10];
        }
        self.pending = [None; 16];
        self.dirty = [0; 16];
        self.pointer = 0;
        self.epoch = self.now;
    }
    pub fn set_clock(&mut self, hz: u32) -> bool {
        if !(10_000_000..=50_000_000).contains(&hz) {
            return false;
        }
        self.config.oscillator_hz = hz;
        true
    }
    pub fn drive(&mut self, enabled: u64, output: u64) {
        self.oe_high = self.config.oe != 255
            && enabled & (1u64 << self.config.oe) != 0
            && output & (1u64 << self.config.oe) != 0;
    }
    pub fn advance(&mut self, now: u64) {
        self.now = now;
        for (active, pending) in self.active.iter_mut().zip(&mut self.pending) {
            if pending.is_some_and(|(at, _)| at <= now) {
                *active = pending.take().unwrap().1;
            }
        }
    }
    fn clock(&self) -> u32 {
        if self.regs[0] & 0x40 != 0 {
            self.config.external_hz
        } else {
            self.config.oscillator_hz
        }
    }
    pub fn pwm(&self, channel: u8) -> Option<(f64, u32)> {
        let b = *self.active.get(channel as usize)?;
        if self.regs[0] & 0x10 != 0 || self.now < self.ready || self.clock() == 0 {
            return None;
        }
        let freq = self.clock() as f64 / (4096. * (self.regs[0xfe] as f64 + 1.));
        if self.oe_high {
            return match self.regs[1] & 3 {
                0 => Some((freq, 0)),
                1 if self.regs[1] & 4 != 0 => Some((freq, 65535)),
                _ => None,
            };
        }
        let on = u16::from_le_bytes([b[0], b[1] & 15]);
        let off = u16::from_le_bytes([b[2], b[3] & 15]);
        let mut ticks = if b[3] & 16 != 0 {
            0
        } else if b[1] & 16 != 0 {
            4096
        } else {
            off.wrapping_sub(on) & 4095
        };
        if self.regs[1] & 16 != 0 {
            ticks = 4096 - ticks;
        }
        Some((freq, (ticks as u32 * 65535 + 2048) / 4096))
    }
    fn latch(&mut self, channel: usize) {
        let bytes = self.regs[6 + channel * 4..10 + channel * 4]
            .try_into()
            .unwrap();
        let clock = self.clock() as u64;
        let period = if clock == 0 {
            0
        } else {
            (self.hz * 4096 * (self.regs[0xfe] as u64 + 1)).div_ceil(clock)
        };
        let at = if self.regs[0] & 0x10 != 0 || period == 0 {
            self.now
        } else {
            let old_on =
                u16::from_le_bytes([self.active[channel][0], self.active[channel][1] & 15]) as u64;
            let on_cycle = (self.hz * old_on * (self.regs[0xfe] as u64 + 1)).div_ceil(clock);
            let at = self.epoch + self.now.saturating_sub(self.epoch) / period * period + on_cycle;
            if at <= self.now {
                at + period
            } else {
                at
            }
        };
        self.pending[channel] = Some((at, bytes));
        self.dirty[channel] = 0;
        if self.regs[0] & 0x80 != 0 {
            self.regs[0] &= !0x80;
        }
    }
    fn write_register(&mut self, reg: u8, value: u8) -> bool {
        match reg {
            0 => {
                let old = self.regs[0];
                let ext = (old & 0x40) | (if old & 0x10 != 0 { value & 0x40 } else { 0 });
                let restart = if value & 0x80 != 0 {
                    0
                } else if value & 0x10 != 0
                    && old & 0x10 == 0
                    && self.active.iter().any(|b| b[3] & 16 == 0)
                {
                    0x80
                } else {
                    old & 0x80
                };
                self.regs[0] = (value & 0x3f) | ext | restart;
                if old & 0x10 != 0 && value & 0x10 == 0 {
                    self.ready = self.now
                        + if ext == 0 {
                            (self.hz * 500).div_ceil(1_000_000)
                        } else {
                            0
                        };
                    self.epoch = self.ready;
                }
                if value & 0x80 != 0 && self.now >= self.ready {
                    self.epoch = self.now;
                }
            }
            1 => self.regs[1] = value & 0x1f,
            2..=5 => self.regs[reg as usize] = value & 0xfe,
            6..=0x45 => {
                let channel = (reg as usize - 6) / 4;
                let part = (reg as usize - 6) % 4;
                self.regs[reg as usize] = if part & 1 != 0 { value & 31 } else { value };
                self.dirty[channel] |= 1 << part;
                if self.regs[1] & 8 != 0 && self.dirty[channel] == 15 {
                    self.latch(channel);
                }
            }
            0xfa..=0xfd => {
                let part = reg as usize - 0xfa;
                for ch in 0..16 {
                    self.write_register((6 + ch * 4 + part) as u8, value);
                }
            }
            0xfe => {
                if self.regs[0] & 0x10 != 0 {
                    self.regs[0xfe] = value.max(3);
                }
            }
            _ => return false,
        }
        true
    }
    fn increment(&mut self) {
        if self.regs[0] & 0x20 != 0 {
            self.pointer = match self.pointer {
                0x45 | 0xfe => 0,
                _ => self.pointer.wrapping_add(1),
            };
        }
    }
}
pub struct PcaI2c {
    state: Arc<Mutex<Pca9685>>,
    pointer_next: bool,
    reset: bool,
}
impl PcaI2c {
    pub fn new(state: Arc<Mutex<Pca9685>>) -> Self {
        Self {
            state,
            pointer_next: true,
            reset: false,
        }
    }
}
impl I2cDevice for PcaI2c {
    fn pins(&self) -> Option<(u8, u8)> {
        let c = self.state.lock().unwrap().config;
        Some((c.sda, c.scl))
    }
    fn matches_address(&self, configured: u8, address: u8, read: bool) -> bool {
        let s = self.state.lock().unwrap();
        address == configured
            || address == 0 && !read
            || (0..4).any(|n| {
                s.regs[0] & (1 << n) != 0 && address == s.regs[if n == 0 { 5 } else { 5 - n }] / 2
            })
    }
    fn start_address(&mut self, address: u8, read: bool) -> bool {
        self.reset = address == 0;
        self.pointer_next = !read;
        true
    }
    fn write(&mut self, byte: u8) -> bool {
        let mut s = self.state.lock().unwrap();
        if self.reset {
            if byte == 6 {
                s.reset();
                return true;
            }
            return false;
        }
        if self.pointer_next {
            self.pointer_next = false;
            s.pointer = byte;
            return byte <= 0x45 || (0xfa..=0xfe).contains(&byte);
        }
        let pointer = s.pointer;
        let ack = s.write_register(pointer, byte);
        s.increment();
        ack
    }
    fn read(&mut self) -> u8 {
        let mut s = self.state.lock().unwrap();
        let value = if s.pointer >= 0xfa && s.pointer <= 0xfd {
            0
        } else {
            s.regs[s.pointer as usize]
        };
        s.increment();
        value
    }
    fn stop(&mut self) {
        let mut s = self.state.lock().unwrap();
        if s.regs[1] & 8 == 0 {
            for ch in 0..16 {
                if s.dirty[ch] != 0 {
                    s.latch(ch);
                }
            }
        }
        let now = s.now;
        s.advance(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> Config {
        Config {
            id: 0,
            sda: 4,
            scl: 5,
            address: 0x40,
            oe: 6,
            oscillator_hz: 25_000_000,
            external_hz: 0,
        }
    }
    fn write(bus: &mut PcaI2c, reg: u8, bytes: &[u8]) {
        bus.start_address(0x40, false);
        assert!(bus.write(reg));
        for b in bytes {
            assert!(bus.write(*b));
        }
        bus.stop();
    }
    fn setup() -> (Arc<Mutex<Pca9685>>, PcaI2c) {
        let s = Arc::new(Mutex::new(Pca9685::new(config(), 1_000_000).unwrap()));
        let mut bus = PcaI2c::new(s.clone());
        write(&mut bus, 0, &[0x31]);
        write(&mut bus, 0xfe, &[121]);
        write(&mut bus, 0, &[0x21]);
        s.lock().unwrap().advance(500);
        (s, bus)
    }
    #[test]
    fn real_prescale_wrap_full_bits_stop_commit_oe_and_clock_calibration() {
        let (s, mut bus) = setup();
        write(&mut bus, 6, &[0xa0, 15, 0xf0, 0]);
        assert_eq!(s.lock().unwrap().pwm(0).unwrap().1, 0);
        s.lock().unwrap().advance(21_000);
        let (freq, duty) = s.lock().unwrap().pwm(0).unwrap();
        assert!((freq - 50.0288).abs() < 0.001);
        assert_eq!(duty, 5376);
        s.lock().unwrap().drive(1 << 6, 1 << 6);
        assert_eq!(s.lock().unwrap().pwm(0).unwrap().1, 0);
        s.lock().unwrap().drive(1 << 6, 0);
        assert!(s.lock().unwrap().set_clock(27_000_000));
        assert!(s.lock().unwrap().pwm(0).unwrap().0 > 54.);
        write(&mut bus, 6, &[0, 16, 0, 16]);
        s.lock().unwrap().advance(42_000);
        assert_eq!(s.lock().unwrap().pwm(0).unwrap().1, 0);
        write(&mut bus, 6, &[0, 16, 0, 0]);
        s.lock().unwrap().advance(63_000);
        assert_eq!(s.lock().unwrap().pwm(0).unwrap().1, 65535);
    }
    #[test]
    fn register_pointer_ai_prescale_sleep_external_clock_and_general_reset() {
        let (s, mut bus) = setup();
        write(&mut bus, 0xfe, &[3]);
        assert_eq!(s.lock().unwrap().regs[0xfe], 121);
        write(&mut bus, 0, &[0x10]);
        write(&mut bus, 6, &[9, 8]);
        assert_eq!(s.lock().unwrap().regs[6], 8);
        assert_eq!(s.lock().unwrap().regs[7], 0);
        write(&mut bus, 0, &[0x50]);
        write(&mut bus, 0xfe, &[1]);
        assert_eq!(s.lock().unwrap().regs[0xfe], 3);
        write(&mut bus, 0, &[0x20]);
        assert_eq!(s.lock().unwrap().regs[0] & 0x40, 0x40);
        assert!(s.lock().unwrap().pwm(0).is_none());
        assert!(bus.matches_address(0x40, 0, false));
        assert!(!bus.matches_address(0x40, 0, true));
        bus.start_address(0, false);
        assert!(!bus.write(5));
        bus.start_address(0, false);
        assert!(bus.write(6));
        assert_eq!(s.lock().unwrap().regs[0], 0x11);
        assert_eq!(s.lock().unwrap().regs[0xfe], 30);
        assert!(bus.matches_address(0x40, 0x70, false));
        assert!(!bus.matches_address(0x40, 0x71, false));
        write(&mut bus, 0, &[0x19]);
        assert!(bus.matches_address(0x40, 0x71, true));
    }
    #[test]
    fn ack_latching_requires_four_bytes_and_identity_clock_bounds() {
        let (s, mut bus) = setup();
        write(&mut bus, 1, &[12]);
        write(&mut bus, 6, &[0, 0, 44]);
        s.lock().unwrap().advance(21_000);
        assert_eq!(s.lock().unwrap().pwm(0).unwrap().1, 0);
        write(&mut bus, 9, &[1]);
        s.lock().unwrap().advance(42_000);
        assert_eq!(s.lock().unwrap().pwm(0).unwrap().1, 4800);
        assert!(!s.lock().unwrap().set_clock(9_999_999));
        assert!(Pca9685::new(
            Config {
                address: 0x70,
                ..config()
            },
            1_000_000
        )
        .is_err());
    }
}
