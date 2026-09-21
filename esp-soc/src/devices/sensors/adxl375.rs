use super::*;

const SCALE: f64 = 0.049 * 9.80665;
pub(super) struct Adxl375 {
    s: SampleState,
    next: Option<u64>,
}
impl Adxl375 {
    pub fn new(clock: Arc<AtomicU64>, hz: u32) -> Self {
        let mut s = SampleState::new(clock, hz);
        s.inputs[6] = 9.80665;
        s.regs[0] = 0xe5;
        s.regs[0x2c] = 0x0a;
        s.regs[0x30] = 2;
        Self { s, next: None }
    }
    fn period(&self) -> u64 {
        if self.s.regs[0x2d] & 4 != 0 {
            self.s.ticks(125_000 << (self.s.regs[0x2d] & 3)).max(1)
        } else {
            // Compute in clock ticks to retain the 312.5 us period at 3200 Hz.
            (self.s.hz * 256 / (25 << (self.s.regs[0x2c] & 15))).max(1)
        }
    }
}
impl RegisterSensor for Adxl375 {
    fn sync(&mut self) {
        self.s.time();
        let Some(at) = self.next else { return };
        if self.s.now < at {
            return;
        }
        let count = 1 + (self.s.now - at) / self.period();
        for axis in 0..3 {
            let offset = self.s.regs[0x1e + axis] as i8 as i32 * 4;
            let mut raw = ((self.s.inputs[4 + axis] / SCALE).round() as i32 + offset)
                .clamp(-4096, 4095) as i16;
            if self.s.regs[0x2d] & 4 == 0 && self.s.regs[0x2c] & 15 >= 14 {
                raw &= !1;
            }
            self.s.readings[4 + axis] = raw as f64 * SCALE;
            if self.s.regs[0x31] & 4 != 0 {
                raw <<= 3;
            }
            let reg = 0x32 + axis * 2;
            self.s.regs[reg..reg + 2].copy_from_slice(&raw.to_le_bytes());
        }
        if self.s.regs[0x2d] & 4 == 0 {
            if self.s.regs[0x30] & 0x80 != 0 || count > 1 {
                self.s.regs[0x30] |= 1;
            }
            self.s.regs[0x30] |= 0x80;
        }
        self.s.publish(count);
        self.next = Some(at + count * self.period());
    }
    fn registers(&self) -> [u8; 256] {
        self.s.regs
    }
    fn read_done(&mut self, reg: u8) {
        if (0x32..=0x37).contains(&reg) {
            self.s.regs[0x30] &= !0x81;
        }
    }
    fn write(&mut self, reg: u8, value: u16) -> bool {
        self.sync();
        let value = value as u8;
        let old = self.s.regs[reg as usize];
        match reg {
            0x1d..=0x27 | 0x2a if reg != 0x2a || value & 0xf0 == 0 => {}
            0x2c if value & 0xe0 == 0 => {}
            0x2d if value & 0xf0 == 0 => {}
            0x2e if value == 0 => {}
            0x2f => {}
            0x31 if value & 0xdb == 0x0b => {}
            0x38 if value == 0 => {}
            _ => return false,
        }
        self.s.regs[reg as usize] = value;
        if matches!(reg, 0x2c | 0x2d) && old != value {
            self.next = if self.s.regs[0x2d] & 8 == 0 {
                None
            } else {
                Some(
                    self.s.now
                        + self.period()
                        + if reg == 0x2d && (old & 8 == 0 || old & 4 != 0 && value & 4 == 0) {
                            self.s.ticks(1100)
                        } else {
                            0
                        },
                )
            };
            if self.s.regs[0x2d] & 4 != 0 {
                self.s.regs[0x30] &= !0x81;
            }
        }
        true
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        self.sync();
        if !(4..=6).contains(&field) || !value.is_finite() || !(-1961.33..=1961.33).contains(&value)
        {
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn adxl375_registers_timing_sleep_format_offsets_and_reset() {
        let clock = Arc::new(AtomicU64::new(0));
        let mut d = Adxl375::new(clock.clone(), 1_000_000);
        assert_eq!(d.registers()[0], 0xe5);
        assert_eq!(d.registers()[0x2c], 10);
        assert_eq!(d.registers()[0x2d], 0);
        assert!(d.set(4, 100. * SCALE));
        assert!(d.set(5, -101. * SCALE));
        assert!(d.set(6, 1961.33));
        assert!(!d.set(4, f64::NAN));
        assert!(!d.set(4, 1962.));
        assert!(!d.set(0, 25.));
        clock.store(100_000, Ordering::Relaxed);
        assert_eq!(d.generation(), 0);
        assert!(d.write(0x31, 0x0b));
        assert!(d.write(0x2d, 8));
        clock.store(111_099, Ordering::Relaxed);
        assert_eq!(d.generation(), 0);
        clock.store(111_100, Ordering::Relaxed);
        assert_eq!(d.generation(), 1);
        assert_eq!(&d.s.regs[0x32..0x36], &[100, 0, 155, 255]);
        assert_eq!(d.s.regs[0x30] & 0x81, 0x80);
        d.read_done(0x32);
        assert_eq!(d.s.regs[0x30] & 0x81, 0);
        assert!(d.write(0x1e, 255));
        clock.store(131_100, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.s.regs[0x32], 96);
        assert_eq!(d.s.regs[0x30] & 0x81, 0x81);
        assert!(d.write(0x2d, 0));
        let generation = d.generation();
        clock.store(1_000_000, Ordering::Relaxed);
        assert_eq!(d.generation(), generation);
        assert!(d.write(0x2d, 15));
        clock.store(2_001_100, Ordering::Relaxed);
        assert_eq!(d.generation(), generation + 1);
        assert_eq!(d.s.regs[0x30] & 0x81, 0);
        assert!(d.write(0x2d, 8));
        clock.store(2_012_199, Ordering::Relaxed);
        assert_eq!(d.generation(), generation + 1);
        clock.store(2_012_200, Ordering::Relaxed);
        assert_eq!(d.generation(), generation + 2);
        assert!(d.write(0x2c, 15));
        assert_eq!(d.period(), 312);
        assert!(d.write(0x31, 15));
        clock.store(2_020_000, Ordering::Relaxed);
        d.sync();
        assert_eq!(&d.s.regs[0x34..0x36], &(-102i16 * 8).to_le_bytes());
        for (reg, value) in [
            (0, 1),
            (0x32, 1),
            (0x38, 0x40),
            (0x2e, 0x40),
            (0x31, 0x8b),
            (0x2d, 0x38),
        ] {
            assert!(!d.write(reg, value));
        }
        let reset = Adxl375::new(clock, 1_000_000);
        assert_eq!(reset.s.regs[0x1e], 0);
        assert_eq!(reset.s.regs[0x31], 0);
        assert_eq!(reset.s.regs[0x2d], 0);
        assert!(reset.s.value(4).is_nan());
    }
    #[test]
    fn adxl375_i2c_snapshot_and_address_contract() {
        let config = SensorConfig {
            id: 0,
            sda: 4,
            scl: 5,
            address: 0x53,
            model: 39,
            shunt_milliohms: 0,
        };
        assert!(config.valid());
        assert!(SensorConfig {
            address: 0x1d,
            ..config
        }
        .valid());
        assert!(!SensorConfig {
            address: 0x54,
            ..config
        }
        .valid());
        let clock = Arc::new(AtomicU64::new(0));
        let state = Arc::new(Mutex::new(Sensor::new(config, clock.clone(), 1_000_000)));
        state.lock().unwrap().set(4, -100. * SCALE);
        let mut bus = SensorI2c::new(state);
        assert_eq!(bus.pins(), Some((4, 5)));
        assert!(bus.start(false));
        assert!(bus.write(0x2d));
        assert!(bus.write(8));
        bus.stop();
        clock.store(20_000, Ordering::Relaxed);
        assert!(bus.start(false));
        assert!(bus.write(0x32));
        assert!(bus.start(true));
        assert_eq!(
            (0..6).map(|_| bus.read()).collect::<Vec<_>>(),
            [156, 255, 0, 0, 20, 0]
        );
    }
}
