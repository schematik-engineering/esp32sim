use super::*;

// ICM-42688-P DS-000347 §§3,14–16: nominal I2C UI-register measurements.
pub(super) struct Icm42688 {
    s: SampleState,
    bank: u8,
    filters: [u8; 2],
    next: [Option<u64>; 3],
    ready_at: [u64; 3],
    reset_at: Option<u64>,
    write_after: u64,
}
impl Icm42688 {
    pub fn new(clock: Arc<AtomicU64>, hz: u32) -> Self {
        let mut d = Self {
            s: SampleState::new(clock, hz),
            bank: 0,
            filters: [0; 2],
            next: [None; 3],
            ready_at: [0; 3],
            reset_at: None,
            write_after: 0,
        };
        d.s.inputs[0] = 25.;
        d.s.inputs[6] = 9.80665;
        d.reset();
        d
    }
    fn reset(&mut self) {
        self.s.regs = [0; 256];
        for (r, v) in [
            (0x13, 5),
            (0x2d, 0x10),
            (0x4c, 0x30),
            (0x4d, 0x91),
            (0x4f, 6),
            (0x50, 6),
            (0x51, 0x16),
            (0x52, 0x11),
            (0x53, 0x0d),
            (0x54, 0x23),
            (0x64, 0x10),
            (0x65, 0x10),
            (0x75, 0x47),
        ] {
            self.s.regs[r] = v;
        }
        for reg in (0x1d..=0x29).step_by(2) {
            self.s.regs[reg] = 0x80;
        }
        self.s.readings = [f64::NAN; FIELD_COUNT];
        self.bank = 0;
        self.filters = [0xa0, 0x30];
        self.next = [None; 3];
        self.ready_at = [0; 3];
        self.write_after = 0;
    }
    fn period(&self, group: usize) -> Option<u64> {
        let power = self.s.regs[0x4e];
        if group == 2 {
            return if power & 0x20 != 0 {
                None
            } else {
                [self.period(0), self.period(1)].into_iter().flatten().min()
            };
        }
        let mode = if group == 0 {
            power & 3
        } else {
            (power >> 2) & 3
        };
        let config = self.s.regs[if group == 0 { 0x50 } else { 0x4f }];
        let odr = config & 15;
        if (group == 0
            && (mode < 2
                || config >> 5 > 3
                || (mode == 2 && odr < 7)
                || (mode == 3 && (12..=14).contains(&odr))))
            || (group == 1 && (mode != 3 || (12..=14).contains(&odr)))
        {
            return None;
        }
        let rate16 = [
            0, 512000, 256000, 128000, 64000, 32000, 16000, 3200, 1600, 800, 400, 200, 100, 50, 25,
            8000,
        ][odr as usize];
        (rate16 != 0).then(|| (self.s.hz * 16 / rate16).max(1))
    }
    fn scale(&self, group: usize) -> f64 {
        if group == 0 {
            (16. * 9.80665 / 32768.) / f64::from(1u32 << (self.s.regs[0x50] >> 5))
        } else {
            (2000f64.to_radians() / 32768.) / f64::from(1u32 << (self.s.regs[0x4f] >> 5))
        }
    }
    fn sample(&mut self, group: usize) {
        let fields = if group == 0 {
            4..7
        } else if group == 1 {
            7..10
        } else {
            0..1
        };
        for (axis, field) in fields.enumerate() {
            let (scale, offset, reg) = if group == 2 {
                (1. / 132.48, 25., 0x1d)
            } else {
                (self.scale(group), 0., 0x1f + 6 * group + 2 * axis)
            };
            let minimum = if group != 2 && self.s.regs[0x4c] & 0x80 == 0 {
                -32766.
            } else {
                -32768.
            };
            let raw = ((self.s.inputs[field] - offset) / scale)
                .round()
                .clamp(minimum, 32767.) as i16;
            let bytes = if self.s.regs[0x4c] & 0x10 != 0 {
                raw.to_be_bytes()
            } else {
                raw.to_le_bytes()
            };
            self.s.regs[reg..reg + 2].copy_from_slice(&bytes);
            self.s.readings[field] = f64::from(raw) * scale + offset;
        }
        self.s.regs[0x2d] |= 8;
    }
}
impl RegisterSensor for Icm42688 {
    fn sync(&mut self) {
        self.s.time();
        if let Some(at) = self.reset_at {
            if self.s.now < at {
                return;
            }
            self.reset_at = None;
            self.reset();
        }
        let mut count = 0;
        for group in 0..3 {
            if let (Some(at), Some(period)) = (self.next[group], self.period(group)) {
                if self.s.now >= at {
                    let n = 1 + (self.s.now - at) / period;
                    self.sample(group);
                    self.next[group] = Some(at + n * period);
                    count = count.max(n);
                }
            }
        }
        if count != 0 {
            self.s.publish(count);
        }
    }
    fn address_ready(&self) -> bool {
        self.reset_at.is_none() && self.s.regs[0x4c] & 3 != 3
    }
    fn registers(&self) -> [u8; 256] {
        let mut regs = if self.bank == 0 {
            self.s.regs
        } else {
            [0; 256]
        };
        if self.bank == 1 {
            regs[0x0b] = self.filters[0];
        }
        if self.bank == 2 {
            regs[0x03] = self.filters[1];
        }
        regs[0x76] = self.bank;
        regs
    }
    fn write(&mut self, reg: u8, value: u16) -> bool {
        self.sync();
        if self.reset_at.is_some() || self.s.now < self.write_after {
            return true;
        }
        let value = value as u8;
        if reg == 0x76 {
            if value <= 4 {
                self.bank = value;
            }
            return true;
        }
        if self.bank != 0 {
            match (self.bank, reg) {
                (1, 0x0b) => self.filters[0] = value,
                (2, 0x03) => self.filters[1] = value,
                _ => {}
            }
            return true;
        }
        match reg {
            0x11 if value & 1 != 0 => {
                self.reset();
                self.s.regs[0x11] = 1;
                self.s.regs[0x2d] = 0;
                self.reset_at = Some(self.s.now + self.s.ticks(1000));
            }
            0x4c => self.s.regs[0x4c] = value,
            0x4e..=0x50 => {
                let old_power = self.s.regs[0x4e];
                let before = [self.period(0), self.period(1), self.period(2)];
                self.s.regs[reg as usize] = value;
                for (group, old) in before.into_iter().enumerate() {
                    let after = self.period(group);
                    if old != after {
                        if old.is_none() && after.is_some() {
                            self.ready_at[group] =
                                self.s.now + self.s.ticks([10000, 30000, 14000][group]);
                        }
                        self.next[group] =
                            after.map(|p| (self.s.now + p).max(self.ready_at[group]));
                    }
                }
                if reg == 0x4e
                    && ((old_power & 3 < 2 && value & 3 >= 2)
                        || (old_power & 12 == 0 && value & 12 != 0))
                {
                    self.write_after = self.s.now + self.s.ticks(200);
                }
            }
            _ => {}
        }
        true
    }
    fn read_done(&mut self, reg: u8) {
        if self.bank == 0 && reg == 0x2d {
            self.s.regs[0x2d] = 0;
        }
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        let range = match field {
            0 => -40.0..=85.,
            4..=6 => -156.9064..=156.9064,
            7..=9 => -2000f64.to_radians()..=2000f64.to_radians(),
            _ => return false,
        };
        if !value.is_finite() || !range.contains(&value) {
            return false;
        }
        self.sync();
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
    fn device() -> Icm42688 {
        Icm42688::new(Arc::new(AtomicU64::new(0)), 1_000_000)
    }
    fn time(d: &mut Icm42688, at: u64) {
        d.s.clock.store(at, Ordering::Relaxed);
        d.sync();
    }
    #[test]
    fn icm42688_reset_banks_startup_write_guard_ready_and_poweroff() {
        let mut d = device();
        assert_eq!(d.registers()[0x75], 0x47);
        assert!(d.value(4).is_nan());
        d.write(0x75, 0);
        assert_eq!(d.registers()[0x75], 0x47);
        d.write(0x76, 1);
        d.write(0x0b, 0xa3);
        assert_eq!(d.registers()[0x0b], 0xa3);
        assert_eq!(d.registers()[0x75], 0);
        d.write(0x76, 2);
        d.write(3, 0x31);
        assert_eq!(d.registers()[3], 0x31);
        d.write(0x76, 0);
        d.write(0x4e, 15);
        d.write(0x50, 0x66);
        assert_eq!(d.s.regs[0x50], 6);
        time(&mut d, 199);
        d.write(0x50, 0x66);
        assert_eq!(d.s.regs[0x50], 6);
        time(&mut d, 200);
        d.write(0x50, 0x66);
        assert_eq!(d.s.regs[0x50], 0x66);
        time(&mut d, 9999);
        assert!(d.value(6).is_nan());
        time(&mut d, 10000);
        assert_eq!(d.value(6), 9.80665);
        assert!(d.value(0).is_nan());
        assert!(d.value(7).is_nan());
        time(&mut d, 14000);
        assert_eq!(d.value(0), 25.);
        time(&mut d, 29999);
        assert!(d.value(7).is_nan());
        time(&mut d, 30000);
        assert_eq!(d.value(7), 0.);
        assert_eq!(d.s.regs[0x2d] & 8, 8);
        d.read_done(0x2d);
        assert_eq!(d.s.regs[0x2d], 0);
        time(&mut d, 50000);
        d.write(0x4e, 0x20);
        let gen = d.generation();
        d.set(6, 0.);
        time(&mut d, 60000);
        assert_eq!(d.generation(), gen);
        assert_eq!(d.value(6), 9.80665);
        d.write(0x11, 1);
        assert!(!d.address_ready());
        assert!(d.value(6).is_nan());
        time(&mut d, 60999);
        assert!(!d.address_ready());
        time(&mut d, 61000);
        assert!(d.address_ready());
        assert_eq!(d.s.regs[0x11], 0);
        assert_eq!(d.s.regs[0x2d], 16);
        assert_eq!(d.s.inputs[6], 0.);
        d.write(0x76, 1);
        assert_eq!(d.registers()[0x0b], 0xa0);
        d.write(0x76, 2);
        assert_eq!(d.registers()[3], 0x30);
    }
    #[test]
    fn icm42688_physical_scales_saturation_endian_and_odr_legality() {
        for accfs in 0..4 {
            for gyrofs in 0..8 {
                let mut d = device();
                d.write(0x50, (accfs << 5) | 6);
                d.write(0x4f, (gyrofs << 5) | 6);
                for (f, v) in [
                    (0, -10.),
                    (4, 1.234),
                    (5, -2.345),
                    (6, 3.456),
                    (7, 0.12),
                    (8, -0.23),
                    (9, 0.24),
                ] {
                    assert!(d.set(f, v));
                }
                d.write(0x4e, 15);
                time(&mut d, 30000);
                for f in 0..7 {
                    let field = if f == 0 { 0 } else { f + 3 };
                    let reg = 0x1d + 2 * f;
                    let raw = i16::from_be_bytes([d.s.regs[reg], d.s.regs[reg + 1]]);
                    let scale = if f == 0 {
                        1. / 132.48
                    } else {
                        d.scale(usize::from(f > 3))
                    };
                    let offset = if f == 0 { 25. } else { 0. };
                    assert!((d.value(field as u32) - d.s.inputs[field]).abs() <= scale / 2. + 1e-9);
                    assert_eq!(d.value(field as u32), f64::from(raw) * scale + offset);
                }
                d.set(4, -156.9064);
                time(&mut d, 31000);
                assert_eq!(d.value(4), -32766. * d.scale(0));
                d.write(0x4c, 0xa0);
                time(&mut d, 32000);
                assert_eq!(i16::from_le_bytes([d.s.regs[0x1f], d.s.regs[0x20]]), -32768);
            }
        }
        let mut d = device();
        for (f, v) in [
            (4, f64::NAN),
            (7, f64::INFINITY),
            (0, 85.1),
            (5, 157.),
            (8, 35.),
            (29, 1.),
        ] {
            assert!(!d.set(f, v));
        }
        d.write(0x4e, 3);
        time(&mut d, 200);
        d.write(0x50, 12);
        assert!(d.period(0).is_none());
        d.write(0x4e, 2);
        assert_eq!(d.period(0), Some(160000));
        time(&mut d, 400);
        d.write(0x50, 1);
        assert!(d.period(0).is_none());
        d.write(0x4f, 12);
        d.write(0x4e, 15);
        assert!(d.period(1).is_none());
    }
    #[test]
    fn icm42688_odr_change_retains_startup_deadline_and_standby_write_delay() {
        let mut d = device();
        assert_eq!(&d.registers()[0x1d..0x21], &[0x80, 0, 0x80, 0]);
        d.write(0x4e, 15);
        time(&mut d, 200);
        d.write(0x4f, 1);
        d.write(0x50, 1);
        time(&mut d, 9999);
        assert!(d.value(4).is_nan());
        time(&mut d, 10000);
        assert!(d.value(4).is_finite());
        assert!(d.value(7).is_nan());
        time(&mut d, 29999);
        assert!(d.value(7).is_nan());
        time(&mut d, 30000);
        assert!(d.value(7).is_finite());
        time(&mut d, 60000);
        d.write(0x4e, 0x20);
        d.write(0x4e, 0x24);
        d.write(0x4f, 8);
        assert_eq!(d.s.regs[0x4f], 1);
        time(&mut d, 60199);
        d.write(0x4f, 8);
        assert_eq!(d.s.regs[0x4f], 1);
        time(&mut d, 60200);
        d.write(0x4f, 8);
        assert_eq!(d.s.regs[0x4f], 8);
        assert!(d.period(1).is_none());
        d.write(0x4c, 0x33);
        assert!(!d.address_ready());
    }
    #[test]
    fn icm42688_i2c_two_addresses_and_coherent_burst_snapshot() {
        for address in [0x68, 0x69] {
            let clock = Arc::new(AtomicU64::new(0));
            let config = SensorConfig {
                id: 0,
                sda: 4,
                scl: 5,
                address,
                model: 50,
                shunt_milliohms: 0,
            };
            assert!(config.valid());
            let sensor = Arc::new(Mutex::new(Sensor::new(config, clock.clone(), 1_000_000)));
            let mut bus = SensorI2c::new(sensor.clone());
            assert!(bus.matches_address(address, address, false));
            assert!(!bus.matches_address(address, 0x6a, false));
            bus.start(false);
            bus.write(0x4e);
            bus.write(15);
            bus.stop();
            clock.store(30000, Ordering::Relaxed);
            sensor.lock().unwrap().set(4, 9.80665);
            clock.store(31000, Ordering::Relaxed);
            bus.start(false);
            bus.write(0x1f);
            bus.start(true);
            let hi = bus.read();
            sensor.lock().unwrap().set(4, -9.80665);
            clock.store(32000, Ordering::Relaxed);
            let lo = bus.read();
            assert_eq!(i16::from_be_bytes([hi, lo]), 2048);
        }
    }
}
