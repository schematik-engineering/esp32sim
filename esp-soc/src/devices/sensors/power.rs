use super::*;
pub(super) struct Ina219 {
    s: SampleState,
    next: Option<u64>,
    shunt: u16,
}
impl Ina219 {
    pub fn new(clock: Arc<AtomicU64>, hz: u32, shunt: u16) -> Self {
        let mut d = Self {
            s: SampleState::new(clock, hz),
            next: None,
            shunt: if shunt == 0 { 100 } else { shunt },
        };
        d.s.inputs[10] = 5.;
        d.s.inputs[12] = 100.;
        d.reset();
        d
    }
    fn reset(&mut self) {
        self.s.regs = [0; 256];
        self.s.put16(0, 0x399f);
        self.s.readings = [f64::NAN; FIELD_COUNT];
        self.s.generation = 0;
        self.next = Some(self.s.now + self.period());
    }
    fn adc_us(v: u16) -> u64 {
        if v & 8 != 0 {
            [532, 1060, 2130, 4260, 8510, 17020, 34050, 68100][(v & 7) as usize]
        } else {
            [84, 148, 276, 532][(v & 3) as usize]
        }
    }
    fn period(&self) -> u64 {
        let c = self.s.get16(0);
        let mode = c & 7;
        self.s
            .ticks(
                if mode & 1 != 0 {
                    Self::adc_us((c >> 3) & 15)
                } else {
                    0
                } + if mode & 2 != 0 {
                    Self::adc_us((c >> 7) & 15)
                } else {
                    0
                },
            )
            .max(1)
    }
    fn capture(&mut self, count: u64) {
        let c = self.s.get16(0);
        let mode = c & 7;
        let gain = 1u32 << ((c >> 11) & 3);
        if mode & 1 != 0 {
            let input = self.s.inputs[12] * self.shunt as f64 / 1000.;
            let limit = 40. * gain as f64;
            let raw = (input.clamp(-limit, limit) / 0.01)
                .round()
                .clamp(-32768., 32767.) as i32;
            self.s.put16(2, raw);
            self.s.readings[11] = raw as f64 * 0.01;
        }
        if mode & 2 != 0 {
            let max = if c & 0x2000 != 0 { 32. } else { 16. };
            let input = self.s.inputs[10];
            let raw = (input.min(max) / 0.004).round() as i32;
            self.s.put16(4, (raw << 3) | 2);
            self.s.readings[10] = raw as f64 * 0.004;
        }
        self.calculate();
        self.s.readings[15] = self.shunt as f64;
        self.s.publish(count);
    }
    fn calculate(&mut self) {
        let calibration = self.s.get16(10) & !1;
        let shunt = self.s.get16(2) as i16 as i64;
        let bus = (self.s.get16(4) >> 3) as i64;
        let current = shunt * calibration as i64 / 4096;
        let power = current * bus / 5000;
        let overflow = !(-32768..=32767).contains(&current) || !(-65535..=65535).contains(&power);
        self.s.put16(8, current as i32);
        self.s.put16(6, power as i32);
        let voltage = (self.s.get16(4) & !1) | u16::from(overflow);
        self.s.put16(4, voltage as i32);
        if calibration != 0 && !overflow {
            let lsb = 0.04096 / (calibration as f64 * self.shunt as f64 / 1000.);
            self.s.readings[12] = (current as i16) as f64 * lsb * 1000.;
            self.s.readings[13] = if power >= 0 {
                power as f64 * lsb * 20000.
            } else {
                f64::NAN
            };
        } else {
            self.s.readings[12] = f64::NAN;
            self.s.readings[13] = f64::NAN;
        }
    }
}
impl RegisterSensor for Ina219 {
    fn format(&self)->WireFormat{WireFormat::Word}
    fn sync(&mut self) {
        self.s.time();
        let Some(next) = self.next else { return };
        if self.s.now < next {
            return;
        }
        let continuous = self.s.get16(0) & 4 != 0;
        let period = self.period();
        let count = if continuous {
            1 + (self.s.now - next) / period
        } else {
            1
        };
        self.capture(count);
        self.next = if continuous {
            Some(next + count * period)
        } else {
            None
        };
    }
    fn registers(&self) -> [u8; 256] {
        self.s.regs
    }
    fn write(&mut self, reg: u8, value: u16) -> bool {
        self.sync();
        match reg {
            0 if value & 0x8000 != 0 => self.reset(),
            0 => {
                self.s.put16(0, (value & 0x3fff) as i32);
                if value & 3 != 0 {
                    let bus = self.s.get16(4) & !2;
                    self.s.put16(4, bus as i32);
                }
                self.next = if value & 3 != 0 {
                    Some(self.s.now + self.period())
                } else {
                    None
                };
            }
            5 => {
                self.s.put16(10, (value & !1) as i32);
                self.calculate();
            }
            _ => {}
        }
        true
    }
    fn read_done(&mut self, reg: u8) {
        if reg == 3 {
            let bus = self.s.get16(4) & !2;
            self.s.put16(4, bus as i32);
        }
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        if !value.is_finite() {
            return false;
        }
        match field {
            10 if (0.0..=32.0).contains(&value) => {
                self.sync();
                self.s.inputs[10] = value;
            }
            11 if (-320.0..=320.0).contains(&value) => {
                self.sync();
                self.s.inputs[12] = value * 1000. / self.shunt as f64;
            }
            12 if value.abs() * self.shunt as f64 / 1000. <= 320. => {
                self.sync();
                self.s.inputs[12] = value;
            }
            15 if value.fract() == 0.
                && (1.0..=65535.0).contains(&value)
                && self.s.inputs[12].abs() * value / 1000. <= 320. =>
            {
                self.sync();
                self.shunt = value as u16;
            }
            _ => return false,
        }
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
    fn timed_adc_calibration_and_physical_shunt_are_independent() {
        let clock = Arc::new(AtomicU64::new(0));
        let mut d = Ina219::new(clock.clone(), 1_000_000, 100);
        d.set(10, 12.);
        d.set(12, 250.);
        clock.store(1063, Ordering::Relaxed);
        assert_eq!(d.generation(), 0);
        clock.store(1064, Ordering::Relaxed);
        assert_eq!(d.generation(), 1);
        assert_eq!(d.s.get16(2), 2500);
        assert_eq!(d.s.get16(4), 3000 << 3 | 2);
        assert_eq!(d.s.get16(8), 0);
        d.write(5, 4097);
        assert_eq!(d.s.get16(10), 4096);
        assert_eq!(d.s.get16(8), 2500);
        assert_eq!(d.s.get16(6), 1500);
        assert!((d.value(12) - 250.).abs() < 1e-9);
        assert!((d.value(13) - 3000.).abs() < 1e-9);
        d.write(5, 8192);
        assert_eq!(d.s.get16(8), 5000);
        assert_eq!(d.generation(), 1);
        assert!((d.value(12) - 250.).abs() < 1e-9);
        d.read_done(3);
        assert_eq!(d.s.get16(4) & 2, 0);
        assert!(d.set(15, 50.));
        assert!(!d.set(15, 65535.));
        assert!(!d.set(12, 6401.));
        clock.store(2128, Ordering::Relaxed);
        assert_eq!(d.generation(), 2);
        assert_eq!(d.s.get16(2), 1250);
        assert_eq!(d.s.get16(8), 2500);
        assert!((d.value(12) - 250.).abs() < 1e-9);
        d.write(0, 0x3998);
        assert_eq!(d.s.get16(4) & 2, 2);
        clock.store(1000000, Ordering::Relaxed);
        assert_eq!(d.generation(), 2);
    }
    #[test]
    fn pga_clipping_is_separate_from_math_overflow_and_trigger_is_one_shot() {
        let clock = Arc::new(AtomicU64::new(0));
        let mut d = Ina219::new(clock.clone(), 1_000_000, 100);
        d.set(12, 1000.);
        d.write(5, 4096);
        d.write(0, 0x019b);
        clock.store(1064, Ordering::Relaxed);
        assert_eq!(d.generation(), 1);
        assert_eq!(d.s.get16(2), 4000);
        assert_eq!(d.s.get16(4) & 1, 0);
        assert!((d.value(12) - 400.).abs() < 1e-9);
        clock.store(2000000, Ordering::Relaxed);
        assert_eq!(d.generation(), 1);
        d.write(5, 65534);
        assert_eq!(d.s.get16(4) & 1, 1);
        assert!(d.value(12).is_nan());
        d.write(5, 4096);
        assert_eq!(d.s.get16(4) & 1, 0);
        d.set(12, -100.);
        d.write(0, 0x399b);
        clock.store(2001064, Ordering::Relaxed);
        assert!((d.value(12) + 100.).abs() < 1e-9);
    }
}
