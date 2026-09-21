use super::*;

const SCALE: [f64; 8] = [
    524288., 1572864., 3670016., 7864320., 253952., 516096., 1040384., 2088960.,
];
const CONVERSION_US: [u64; 8] = [3600, 5200, 8400, 14800, 27600, 53200, 104400, 206800];

pub(super) struct Dps310 {
    s: SampleState,
    reset_at: u64,
    next: [Option<u64>; 2],
}
impl Dps310 {
    pub fn new(clock: Arc<AtomicU64>, hz: u32) -> Self {
        let mut d = Self {
            s: SampleState::new(clock, hz),
            reset_at: 0,
            next: [None; 2],
        };
        d.s.inputs[0] = 23.;
        d.s.inputs[2] = 101325.;
        d.reset();
        d
    }
    fn reset(&mut self) {
        self.s.time();
        self.reset_at = self.s.now;
        self.s.regs = [0; 256];
        self.s.regs[0x0d] = 0x10;
        self.s.regs[0x28] = 0x80;
        self.s.regs[0x0b] = 1;
        self.s.readings = [f64::NAN; FIELD_COUNT];
        self.next = [None; 2];
        // Fixed factory trim. The guest performs the documented polynomial compensation.
        let (c0, c1, c00, c10) = (40u32, 100u32, 75000u32, 75000u32);
        self.s.regs[0x10..0x18].copy_from_slice(&[
            (c0 >> 4) as u8,
            ((c0 << 4) | (c1 >> 8)) as u8,
            c1 as u8,
            (c00 >> 12) as u8,
            (c00 >> 4) as u8,
            ((c00 << 4) | (c10 >> 16)) as u8,
            (c10 >> 8) as u8,
            c10 as u8,
        ]);
        for (i, v) in [50, 100, 50, 10, 5].into_iter().enumerate() {
            self.s.put16(0x18 + i * 2, v);
        }
    }
    fn duration(&self, channel: usize) -> u64 {
        self.s
            .ticks(CONVERSION_US[(self.s.regs[6 + channel] & 7) as usize])
    }
    fn period(&self, channel: usize) -> u64 {
        let mode = self.s.regs[8] & 7;
        let work = if mode == 7 {
            self.duration(0) + self.duration(1)
        } else {
            self.duration(channel)
        };
        self.s
            .ticks(1_000_000 >> ((self.s.regs[6 + channel] >> 4) & 7))
            .max(work)
    }
    fn configure(&mut self) {
        let mode = self.s.regs[8] & 7;
        self.next = [None; 2];
        if self.s.regs[8] & 0x40 == 0 {
            return;
        }
        if matches!(mode, 1 | 5 | 7) {
            self.next[0] = Some(self.s.now + self.duration(0));
        }
        if matches!(mode, 2 | 6 | 7) {
            self.next[1] =
                Some(self.s.now + self.duration(1) + if mode == 7 { self.duration(0) } else { 0 });
        }
    }
    fn raw_scale(&self, channel: usize) -> f64 {
        let os = (self.s.regs[6 + channel] & 7) as usize;
        SCALE[os]
            * if os >= 4 && self.s.regs[9] & (4 << channel) == 0 {
                16.
            } else {
                1.
            }
    }
    fn compensated(p: f64, t: f64) -> f64 {
        75000. + p * (75000. + p * (50. + p * 5.)) + t * (50. + p * (100. + p * 10.))
    }
    fn capture(&mut self, channel: usize, count: u64) {
        let t = (self.s.inputs[0] - 20.) / 100.;
        let raw = if channel == 1 {
            (t * self.raw_scale(1)).round().clamp(-8388608., 8388607.) as i32
        } else {
            let scale = self.raw_scale(0);
            let n = inverse(self.s.inputs[2], 0xffffff, false, |v| {
                Self::compensated((v - 8388608.) / scale, t)
            });
            n as i32 - 8388608
        };
        let reg = channel * 3;
        self.s.regs[reg..reg + 3].copy_from_slice(&raw.to_be_bytes()[1..]);
        self.s.regs[8] |= 0x10 << channel;
        self.s.readings[if channel == 0 { 2 } else { 0 }] = if channel == 0 {
            Self::compensated(raw as f64 / self.raw_scale(0), t)
        } else {
            20. + raw as f64 / self.raw_scale(1) * 100.
        };
        self.s.publish(count);
    }
}
impl RegisterSensor for Dps310 {
    fn sync(&mut self) {
        self.s.time();
        if self.s.now >= self.reset_at + self.s.ticks(12000) {
            self.s.regs[8] |= 0x40;
        }
        if self.s.now >= self.reset_at + self.s.ticks(40000) {
            self.s.regs[8] |= 0x80;
        }
        for channel in 0..2 {
            if let Some(next) = self.next[channel] {
                if next <= self.s.now {
                    let mode = self.s.regs[8] & 7;
                    let period = self.period(channel);
                    let count = if mode >= 5 {
                        1 + (self.s.now - next) / period
                    } else {
                        1
                    };
                    self.capture(channel, count);
                    self.next[channel] = if mode >= 5 {
                        Some(next + period * count)
                    } else {
                        self.s.regs[8] &= !7;
                        None
                    };
                }
            }
        }
    }
    fn registers(&self) -> [u8; 256] {
        let mut r = self.s.regs;
        if r[8] & 0x80 == 0 {
            r[0x10..0x22].fill(0);
        }
        r
    }
    fn write(&mut self, reg: u8, value: u16) -> bool {
        self.sync();
        let value = value as u8;
        match reg {
            6 | 7 if value & 0x08 == 0 => {
                self.s.regs[reg as usize] = value;
                self.configure();
            }
            8 if matches!(value & 7, 0 | 1 | 2 | 5 | 6 | 7) => {
                self.s.regs[8] = (self.s.regs[8] & 0xf0) | (value & 7);
                self.configure();
            }
            9 if value & 0xf3 == 0 => self.s.regs[9] = value,
            0x0c if value & 15 == 9 => self.reset(),
            _ => return false,
        }
        true
    }
    fn read_done(&mut self, reg: u8) {
        if reg == 2 {
            self.s.regs[8] &= !0x10;
        }
        if reg == 5 {
            self.s.regs[8] &= !0x20;
        }
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        self.sync();
        set_environment(&mut self.s, field, value, 30000., 120000.)
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
fn set_environment(s: &mut SampleState, field: u32, value: f64, min: f64, max: f64) -> bool {
    let valid = value.is_finite()
        && match field {
            0 => (-40. ..=85.).contains(&value),
            2 => (min..=max).contains(&value),
            _ => false,
        };
    if valid {
        s.inputs[field as usize] = value;
    }
    valid
}

pub(super) struct Lps22df {
    s: SampleState,
    next: Option<u64>,
    boot: Option<u64>,
    read_mask: [u8; 2],
}
impl Lps22df {
    pub fn new(clock: Arc<AtomicU64>, hz: u32) -> Self {
        let mut d = Self {
            s: SampleState::new(clock, hz),
            next: None,
            boot: None,
            read_mask: [0; 2],
        };
        d.s.inputs[0] = 23.;
        d.s.inputs[2] = 101325.;
        d.reset();
        d
    }
    fn reset(&mut self) {
        self.s.regs = [0; 256];
        self.read_mask = [0; 2];
        self.s.regs[15] = 0xb4;
        self.s.regs[18] = 1;
        self.s.regs[25] = 0x80;
        self.s.time();
        self.s.regs[0x24] = 0x80;
        self.boot = Some(self.s.now + self.s.ticks(10000));
        self.s.readings = [f64::NAN; FIELD_COUNT];
        self.next = None;
    }
    fn odr(&self) -> u64 {
        [0, 1, 4, 10, 25, 50, 75, 100, 200][((self.s.regs[16] >> 3) & 15).min(8) as usize]
    }
    fn conversion(&self) -> u64 {
        self.s.ticks(
            [1200, 1500, 2400, 3400, 5400, 9400, 33400, 33400][(self.s.regs[16] & 7) as usize],
        )
    }
    fn period(&self) -> u64 {
        self.s
            .ticks(1_000_000 / self.odr().max(1))
            .max(self.conversion())
    }
    fn capture(&mut self, count: u64) {
        let pressure = self.read_mask[0] == 0;
        let temperature = self.read_mask[1] == 0;
        if pressure {
            let offset =
                i16::from_le_bytes([self.s.regs[0x1a], self.s.regs[0x1b]]) as f64 * 100. / 16.;
            let raw = ((self.s.inputs[2] - offset) * 4096. / 100.).round() as i32;
            self.s.regs[0x28..0x2b].copy_from_slice(&raw.to_le_bytes()[..3]);
            self.s.readings[2] = raw as f64 * 100. / 4096.;
        }
        if temperature {
            let t = (self.s.inputs[0] * 100.).round() as i16;
            self.s.regs[0x2b..0x2d].copy_from_slice(&t.to_le_bytes());
            self.s.readings[0] = t as f64 / 100.;
        }
        let updated = pressure as u8 | ((temperature as u8) << 1);
        self.s.regs[0x27] |= (self.s.regs[0x27] & updated) << 4;
        self.s.regs[0x27] |= updated;
        if updated != 0 {
            self.s.publish(count);
        }
    }
}
impl RegisterSensor for Lps22df {
    fn sync(&mut self) {
        self.s.time();
        if let Some(at) = self.boot {
            if self.s.now >= at {
                self.boot = None;
                self.s.regs[17] &= !0x84;
                self.s.regs[0x24] &= !0x80;
            }
        }
        if let Some(at) = self.next {
            if self.s.now >= at {
                let continuous = self.odr() > 0;
                let period = self.period();
                let count = if continuous {
                    1 + (self.s.now - at) / period
                } else {
                    1
                };
                self.capture(count);
                self.s.regs[17] &= !1;
                self.next = if continuous {
                    Some(at + period * count)
                } else {
                    None
                };
            }
        }
    }
    fn registers(&self) -> [u8; 256] {
        if self.s.regs[0x24] & 0x80 != 0 {
            let mut registers = [0; 256];
            registers[0x24] = 0x80;
            return registers;
        }
        self.s.regs
    }
    fn next_address(&self, reg: u8) -> u8 {
        if self.s.regs[18] & 1 != 0 {
            reg.wrapping_add(1)
        } else {
            reg
        }
    }
    fn write(&mut self, reg: u8, value: u16) -> bool {
        self.sync();
        let value = value as u8;
        if self.boot.is_some() {
            return false;
        }
        match reg {
            16 if value & 7 != 6 => {
                self.s.regs[16] = value & 0x7f;
                self.next = if self.odr() > 0 {
                    Some(self.s.now + self.period())
                } else {
                    None
                };
            }
            17 if value & 0x72 == 0 && value & 0x84 != 0x84 => {
                if value & 4 != 0 {
                    for reg in [
                        0x0b, 0x0c, 0x0d, 0x0e, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x24, 0x25,
                        0x26, 0x27,
                    ] {
                        self.s.regs[reg] = 0;
                    }
                    self.s.regs[18] = 1;
                    self.read_mask = [0; 2];
                    self.next = None;
                    self.s.regs[17] = 4;
                    self.boot = Some(self.s.now + self.s.ticks(50));
                } else {
                    if (self.s.regs[17] ^ value) & 8 != 0 {
                        self.read_mask = [0; 2];
                    }
                    self.s.regs[17] = value;
                    if value & 0x80 != 0 {
                        self.read_mask = [0; 2];
                        self.s.regs[0x1a..0x1c].fill(0);
                        self.s.regs[16] &= 7;
                        self.next = None;
                        self.s.regs[0x24] = 0x80;
                        self.boot = Some(self.s.now + self.s.ticks(10000));
                    }
                    if value & 1 != 0 && self.odr() == 0 {
                        self.next = Some(self.s.now + self.conversion());
                    }
                }
            }
            18 => self.s.regs[18] = value & 0xb,
            0x1a | 0x1b => self.s.regs[reg as usize] = value,
            _ => return false,
        }
        true
    }
    fn read_done(&mut self, reg: u8) {
        let (channel, first, last) = match reg {
            0x28..=0x2a => (0, 0x28, 0x2a),
            0x2b..=0x2c => (1, 0x2b, 0x2c),
            _ => return,
        };
        if self.s.regs[17] & 8 != 0 {
            self.read_mask[channel] |= 1 << (reg - first);
            if reg == last && self.read_mask[channel] == (1 << (last - first + 1)) - 1 {
                self.read_mask[channel] = 0;
            }
        }
        if reg == last {
            self.s.regs[0x27] &= !(0x11 << channel);
        }
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        self.sync();
        set_environment(&mut self.s, field, value, 26000., 126000.)
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
    fn dps_trim_conversion_modes_and_status() {
        let clock = Arc::new(AtomicU64::new(0));
        let mut d = Dps310::new(clock.clone(), 1_000_000);
        assert_eq!(d.registers()[8], 0);
        clock.store(12000, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.registers()[8], 0x40);
        clock.store(40000, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.registers()[8], 0xc0);
        assert!(d.set(0, -10.25));
        assert!(d.set(2, 99555.));
        assert!(!d.set(2, 120001.));
        d.write(7, 0x80);
        d.write(8, 2);
        clock.store(43599, Ordering::Relaxed);
        d.sync();
        assert!(d.value(0).is_nan());
        clock.store(43600, Ordering::Relaxed);
        d.sync();
        assert!((d.value(0) + 10.25).abs() < 0.001);
        assert_eq!(d.registers()[8] & 7, 0);
        assert_ne!(d.registers()[8] & 32, 0);
        d.read_done(5);
        assert_eq!(d.registers()[8] & 32, 0);
        d.write(8, 1);
        clock.store(47200, Ordering::Relaxed);
        d.sync();
        assert!((d.value(2) - 99555.).abs() < 0.1);
        d.write(0x0c, 0x89);
        assert!(d.value(2).is_nan());
        assert_eq!(d.registers()[8], 0);
    }
    #[test]
    fn lps_identity_one_shot_shutdown_bdu_and_reset() {
        let clock = Arc::new(AtomicU64::new(0));
        let mut d = Lps22df::new(clock.clone(), 1_000_000);
        assert_eq!(d.registers()[0x24], 0x80);
        assert_eq!(d.registers()[15], 0);
        clock.store(10000, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.registers()[15], 0xb4);
        d.set(0, -17.25);
        d.set(2, 103456.);
        d.write(17, 1);
        clock.store(11199, Ordering::Relaxed);
        assert!(d.value(0).is_nan());
        clock.store(11200, Ordering::Relaxed);
        assert_eq!(d.value(0), -17.25);
        assert!((d.value(2) - 103456.).abs() < 0.02);
        assert_eq!(d.registers()[17] & 1, 0);
        d.set(0, 30.);
        clock.store(1_000_000, Ordering::Relaxed);
        assert_eq!(d.value(0), -17.25);
        d.write(16, 0x18);
        d.write(17, 8);
        d.read_done(0x2b);
        clock.store(1_100_000, Ordering::Relaxed);
        assert_eq!(d.value(0), -17.25);
        d.read_done(0x2c);
        clock.store(1_200_000, Ordering::Relaxed);
        assert_eq!(d.value(0), 30.);
        d.write(18, 0);
        assert_eq!(d.next_address(0x28), 0x28);
        d.write(17, 4);
        assert_eq!(d.registers()[16], 0);
        assert_eq!(d.next_address(0x28), 0x29);
        clock.store(1_201_000, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.registers()[17], 0);
        assert!(!d.set(0, 86.));
        assert!(!d.write(17, 0x10));
    }
    #[test]
    fn pressure_ranges_oversampling_and_offset() {
        let clock = Arc::new(AtomicU64::new(0));
        let mut d = Dps310::new(clock.clone(), 1_000_000);
        clock.store(40000, Ordering::Relaxed);
        d.sync();
        for os in 0..8 {
            d.write(6, os);
            d.write(7, 0x80 | os);
            d.write(9, if os >= 4 { 12 } else { 0 });
            for (t, p) in [(-40., 30000.), (85., 120000.)] {
                d.set(0, t);
                d.set(2, p);
                d.capture(0, 1);
                d.capture(1, 1);
                assert!((d.value(0) - t).abs() < 0.001);
                assert!((d.value(2) - p).abs() < 0.2);
            }
        }
        let mut l = Lps22df::new(clock.clone(), 1_000_000);
        clock.store(50000, Ordering::Relaxed);
        l.sync();
        l.set(2, 100000.);
        l.write(0x1a, 16);
        l.write(0x1b, 0);
        l.write(17, 1);
        clock.store(51200, Ordering::Relaxed);
        assert!((l.value(2) - 99900.).abs() < 0.03);
        l.capture(1);
        assert_eq!(l.registers()[0x27], 0x33);
        l.read_done(0x2a);
        assert_eq!(l.registers()[0x27], 0x22);
        l.read_done(0x2c);
        assert_eq!(l.registers()[0x27], 0);
        l.write(17, 4);
        clock.store(51250, Ordering::Relaxed);
        l.sync();
        assert_eq!(l.registers()[0x1a], 16);
        l.write(17, 0x80);
        assert_eq!(l.registers()[0x24], 128);
        clock.store(61250, Ordering::Relaxed);
        l.sync();
        assert_eq!(l.registers()[0x1a], 0);
        assert_eq!(l.registers()[16] & 0x78, 0);
    }
    #[test]
    fn lps_bdu_locks_only_partial_reads_and_each_channel_separately() {
        let clock = Arc::new(AtomicU64::new(0));
        let mut d = Lps22df::new(clock.clone(), 1_000_000);
        clock.store(10000, Ordering::Relaxed);
        d.sync();
        d.write(17, 8);
        d.capture(1);
        // Data-ready alone never freezes unread output.
        d.set(0, 30.);
        d.set(2, 100000.);
        d.capture(1);
        assert_eq!(d.value(0), 30.);
        assert!((d.value(2) - 100000.).abs() < 0.03);
        d.read_done(0x28);
        d.read_done(0x2b);
        d.set(0, 40.);
        d.set(2, 110000.);
        d.capture(1);
        assert_eq!(d.value(0), 30.);
        assert!((d.value(2) - 100000.).abs() < 0.03);
        // Reading only pressure's high byte cannot release its missing middle byte.
        d.read_done(0x2a);
        d.capture(1);
        assert!((d.value(2) - 100000.).abs() < 0.03);
        d.read_done(0x29);
        d.capture(1);
        assert!((d.value(2) - 100000.).abs() < 0.03);
        d.read_done(0x2a);
        d.capture(1);
        assert!((d.value(2) - 110000.).abs() < 0.03);
        assert_eq!(d.value(0), 30.);
        d.read_done(0x2c);
        d.capture(1);
        assert_eq!(d.value(0), 40.);
        // A lone high-byte read is also partial, not an unconditional unlock.
        d.read_done(0x2c);
        d.set(0, 50.);
        d.capture(1);
        assert_eq!(d.value(0), 40.);
        d.read_done(0x2b);
        d.read_done(0x2c);
        d.capture(1);
        assert_eq!(d.value(0), 50.);
        d.read_done(0x28);
        d.read_done(0x2b);
        d.write(17, 0);
        d.set(0, 60.);
        d.set(2, 90000.);
        d.capture(1);
        assert_eq!(d.value(0), 60.);
        assert!((d.value(2) - 90000.).abs() < 0.03);
    }
}
