use super::*;

pub(super) struct Bmm150 {
    s: SampleState,
    next: Option<u64>,
    ready_at: u64,
    reset_at: Option<u64>,
}
impl Bmm150 {
    pub fn new(clock: Arc<AtomicU64>, hz: u32) -> Self {
        let mut d = Self {
            s: SampleState::new(clock, hz),
            next: None,
            ready_at: 0,
            reset_at: None,
        };
        d.reset(false);
        d
    }
    fn reset(&mut self, powered: bool) {
        self.s.regs = [0; 256];
        self.s.regs[0x40] = 0x32;
        self.s.regs[0x4b] = u8::from(powered);
        self.s.regs[0x4c] = 6;
        self.s.regs[0x4d] = 0x3f;
        self.s.regs[0x4e] = 7;
        // Fixed nominal trims: XY raw × 0.375 µT, Z raw × 0.25 µT at RHALL=10000.
        self.s.regs[0x64] = 32;
        self.s.regs[0x65] = 32;
        for (r, v) in [(0x68, (-1808i16) as u16), (0x6a, 32768), (0x6c, 10000)] {
            self.s.regs[r..r + 2].copy_from_slice(&v.to_le_bytes());
        }
        self.s.readings = [f64::NAN; FIELD_COUNT];
        self.next = None;
    }
    fn duration(&self) -> u64 {
        self.s
            .ticks(
                145 * (2 * u64::from(self.s.regs[0x51]) + 1)
                    + 500 * (u64::from(self.s.regs[0x52]) + 1)
                    + 980,
            )
            .max(1)
    }
    fn period(&self) -> u64 {
        (self.s.hz / [10, 2, 6, 8, 15, 20, 25, 30][((self.s.regs[0x4c] >> 3) & 7) as usize])
            .max(self.duration())
    }
    fn sample(&mut self) {
        for a in 0..3 {
            if self.s.regs[0x4e] & (8 << a) != 0 {
                continue;
            }
            let (scale, limit, shift) = if a < 2 {
                (0.375, 4095., 3)
            } else {
                (0.25, 16383., 1)
            };
            let raw = (self.s.inputs[29 + a] / scale).round().clamp(-limit, limit) as i16;
            let bytes = ((raw as u16) << shift).to_le_bytes();
            self.s.regs[0x42 + a * 2..0x44 + a * 2].copy_from_slice(&bytes);
            self.s.readings[29 + a] = f64::from(raw) * scale;
        }
        let rhall = if self.s.regs[0x4e] & 0x20 == 0 {
            10000u16 << 2
        } else {
            0
        };
        self.s.regs[0x48..0x4a].copy_from_slice(&(rhall | 1).to_le_bytes());
    }
}
impl RegisterSensor for Bmm150 {
    fn sync(&mut self) {
        self.s.time();
        if let Some(at) = self.reset_at {
            if self.s.now < at {
                return;
            }
            self.reset_at = None;
            self.reset(true);
        }
        if let Some(at) = self.next {
            if self.s.now >= at && self.s.regs[0x4b] & 1 != 0 {
                let normal = self.s.regs[0x4c] & 6 == 0;
                let n = if normal {
                    1 + (self.s.now - at) / self.period()
                } else {
                    1
                };
                self.sample();
                self.s.publish(n);
                self.next = if normal {
                    Some(at + n * self.period())
                } else {
                    self.s.regs[0x4c] = (self.s.regs[0x4c] & !6) | 6;
                    None
                };
            }
        }
    }
    fn registers(&self) -> [u8; 256] {
        if self.s.regs[0x4b] & 1 == 0 || self.s.now < self.ready_at || self.reset_at.is_some() {
            let mut r = [0; 256];
            r[0x4b] = self.s.regs[0x4b];
            r
        } else {
            self.s.regs
        }
    }
    fn write(&mut self, r: u8, v: u16) -> bool {
        self.sync();
        let v = v as u8;
        if r == 0x4b {
            let powered = self.s.regs[0x4b] & 1 != 0;
            if !powered && v & 1 != 0 {
                self.reset(true);
                self.ready_at = self.s.now + self.s.ticks(3000);
            } else if v & 1 == 0 {
                self.reset(false);
            } else if v & 0x82 == 0x82 {
                self.s.regs[0x4b] = v;
                self.reset_at = Some(self.s.now + self.s.ticks(1000));
                self.next = None;
            }
            return true;
        }
        if self.s.regs[0x4b] & 1 == 0 || self.s.now < self.ready_at || self.reset_at.is_some() {
            return true;
        }
        match r {
            0x4c => {
                self.s.regs[r as usize] = v;
                self.next = if v & 6 == 0 || v & 6 == 2 {
                    Some(self.s.now + self.duration())
                } else {
                    None
                };
            }
            0x4d..=0x52 => self.s.regs[r as usize] = v,
            _ => {}
        }
        true
    }
    fn read_done(&mut self, r: u8) {
        if (0x42..=0x49).contains(&r) {
            self.s.regs[0x48] &= !1;
        }
    }
    fn set(&mut self, f: u32, v: f64) -> bool {
        let max = match f {
            29 | 30 => 1300.,
            31 => 2500.,
            _ => return false,
        };
        if !v.is_finite() || v.abs() > max {
            return false;
        }
        self.sync();
        self.s.inputs[f as usize] = v;
        true
    }
    fn generation(&mut self) -> u32 {
        self.sync();
        self.s.generation
    }
    fn value(&mut self, f: u32) -> f64 {
        self.sync();
        self.s.value(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn device() -> Bmm150 {
        Bmm150::new(Arc::new(AtomicU64::new(0)), 1_000_000)
    }
    fn time(d: &mut Bmm150, at: u64) {
        d.s.clock.store(at, Ordering::Relaxed);
        d.sync();
    }
    fn power(d: &mut Bmm150) {
        d.write(0x4b, 1);
        time(d, 3000);
    }
    #[test]
    fn bmi_bmm150_power_forced_timing_readiness_and_reset() {
        let mut d = device();
        assert_eq!(d.registers()[0x40], 0);
        d.write(0x4b, 1);
        time(&mut d, 2999);
        assert_eq!(d.registers()[0x40], 0);
        time(&mut d, 3000);
        assert_eq!(d.registers()[0x40], 0x32);
        assert_eq!(d.registers()[0x4e], 7);
        assert_eq!(d.duration(), 1625);
        d.set(29, 30.);
        d.set(30, -45.);
        d.set(31, 60.);
        d.write(0x4c, 2);
        time(&mut d, 4624);
        assert!(d.value(29).is_nan());
        time(&mut d, 4625);
        assert_eq!(d.value(29), 30.);
        assert_eq!(d.value(30), -45.);
        assert_eq!(d.value(31), 60.);
        assert_eq!(d.s.regs[0x4c] & 6, 6);
        assert_eq!(d.s.regs[0x48] & 1, 1);
        d.read_done(0x42);
        assert_eq!(d.s.regs[0x48] & 1, 0);
        d.set(29, 60.);
        time(&mut d, 100000);
        assert_eq!(d.value(29), 30.);
        d.write(0x4b, 0x83);
        time(&mut d, 100999);
        assert_eq!(d.registers()[0x40], 0);
        time(&mut d, 101000);
        assert_eq!(d.registers()[0x40], 0x32);
        assert_eq!(d.s.regs[0x4b], 1);
        assert!(d.value(29).is_nan());
        assert_eq!(d.s.inputs[29], 60.);
    }
    #[test]
    fn bmi_bmm150_compensated_physical_values_repetitions_and_saturation() {
        let mut d = device();
        power(&mut d);
        d.write(0x51, 4);
        d.write(0x52, 14);
        assert_eq!(d.duration(), 9785);
        d.write(0x4c, 0);
        for (f, v) in [(29, 123.4), (30, -234.5), (31, 456.7)] {
            assert!(d.set(f, v));
        }
        time(&mut d, 12785);
        for a in 0..3 {
            let r = 0x42 + 2 * a;
            let packed = i16::from_le_bytes([d.s.regs[r], d.s.regs[r + 1]]);
            let raw = packed >> if a < 2 { 3 } else { 1 };
            let scale = if a < 2 { 0.375 } else { 0.25 };
            assert_eq!(d.value((29 + a) as u32), f64::from(raw) * scale);
            assert!((d.value((29 + a) as u32) - d.s.inputs[29 + a]).abs() <= scale / 2.);
            let compensated = if a < 2 {
                ((i32::from(raw) * 6) as i16) / 16
            } else {
                (i32::from(raw) * 4).clamp(-32767, 32767) as i16 / 16
            };
            assert_eq!(compensated as f64, d.value((29 + a) as u32).trunc());
        }
        d.set(31, 2500.);
        time(&mut d, 112785);
        assert_eq!(d.value(31), 2500.);
        let old = d.value(29);
        d.write(0x4e, 15);
        d.set(29, 1.);
        time(&mut d, 212785);
        assert_eq!(d.value(29), old);
        for (f, v) in [(29, 1300.1), (30, f64::NAN), (31, 2500.1), (0, 25.)] {
            assert!(!d.set(f, v));
        }
    }
    #[test]
    fn bmi_bmm150_address_straps_and_factory_trim_readonly() {
        for address in 0x10..=0x13 {
            let c = SensorConfig {
                id: 0,
                sda: 4,
                scl: 5,
                address,
                model: 52,
                shunt_milliohms: 0,
            };
            assert!(c.valid());
        }
        let mut d = device();
        power(&mut d);
        let before = d.registers();
        d.write(0x64, 0);
        d.write(0x40, 0);
        assert_eq!(d.registers(), before);
        let mut other = device();
        power(&mut other);
        d.set(29, 30.);
        other.set(29, -45.);
        d.write(0x4c, 2);
        other.write(0x4c, 2);
        time(&mut d, 4625);
        time(&mut other, 4625);
        assert_eq!(d.value(29), 30.);
        assert_eq!(other.value(29), -45.);
        d.write(0x4b, 0);
        assert_eq!(d.registers()[0x40], 0);
        assert!(d.value(29).is_nan());
    }
}
