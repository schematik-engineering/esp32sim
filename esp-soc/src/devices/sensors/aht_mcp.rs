use super::*;

pub(super) struct Aht20 {
    s: SampleState,
    command: Vec<u8>,
    ready: u64,
    measuring: bool,
    sample: [f64; 2],
}
impl Aht20 {
    pub fn new(clock: Arc<AtomicU64>, hz: u32) -> Self {
        let mut s = SampleState::new(clock, hz);
        s.time();
        s.inputs[0] = 25.;
        s.inputs[1] = 50.;
        s.regs[0] = 0x88;
        let ready = s.now + s.ticks(40_000);
        Self {
            s,
            command: Vec::with_capacity(3),
            ready,
            measuring: false,
            sample: [25., 50.],
        }
    }
}
impl RegisterSensor for Aht20 {
    fn format(&self) -> WireFormat {
        WireFormat::Command
    }
    fn start(&mut self, read: bool) {
        if !read {
            self.command.clear();
        }
    }
    fn stop(&mut self) {
        self.command.clear();
    }
    fn sync(&mut self) {
        self.s.time();
        if self.s.now >= self.ready {
            self.s.regs[0] = 8;
            if self.measuring {
                self.measuring = false;
                let h = (self.sample[1] * 1048576. / 100.)
                    .round()
                    .clamp(0., 1048575.) as u32;
                let t = ((self.sample[0] + 50.) * 1048576. / 200.)
                    .round()
                    .clamp(0., 1048575.) as u32;
                self.s.regs[1..6].copy_from_slice(&[
                    (h >> 12) as u8,
                    (h >> 4) as u8,
                    ((h << 4) | (t >> 16)) as u8,
                    (t >> 8) as u8,
                    t as u8,
                ]);
                self.s.readings[0] = t as f64 * 200. / 1048576. - 50.;
                self.s.readings[1] = h as f64 * 100. / 1048576.;
                self.s.publish(1);
            }
        }
        self.s.regs[6] = temperature::crc8(&self.s.regs[..6], 0xff, 0x31);
    }
    fn registers(&self) -> [u8; 256] {
        self.s.regs
    }
    fn write(&mut self, _reg: u8, value: u16) -> bool {
        self.sync();
        let byte = value as u8;
        if self.command.is_empty() {
            if byte == 0xba {
                self.measuring = false;
                self.ready = self.s.now + self.s.ticks(20_000);
                self.s.regs = [0; 256];
                self.s.regs[0] = 0x88;
                self.s.readings = [f64::NAN; FIELD_COUNT];
                self.command.push(byte);
                return true;
            }
            if (self.s.now < self.ready && byte != 0x71) || !matches!(byte, 0xac | 0xbe | 0x71) {
                return false;
            }
            self.command.push(byte);
            return true;
        }
        let expected = match (self.command[0], self.command.len()) {
            (0xac, 1) => 0x33,
            (0xbe, 1) => 8,
            (0xac | 0xbe, 2) => 0,
            _ => return false,
        };
        if byte != expected {
            return false;
        }
        self.command.push(byte);
        if self.command.len() == 3 {
            self.measuring = self.command[0] == 0xac;
            self.sample = [self.s.inputs[0], self.s.inputs[1]];
            self.ready = self.s.now + self.s.ticks(if self.measuring { 80_000 } else { 10_000 });
            self.s.regs[0] = 0x88;
        }
        true
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        self.sync();
        let range = match field {
            0 => -40.0..=85.0,
            1 => 0.0..=100.0,
            _ => return false,
        };
        if !value.is_finite() || !range.contains(&value) {
            return false;
        }
        self.s.inputs[field as usize] = value;
        true
    }
    fn generation(&mut self) -> u32 {
        self.sync();
        self.s.generation
    }
    fn value(&mut self, field: u32) -> f64 {
        self.sync();
        self.s.value(field)
    }
}

