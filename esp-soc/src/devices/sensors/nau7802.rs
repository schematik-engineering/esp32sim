use super::*;
pub(super) struct Nau7802 {
    s: SampleState,
    powered_at: Option<u64>,
    next: Option<u64>,
    calibration: Option<u64>,
}
impl Nau7802 {
    pub fn new(clock: Arc<AtomicU64>, hz: u32) -> Self {
        let mut d = Self {
            s: SampleState::new(clock, hz),
            powered_at: None,
            next: None,
            calibration: None,
        };
        d.s.inputs[52] = 1000.;
        d.s.inputs[53] = 2.;
        d.s.inputs[56] = 3.3;
        d.reset();
        d
    }
    fn reset(&mut self) {
        self.s.regs = [0; 256];
        self.s.regs[7] = 0x80;
        self.s.regs[14] = 0x80;
        self.next = None;
        self.powered_at = None;
        self.calibration = None;
        self.s.readings = [f64::NAN; FIELD_COUNT];
        self.s.generation = 0;
    }
    fn period(&self) -> Option<u64> {
        let rate = [10, 20, 40, 80, 0, 0, 0, 320][((self.s.regs[2] >> 4) & 7) as usize];
        (rate != 0).then(|| self.s.hz / rate)
    }
    fn operating(&self) -> bool {
        self.s.regs[0] & 7 == 6
    }
    fn restart(&mut self, cycles: u64) {
        self.next = if self.operating() {
            self.period().map(|p| self.s.now + p * cycles)
        } else {
            None
        };
        self.s.regs[0] &= !0x20;
    }
    fn channel(&self) -> usize {
        usize::from(self.s.regs[2] & 0x80 != 0)
    }
    fn reference(&self) -> f64 {
        if self.s.regs[0] & 0x80 != 0 {
            [4.5, 4.2, 3.9, 3.6, 3.3, 3., 2.7, 2.4][((self.s.regs[1] >> 3) & 7) as usize]
        } else {
            self.s.inputs[56]
        }
    }
    fn raw(&self) -> f64 {
        let reference = self.reference();
        let voltage = if self.channel() == 0 {
            (self.s.inputs[54] + self.s.inputs[51] / self.s.inputs[52] * self.s.inputs[53])
                * reference
                / 1000.
        } else {
            self.s.inputs[55] / 1000.
        };
        let gain = if self.s.regs[0x1b] & 0x10 != 0 {
            1
        } else {
            1u32 << (self.s.regs[1] & 7)
        };
        (voltage * gain as f64 * 2. / reference * 8388608.).clamp(-8388608., 8388607.)
    }
    fn offset(&self) -> i32 {
        let r = if self.channel() == 0 { 3 } else { 10 };
        ((u32::from_be_bytes([0, self.s.regs[r], self.s.regs[r + 1], self.s.regs[r + 2]]) << 8)
            as i32)
            >> 8
    }
    fn gain(&self) -> f64 {
        let r = if self.channel() == 0 { 6 } else { 13 };
        u32::from_be_bytes(self.s.regs[r..r + 4].try_into().unwrap()) as f64 / 8388608.
    }
    fn capture(&mut self, count: u64) {
        let raw = ((self.raw() + self.offset() as f64) * self.gain())
            .round()
            .clamp(-8388608., 8388607.) as i32;
        self.s.regs[0x12..=0x14].copy_from_slice(&raw.to_be_bytes()[1..]);
        self.s.regs[0] |= 0x20;
        self.s.readings[57] = raw as f64;
        for field in 51..=56 {
            self.s.readings[field] = self.s.inputs[field];
        }
        self.s.publish(count);
    }
    fn calibrate(&mut self) {
        let mode = self.s.regs[2] & 3;
        let r = if self.channel() == 0 { 3 } else { 10 };
        let mut error = false;
        match mode {
            0 => self.s.regs[r..r + 3].fill(0),
            2 => {
                let raw = (-self.raw()).round() as i32;
                self.s.regs[r..r + 3].copy_from_slice(&raw.to_be_bytes()[1..]);
            }
            3 => {
                let input = self.raw() + self.offset() as f64;
                let gain = 8388607. / input;
                if input <= 0. || gain >= 256. {
                    error = true;
                } else {
                    self.s.regs[r + 3..r + 7]
                        .copy_from_slice(&((gain * 8388608.).round() as u32).to_be_bytes());
                }
            }
            _ => error = true,
        }
        self.s.regs[2] = (self.s.regs[2] & !12) | if error { 8 } else { 0 };
        self.calibration = None;
        self.restart(4);
    }
}
impl RegisterSensor for Nau7802 {
    fn sync(&mut self) {
        self.s.time();
        if let Some(at) = self.powered_at {
            if self.s.now >= at {
                self.s.regs[0] |= 8;
                self.powered_at = None;
            }
        }
        if let Some(at) = self.calibration {
            if self.s.now >= at {
                self.calibrate();
            } else {
                return;
            }
        }
        if let (Some(next), Some(period)) = (self.next, self.period()) {
            if self.s.now >= next {
                let n = 1 + (self.s.now - next) / period;
                self.capture(n);
                self.next = Some(next + n * period);
            }
        }
    }
    fn registers(&self) -> [u8; 256] {
        self.s.regs
    }
    fn write(&mut self, reg: u8, value: u16) -> bool {
        self.sync();
        let v = value as u8;
        match reg {
            0 if v & 1 != 0 => {
                self.reset();
                self.s.regs[0] = 1;
            }
            0 => {
                let old = self.s.regs[0];
                self.s.regs[0] = (v & !0x28) | (old & 0x28);
                if v & 2 != 0 && old & 2 == 0 {
                    self.powered_at = Some(self.s.now + self.s.ticks(200));
                }
                if !self.operating() {
                    self.next = None;
                    self.calibration = None;
                    self.s.regs[0] &= !0x28;
                } else if old & 6 != 6 {
                    self.restart(6);
                } else if old & 0x10 == 0 && v & 0x10 != 0 && old & 0x20 == 0 {
                    self.restart(4);
                }
            }
            2 => {
                let active = self.calibration.is_some();
                self.s.regs[2] = (v & !12) | (self.s.regs[2] & 12);
                if !active && v & 4 != 0 {
                    self.s.regs[2] |= 4;
                    self.next = None;
                    self.calibration = Some(self.s.now + self.s.ticks(344000));
                } else if !active {
                    self.restart(4);
                }
            }
            1 | 3..=0x11 | 0x15 | 0x1b | 0x1c => {
                self.s.regs[reg as usize] = v;
                if reg == 1 || reg == 0x1b {
                    self.restart(4);
                }
            }
            _ => {}
        }
        true
    }
    fn read_done(&mut self, reg: u8) {
        if reg == 0x12 {
            self.s.regs[0] &= !0x20;
        }
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        let range = match field {
            51 => -1_000_000.0..=1_000_000.0,
            52 => 1.0..=1_000_000.0,
            53 => 0.001..=10.0,
            54 => -10.0..=10.0,
            55 => -100.0..=100.0,
            56 => 0.1..=5.5,
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
    fn physical_bridge_gain_channel_timing_and_calibration() {
        let clock = Arc::new(AtomicU64::new(0));
        let mut d = Nau7802::new(clock.clone(), 1_000_000);
        d.set(51, 250.);
        d.set(54, 0.1);
        d.write(0, 0x86);
        d.write(1, 0x27);
        d.write(2, 0x30);
        clock.store(199, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.s.regs[0] & 8, 0);
        clock.store(200, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.s.regs[0] & 8, 8);
        clock.store(49999, Ordering::Relaxed);
        assert_eq!(d.generation(), 0);
        clock.store(50000, Ordering::Relaxed);
        assert!((d.value(57) - 1288490.).abs() < 2.);
        assert_eq!(d.s.regs[0] & 0x20, 0x20);
        d.read_done(0x12);
        assert_eq!(d.s.regs[0] & 0x20, 0);
        d.write(1, 0x26);
        clock.store(100000, Ordering::Relaxed);
        assert!((d.value(57) - 644245.).abs() < 2.);
        d.set(55, -1.);
        d.write(2, 0xb0);
        clock.store(150000, Ordering::Relaxed);
        assert!((d.value(57) + 325376.).abs() < 2.);
        d.write(2, 0xb6);
        assert_eq!(d.s.regs[2] & 4, 4);
        clock.store(494000, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.s.regs[2] & 12, 0);
        clock.store(544000, Ordering::Relaxed);
        assert!(d.value(57).abs() < 2.);
        d.write(0, 0x80);
        d.set(55, 10.);
        clock.store(1_000_000, Ordering::Relaxed);
        assert!(d.value(57).abs() < 2.);
        d.write(0, 1);
        assert_eq!(d.s.regs[0x15], 0);
        assert_eq!(d.generation(), 0);
        assert_eq!(d.s.inputs[51], 250.);
    }
}
