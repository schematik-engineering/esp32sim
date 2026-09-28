use super::*;
pub(super) struct Mpu6050 {
    s: SampleState,
    next: Option<u64>,
}
impl Mpu6050 {
    pub fn new(clock: Arc<AtomicU64>, hz: u32) -> Self {
        let mut d = Self {
            s: SampleState::new(clock, hz),
            next: None,
        };
        d.s.inputs[0] = 30.;
        d.s.inputs[6] = 9.80665;
        d.reset();
        d
    }
    fn reset(&mut self) {
        self.s.regs = [0; 256];
        self.s.regs[0x75] = 0x68;
        self.s.regs[0x6b] = 0x40;
        self.s.readings = [f64::NAN; FIELD_COUNT];
        self.s.generation = 0;
        self.next = None;
    }
    fn period(&self) -> u64 {
        let dlpf = self.s.regs[0x1a] & 7;
        self.s.hz * (self.s.regs[0x19] as u64 + 1) / if matches!(dlpf, 0 | 7) { 8000 } else { 1000 }
    }
    fn capture(&mut self, count: u64) {
        let acc = 16384.0 / (1u32 << ((self.s.regs[0x1c] >> 3) & 3)) as f64;
        let gyro = [131., 65.5, 32.8, 16.4][((self.s.regs[0x1b] >> 3) & 3) as usize];
        for axis in 0..3 {
            let a = (self.s.inputs[4 + axis] / 9.80665 * acc)
                .round()
                .clamp(-32768., 32767.) as i32;
            let g = (self.s.inputs[7 + axis].to_degrees() * gyro)
                .round()
                .clamp(-32768., 32767.) as i32;
            self.s.put16(0x3b + axis * 2, a);
            self.s.put16(0x43 + axis * 2, g);
            self.s.readings[4 + axis] = a as f64 / acc * 9.80665;
            self.s.readings[7 + axis] = (g as f64 / gyro).to_radians();
        }
        if self.s.regs[0x6b] & 8 == 0 {
            let t = ((self.s.inputs[0] - 36.53) * 340.).round() as i32;
            self.s.put16(0x41, t);
            self.s.readings[0] = t as f64 / 340. + 36.53;
        } else {
            self.s.readings[0] = f64::NAN;
        }
        self.s.regs[0x3a] |= 1;
        self.s.publish(count);
    }
}
impl RegisterSensor for Mpu6050 {
    fn sync(&mut self) {
        self.s.time();
        let Some(next) = self.next else { return };
        if self.s.now < next {
            return;
        }
        let period = self.period().max(1);
        let count = 1 + (self.s.now - next) / period;
        self.capture(count);
        self.next = Some(next + count * period);
    }
    fn registers(&self) -> [u8; 256] {
        self.s.regs
    }
    fn write(&mut self, reg: u8, value: u16) -> bool {
        self.sync();
        let v = value as u8;
        match reg {
            0x6b if v & 0x80 != 0 => self.reset(),
            0x6b => {
                self.s.regs[reg as usize] = v & 0x7f;
                self.next = if v & 0x40 == 0 {
                    Some(self.s.now + self.period().max(1))
                } else {
                    None
                };
            }
            0x19..=0x1c => {
                self.s.regs[reg as usize] = v;
                if self.next.is_some() {
                    self.next = Some(self.s.now + self.period().max(1));
                }
            }
            0x68 => {}
            0x6a => self.s.regs[reg as usize] = v & !7,
            0x06..=0x0b | 0x13..=0x18 | 0x1f..=0x38 | 0x63..=0x67 | 0x69 | 0x6c => {
                self.s.regs[reg as usize] = v
            }
            _ => {}
        }
        true
    }
    fn read_done(&mut self, reg: u8) {
        if reg == 0x3a || self.s.regs[0x37] & 0x10 != 0 {
            self.s.regs[0x3a] = 0;
        }
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        let range = match field {
            0 => -40.0..=85.0,
            4..=6 => -156.9064..=156.9064,
            7..=9 => -34.90658504..=34.90658504,
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
    #[test]
    fn conversion_registers_follow_clock_sleep_and_firmware_ranges() {
        let clock = Arc::new(AtomicU64::new(0));
        let mut d = Mpu6050::new(clock.clone(), 1_000_000);
        assert_eq!(d.registers()[0x75], 0x68);
        assert_eq!(d.generation(), 0);
        assert!(d.set(4, 9.80665));
        assert!(d.set(7, 1.0_f64.to_radians()));
        assert!(!d.set(4, 160.));
        assert!(!d.set(10, 1.));
        d.write(0x1a, 3);
        d.write(0x19, 4);
        d.write(0x6b, 1);
        clock.store(4999, Ordering::Relaxed);
        assert_eq!(d.generation(), 0);
        clock.store(5000, Ordering::Relaxed);
        assert_eq!(d.generation(), 1);
        assert_eq!(d.s.get16(0x3b), 16384);
        assert_eq!(d.s.get16(0x43), 131);
        assert_eq!(d.registers()[0x3a], 1);
        d.read_done(0x3a);
        assert_eq!(d.registers()[0x3a], 0);
        d.write(0x6b, 0x41);
        clock.store(50000, Ordering::Relaxed);
        assert_eq!(d.generation(), 1);
        d.write(0x1c, 0x18);
        d.write(0x1b, 0x18);
        d.write(0x6b, 1);
        clock.store(55000, Ordering::Relaxed);
        assert_eq!(d.generation(), 2);
        assert_eq!(d.s.get16(0x3b), 2048);
        assert_eq!(d.s.get16(0x43), 16);
        d.set(4, 9.80665 * 8.);
        d.write(0x1c, 0);
        clock.store(60000, Ordering::Relaxed);
        assert!((d.value(4) - 32767. / 16384. * 9.80665).abs() < 1e-9);
        d.write(0x6b, 0x80);
        assert_eq!(d.registers()[0x6b], 0x40);
        assert!(d.value(4).is_nan());
    }
}
