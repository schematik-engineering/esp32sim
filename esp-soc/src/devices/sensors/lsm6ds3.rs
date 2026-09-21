use super::*;

pub(super) struct Lsm6ds3 {
    s: SampleState,
    next: [Option<u64>; 3],
    reset_at: Option<u64>,
    unread: u16,
}
impl Lsm6ds3 {
    pub fn new(clock: Arc<AtomicU64>, hz: u32) -> Self {
        let mut d = Self {
            s: SampleState::new(clock, hz),
            next: [None; 3],
            reset_at: None,
            unread: 0,
        };
        d.s.inputs[0] = 25.;
        d.s.inputs[6] = 9.80665;
        d.reset();
        d
    }
    fn reset(&mut self) {
        self.s.regs = [0; 256];
        self.s.regs[0xf] = 0x69;
        self.s.regs[0x12] = 4;
        self.s.readings = [f64::NAN; FIELD_COUNT];
        self.s.generation = 0;
        self.next = [None; 3];
        self.unread = 0;
    }
    fn period(&self, group: usize) -> Option<u64> {
        if group == 2 {
            return (self.s.regs[0x10] >> 4 != 0 || self.s.regs[0x11] >> 4 != 0)
                .then(|| self.s.hz / 52);
        }
        let odr = (self.s.regs[0x10 + group] >> 4) as usize;
        let twice_hz = [0, 25, 52, 104, 208, 416, 832, 1666, 3332, 6660, 13320];
        if odr == 0 || odr > if group == 0 { 10 } else { 8 } {
            None
        } else {
            Some((self.s.hz * 2 / twice_hz[odr]).max(1))
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
        let acc =
            [0.061, 0.488, 0.122, 0.244][((self.s.regs[0x10] >> 2) & 3) as usize] * 9.80665 / 1000.;
        let gyro = if self.s.regs[0x11] & 2 != 0 {
            4.375
        } else {
            [8.75, 17.5, 35., 70.][((self.s.regs[0x11] >> 2) & 3) as usize]
        } * std::f64::consts::PI
            / 180000.;
        for (axis, field) in fields.enumerate() {
            let reg = if group == 0 {
                0x28 + axis * 2
            } else if group == 1 {
                0x22 + axis * 2
            } else {
                0x20
            };
            let mask = 3u16 << (reg - 0x20);
            if self.s.regs[0x12] & 0x40 != 0 && self.unread & mask != 0 {
                continue;
            }
            let scale = if group == 0 {
                acc
            } else if group == 1 {
                gyro
            } else {
                1. / 16.
            };
            let offset = if group == 2 { 25. } else { 0. };
            let raw = ((self.s.inputs[field] - offset) / scale)
                .round()
                .clamp(-32768., 32767.) as i16;
            let bytes = if self.s.regs[0x12] & 2 != 0 {
                raw.to_be_bytes()
            } else {
                raw.to_le_bytes()
            };
            self.s.regs[reg..reg + 2].copy_from_slice(&bytes);
            self.unread |= mask;
            self.s.readings[field] = raw as f64 * scale + offset;
        }
        self.s.regs[0x1e] |= 1 << group;
    }
}
impl RegisterSensor for Lsm6ds3 {
    fn sync(&mut self) {
        self.s.time();
        if let Some(end) = self.reset_at {
            if self.s.now < end {
                return;
            }
            self.reset_at = None;
            self.reset();
        }
        let mut count = 0;
        for group in 0..3 {
            if let (Some(next), Some(period)) = (self.next[group], self.period(group)) {
                if self.s.now >= next {
                    let n = 1 + (self.s.now - next) / period;
                    self.sample(group);
                    self.next[group] = Some(next + n * period);
                    count = count.max(n);
                }
            }
        }
        if count > 0 {
            self.s.publish(count);
        }
    }
    fn registers(&self) -> [u8; 256] {
        self.s.regs
    }
    fn next_address(&self, reg: u8) -> u8 {
        if self.s.regs[0x12] & 4 != 0 {
            reg.wrapping_add(1)
        } else {
            reg
        }
    }
    fn write(&mut self, reg: u8, value: u16) -> bool {
        self.sync();
        let v = value as u8;
        match reg {
            0x12 if v & 1 != 0 => {
                self.reset();
                self.s.regs[0x12] |= 1;
                self.reset_at = Some(self.s.now + self.s.ticks(50));
            }
            0x10..=0x19 => {
                self.s.regs[reg as usize] = v;
                if reg == 0x10 || reg == 0x11 {
                    for g in 0..3 {
                        self.next[g] = self.period(g).map(|p| self.s.now + p);
                    }
                }
            }
            _ => {}
        }
        true
    }
    fn read_done(&mut self, reg: u8) {
        if (0x20..=0x2d).contains(&reg) {
            self.unread &= !(1 << (reg - 0x20));
            let (group, mask) = if reg < 0x22 {
                (2, 3)
            } else if reg < 0x28 {
                (1, 0xfc)
            } else {
                (0, 0x3f00)
            };
            if self.unread & mask == 0 {
                self.s.regs[0x1e] &= !(1 << group);
            }
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
    fn lsm_ranges_clock_bdu_auto_increment_and_reset() {
        let clock = Arc::new(AtomicU64::new(0));
        let mut d = Lsm6ds3::new(clock.clone(), 1_000_000);
        assert_eq!(d.registers()[0xf], 0x69);
        assert_eq!(d.generation(), 0);
        assert!(d.value(4).is_nan());
        d.set(4, 9.80665);
        d.set(7, 1f64.to_radians());
        d.write(0x10, 0x40);
        d.write(0x11, 0x40);
        clock.store(9614, Ordering::Relaxed);
        assert_eq!(d.generation(), 0);
        clock.store(9615, Ordering::Relaxed);
        assert!(d.generation() > 0);
        assert_eq!(
            i16::from_le_bytes(d.s.regs[0x28..0x2a].try_into().unwrap()),
            16393
        );
        assert!((d.value(7).to_degrees() - 0.9975).abs() < 1e-5);
        d.write(0x12, 0x44);
        let before = d.value(4);
        d.set(4, 0.);
        clock.store(20000, Ordering::Relaxed);
        assert_eq!(d.value(4), before);
        d.read_done(0x28);
        d.read_done(0x29);
        clock.store(30000, Ordering::Relaxed);
        assert_eq!(d.value(4), 0.);
        d.write(0x12, 0);
        assert_eq!(d.next_address(0x20), 0x20);
        d.write(0x12, 4);
        assert_eq!(d.next_address(0x20), 0x21);
        d.write(0x10, 0);
        d.write(0x11, 0);
        let generation = d.generation();
        clock.store(90000, Ordering::Relaxed);
        assert_eq!(d.generation(), generation);
        d.write(0x12, 1);
        assert_eq!(d.registers()[0x12] & 1, 1);
        clock.store(90049, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.registers()[0x12] & 1, 1);
        clock.store(90050, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.registers()[0x12], 4);
        assert!(d.value(4).is_nan());
        assert_eq!(d.s.inputs[4], 0.);
    }
}