pub(super) struct Mcp9808 {
    s: SampleState,
    next: Option<u64>,
}
impl Mcp9808 {
    pub fn new(clock: Arc<AtomicU64>, hz: u32) -> Self {
        let mut s = SampleState::new(clock, hz);
        s.time();
        s.inputs[0] = 25.;
        s.put16(6 * 4, 0x54);
        s.put16(7 * 4, 0x400);
        s.regs[8 * 4] = 3;
        let next = Some(s.now + s.ticks(250_000));
        Self { s, next }
    }
    fn period(&self) -> u64 {
        self.s
            .ticks([30_000, 65_000, 130_000, 250_000][self.s.regs[32] as usize])
    }
    fn signed(raw: u16) -> i32 {
        let raw = (raw & 0x1fff) as i32;
        if raw & 0x1000 != 0 {
            raw - 0x2000
        } else {
            raw
        }
    }
}
impl RegisterSensor for Mcp9808 {
    fn format(&self) -> WireFormat {
        WireFormat::Block
    }
    fn register_width(&self, reg: u8) -> u8 {
        if reg == 8 {
            1
        } else {
            2
        }
    }
    fn sync(&mut self) {
        self.s.time();
        if let Some(at) = self.next {
            if at <= self.s.now {
                let period = self.period();
                let count = 1 + (self.s.now - at) / period;
                let mask = (1i32 << (3 - self.s.regs[32])) - 1;
                let raw = ((self.s.inputs[0] * 16.).floor() as i32) & !mask;
                let mut word = (raw & 0x1fff) as u16;
                if raw >= Self::signed(self.s.get16(16)) {
                    word |= 0x8000;
                }
                if raw > Self::signed(self.s.get16(8)) {
                    word |= 0x4000;
                }
                if raw < Self::signed(self.s.get16(12)) {
                    word |= 0x2000;
                }
                self.s.put16(20, word as i32);
                self.s.readings[0] = raw as f64 / 16.;
                self.s.publish(count);
                self.next = if self.s.get16(4) & 0x100 != 0 {
                    None
                } else {
                    Some(at + count * period)
                };
            }
        }
    }
    fn registers(&self) -> [u8; 256] {
        self.s.regs
    }
    fn write(&mut self, reg: u8, value: u16) -> bool {
        self.sync();
        let cfg = self.s.get16(4);
        match reg {
            1 => {
                let locked = cfg & 0xc0;
                let mut value = value & 0x7cf;
                if locked != 0 {
                    value = (value & !0x60b) | (cfg & 0x60b);
                    value &= !(0x100 & !cfg);
                }
                if cfg & 0x40 != 0 {
                    value = (value & !4) | (cfg & 4);
                }
                value |= locked;
                self.s.put16(4, value as i32);
                if value & 0x100 != 0 {
                    self.next = None;
                }
                if cfg & 0x100 != 0 && value & 0x100 == 0 {
                    self.next = Some(self.s.now + self.period());
                }
                true
            }
            2 | 3 if cfg & 0x40 == 0 => {
                self.s.put16(reg as usize * 4, (value & 0x1ffc) as i32);
                true
            }
            4 if cfg & 0x80 == 0 => {
                self.s.put16(16, (value & 0x1ffc) as i32);
                true
            }
            8 => {
                self.s.regs[32] = value as u8 & 3;
                if cfg & 0x100 == 0 {
                    self.next = Some(self.s.now + self.period());
                }
                true
            }
            _ => false,
        }
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        self.sync();
        if field != 0 || !value.is_finite() || !(-40.0..=125.).contains(&value) {
            return false;
        }
        self.s.inputs[0] = value;
        true
    }
    fn generation(&mut self) -> u32 {
        self.sync();
        self.s.generation
    }
    fn value(&mut self, field: u32) -> f64 {
        self.sync();
        self.s.value(field)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn write_aht(sensor: &mut Aht20, bytes: &[u8]) -> bool {
        sensor.start(false);
        let ack = bytes.iter().all(|&b| sensor.write(0, b as u16));
        sensor.stop();
        ack
    }
    #[test]
    fn aht_timing_crc_snapshot_reset_and_validation() {
        let clock = Arc::new(AtomicU64::new(0));
        let mut d = Aht20::new(clock.clone(), 1_000_000);
        assert!(!write_aht(&mut d, &[0xac, 0x33, 0]));
        clock.store(40_000, Ordering::Relaxed);
        assert!(!write_aht(&mut d, &[0xe1, 8, 0]));
        assert!(write_aht(&mut d, &[0xbe, 8, 0]));
        assert_eq!(d.registers()[0], 0x88);
        clock.store(50_000, Ordering::Relaxed);
        assert!(d.set(0, -10.25));
        assert!(d.set(1, 73.5));
        assert!(!d.set(0, 86.));
        assert!(!d.set(1, f64::NAN));
        assert!(write_aht(&mut d, &[0xac, 0x33]));
        clock.store(200_000, Ordering::Relaxed);
        assert_eq!(d.generation(), 0);
    }
    #[test]
    fn aht_complete_command_and_independent_instances() {
        let clock = Arc::new(AtomicU64::new(40_000));
        let mut d = Aht20::new(clock.clone(), 1_000_000);
        let mut other = Aht20::new(clock.clone(), 1_000_000);
        clock.store(80_000, Ordering::Relaxed);
        d.set(0, -10.25);
        d.set(1, 73.5);
        assert!(write_aht(&mut d, &[0xac, 0x33, 0]));
        assert!(write_aht(&mut other, &[0xac, 0x33, 0]));
        d.set(0, 50.);
        clock.store(159_999, Ordering::Relaxed);
        assert_eq!(d.generation(), 0);
        assert_eq!(d.registers()[0], 0x88);
        clock.store(160_000, Ordering::Relaxed);
        assert_eq!(d.generation(), 1);
        let bytes = d.registers();
        assert_eq!(bytes[0], 8);
        assert_eq!(temperature::crc8(&bytes[..6], 0xff, 0x31), bytes[6]);
        assert!((d.value(0) + 10.25).abs() < 0.001);
        assert!((d.value(1) - 73.5).abs() < 0.001);
        assert!((other.value(0) - 25.).abs() < 0.001);
        assert!(write_aht(&mut d, &[0xba]));
        assert!(d.value(0).is_nan());
        clock.store(180_000, Ordering::Relaxed);
        assert!(write_aht(&mut d, &[0xac, 0x33, 0]));
        clock.store(260_000, Ordering::Relaxed);
        assert_eq!(d.value(0), 50.);
    }
    #[test]
    fn mcp_identity_mixed_width_signed_resolution_shutdown_and_locks() {
        let clock = Arc::new(AtomicU64::new(0));
        let config = SensorConfig {
            id: 0,
            sda: 4,
            scl: 5,
            address: 0x1f,
            model: 31,
            shunt_milliohms: 0,
        };
        assert!(config.valid());
        assert!(!SensorConfig {
            address: 0x20,
            ..config
        }
        .valid());
        let state = Arc::new(Mutex::new(Sensor::new(config, clock.clone(), 1_000_000)));
        let mut wire = SensorI2c::new(state.clone());
        assert_eq!(wire.pins(), Some((4, 5)));
        for (reg, expected) in [(6, [0, 0x54]), (7, [4, 0])] {
            assert!(wire.start(false));
            assert!(wire.write(reg));
            assert!(wire.start(true));
            assert_eq!([wire.read(), wire.read()], expected);
        }
        wire.start(false);
        wire.write(8);
        wire.start(true);
        assert_eq!([wire.read(), wire.read()], [3, 255]);
        let mut d = Mcp9808::new(clock.clone(), 1_000_000);
        d.set(0, -10.3125);
        clock.store(249_999, Ordering::Relaxed);
        assert_eq!(d.generation(), 0);
        clock.store(250_000, Ordering::Relaxed);
        assert_eq!(d.value(0), -10.3125);
        assert_eq!(Mcp9808::signed(d.s.get16(20)), -165);
        d.write(8, 0);
        clock.store(280_000, Ordering::Relaxed);
        assert_eq!(d.value(0), -10.5);
        d.write(1, 0x100);
        clock.store(310_000, Ordering::Relaxed);
        d.sync();
        let generation = d.generation();
        d.set(0, 60.);
        clock.store(1_000_000, Ordering::Relaxed);
        assert_eq!(d.generation(), generation);
        assert_eq!(d.value(0), -10.5);
        d.write(1, 0);
        clock.store(1_030_000, Ordering::Relaxed);
        assert_eq!(d.value(0), 60.);
        assert!(!d.set(1, 5.));
        assert!(!d.set(0, f64::INFINITY));
        d.write(2, 0x1234);
        d.write(1, 0xc0);
        assert!(!d.write(2, 0));
        assert!(!d.write(4, 0));
        d.write(1, 0);
        assert_eq!(d.s.get16(4) & 0xc0, 0xc0);
        let mut locked_shutdown = Mcp9808::new(clock.clone(), 1_000_000);
        locked_shutdown.write(1, 0x100);
        locked_shutdown.write(1, 0x1c0);
        locked_shutdown.write(1, 0);
        assert_eq!(locked_shutdown.s.get16(4), 0xc0);
        locked_shutdown.write(1, 0x100);
        assert_eq!(locked_shutdown.s.get16(4), 0xc0);
        assert!(!d.write(6, 0));
        assert_eq!(d.s.get16(24), 0x54);
    }
}
