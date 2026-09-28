use super::*;

pub(super) struct Vl53l0x {
    s: SampleState,
    banks: [[u8; 256]; 8],
    page: usize,
    next: Option<u64>,
    boot: Option<u64>,
    strobe: Option<u64>,
    mode: u8,
}
impl Vl53l0x {
    pub fn new(clock: Arc<AtomicU64>, hz: u32) -> Self {
        let mut d = Self {
            s: SampleState::new(clock, hz),
            banks: [[0; 256]; 8],
            page: 0,
            next: None,
            boot: None,
            strobe: None,
            mode: 0,
        };
        d.s.inputs[58] = 1000.;
        d.reset();
        d
    }
    fn reset(&mut self) {
        self.banks = [[0; 256]; 8];
        self.page = 0;
        self.next = None;
        self.strobe = None;
        self.mode = 0;
        for (r, v) in [
            (1, 0xff),
            (0x50, 6),
            (0x52, 0x96),
            (0x70, 4),
            (0x71, 1),
            (0x72, 0xfe),
            (0x46, 0x25),
            (0x8a, 0x29),
            (0xbf, 1),
            (0xc2, 0x10),
        ] {
            self.banks[0][r] = v;
        }
        self.banks[1][0x91] = 0x3c;
        self.banks[1][0x84..0x86].copy_from_slice(&0x970au16.to_be_bytes());
        self.banks[0][0xf8..0xfa].copy_from_slice(&1u16.to_be_bytes());
        self.boot = Some(self.s.now + self.s.ticks(1200));
        self.s.generation = 0;
        self.s.readings = [f64::NAN; FIELD_COUNT];
    }
    fn word(&self, p: usize, r: usize) -> u16 {
        u16::from_be_bytes([self.banks[p][r], self.banks[p][r + 1]])
    }
    fn put(&mut self, p: usize, r: usize, v: u16) {
        self.banks[p][r..r + 2].copy_from_slice(&v.to_be_bytes());
    }
    fn timeout(v: u16) -> u64 {
        (u64::from(v & 255)
            .checked_shl(u32::from(v >> 8))
            .unwrap_or(u64::MAX))
        .saturating_add(1)
    }
    fn us(&self, clocks: u64, reg: usize) -> u64 {
        let ns = (2304 * 1655 * (u64::from(self.banks[0][reg]) + 1) * 2 + 500) / 1000;
        clocks.saturating_mul(ns).saturating_add(ns / 2) / 1000
    }
    fn budget(&self) -> u64 {
        let seq = self.banks[0][1];
        let pre = Self::timeout(self.word(0, 0x51));
        let msrc = self.us(u64::from(self.banks[0][0x46]) + 1, 0x50);
        let mut us = 1910 + 960;
        if seq & 0x10 != 0 {
            us += msrc + 590;
        }
        if seq & 8 != 0 {
            us += 2 * (msrc + 690);
        } else if seq & 4 != 0 {
            us += msrc + 660;
        }
        if seq & 0x40 != 0 {
            us += self.us(pre, 0x50) + 660;
        }
        if seq & 0x80 != 0 {
            us = us
                .saturating_add(self.us(
                    Self::timeout(self.word(0, 0x71)).saturating_sub(if seq & 0x40 != 0 {
                        pre
                    } else {
                        0
                    }),
                    0x70,
                ))
                .saturating_add(550);
        }
        // ponytail: malformed timeout exponents cap at 60 s; model longer exposures when required.
        us.min(60_000_000)
    }
    fn period(&self) -> u64 {
        let inter = u32::from_be_bytes(self.banks[0][4..8].try_into().unwrap()) as u64;
        self.s
            .ticks(self.budget().max(if self.mode == 4 {
                inter * 1000 / u64::from(self.word(0, 0xf8).max(1))
            } else {
                0
            }))
            .max(1)
    }
    fn capture(&mut self, n: u64) {
        let count: u32 = self.banks[0][0xb0..0xb6]
            .iter()
            .map(|v| v.count_ones())
            .sum();
        // Nominal 4 MCPS per reference SPAD; calibration really searches its enabled map.
        self.put(1, 0xb6, (count * 512).min(65535) as u16);
        if self.banks[0][1] & 0x80 != 0 {
            let offset = ((self.word(0, 0x28) << 4) as i16 >> 4) as f64 / 4.;
            let distance = (self.s.inputs[58] + offset).max(0.);

            let signal = (20.0 * (400.0 / self.s.inputs[58].max(30.)).powi(2)).min(100.);
            let limit = self.word(0, 0x44) as f64 / 128.;
            self.banks[0][0x14] = (if signal < limit { 4 } else { 11 }) << 3 | 1;
            self.put(0, 0x16, 5 * 256);
            self.put(0, 0x1a, (signal * 128.).round() as u16);
            self.put(0, 0x1c, 1);
            let scale = if self.banks[0][9] & 1 != 0 { 4. } else { 1. };
            self.put(0, 0x1e, (distance * scale).round().min(65535.) as u16);
            self.s.readings[58] = self.word(0, 0x1e) as f64 / scale;
            self.s.publish(n);
        } else {
            self.banks[0][0x14] = 1;
            if self.banks[0][1] & 1 != 0 {
                self.banks[0][0xcb] = 0x20;
            }
            if self.banks[0][1] & 2 != 0 {
                self.banks[0][0xee] = 4;
            }
        }
        let distance = self.word(0, 0x1e) / if self.banks[0][9] & 1 != 0 { 4 } else { 1 };
        let low = (self.word(0, 0xe) & 0xfff) * 2;
        let high = (self.word(0, 0xc) & 0xfff) * 2;
        let conf = self.banks[0][0xa] & 7;
        if match conf {
            1 => distance < low,
            2 => distance > high,
            3 => distance < low || distance > high,
            4 => true,
            _ => false,
        } {
            self.banks[0][0x13] = conf;
        }
    }
    fn nvm(&mut self) {
        let value: u32 = match self.banks[7][0x94] {
            0x02 => 0x01000000,
            0x7b => 0x27000001,
            0x7c => 1,
            0x6b => 5 << 8,
            0x24 => u32::MAX,
            0x25 => 0xff0f0000,
            0x73 => 0,
            0x74 => 0x17000000,
            0x75 => 0x19,
            0x76 => 0,
            _ => 0,
        };
        self.banks[7][0x90..0x94].copy_from_slice(&value.to_be_bytes());
        self.banks[7][0x83] = 1;
    }
}
impl RegisterSensor for Vl53l0x {
    fn address(&self, _configured: u8) -> u8 {
        self.banks[0][0x8a] & 127
    }
    fn sync(&mut self) {
        self.s.time();
        if self.boot.is_some_and(|at| self.s.now >= at) {
            self.boot = None;
            self.banks[0][0xc0] = 0xee;
        }
        if self.strobe.is_some_and(|at| self.s.now >= at) {
            self.strobe = None;
            self.nvm();
        }
        if let Some(at) = self.next {
            if self.s.now >= at {
                let period = self.period();
                let n = if self.mode == 2 || self.mode == 4 {
                    1 + (self.s.now - at) / period
                } else {
                    1
                };
                self.capture(n);
                self.next = if self.mode == 2 || self.mode == 4 {
                    Some(at + n * period)
                } else {
                    None
                };
            }
        }
    }
    fn registers(&self) -> [u8; 256] {
        let mut r = self.banks[self.page];
        r[255] = self.page as u8;
        r
    }
    fn write(&mut self, reg: u8, value: u16) -> bool {
        self.sync();
        let v = value as u8;
        let r = reg as usize;
        if r == 255 {
            self.page = (v & 7) as usize;
            return true;
        }
        if self.page == 0 {
            match r {
                0xbf if v & 1 == 0 => {
                    self.reset();
                    self.boot = None;
                    self.banks[0][0xbf] = 0;
                    return true;
                }
                0xbf => {
                    self.banks[0][r] = 1;
                    self.boot = Some(self.s.now + self.s.ticks(1200));
                    return true;
                }
                0 => {
                    self.mode = v & 6;
                    self.banks[0][0] = v & !1;
                    self.next = if v & 7 != 0 && self.boot.is_none() && self.banks[0][0xbf] != 0 {
                        Some(self.s.now + self.s.ticks(self.budget()))
                    } else {
                        None
                    };
                    return true;
                }
                0xb => {
                    if v & 1 != 0 {
                        self.banks[0][0x13] = 0;
                        self.banks[0][0x14] &= !1;
                    }
                    return true;
                }
                0x13..=0x1f | 0xc0..=0xc2 => return true,
                0x8a if !(8..=0x77).contains(&v) => return false,
                _ => {}
            }
        }
        self.banks[self.page][r] = v;
        if self.page == 7 && r == 0x83 && v == 0 {
            self.strobe = Some(self.s.now + self.s.ticks(100));
        }
        true
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        if field != 58 || !value.is_finite() || !(0.0..=2000.0).contains(&value) {
            return false;
        }
        self.sync();
        self.s.inputs[58] = value;
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
    fn vl53l0x_wire_snapshot_continuous_period_and_threshold_units() {
        let clock = Arc::new(AtomicU64::new(1200));
        let state = Arc::new(Mutex::new(Sensor::new(
            SensorConfig {
                id: 0,
                sda: 4,
                scl: 5,
                address: 0x29,
                model: 41,
                shunt_milliohms: 0,
            },
            clock.clone(),
            1_000_000,
        )));
        let mut wire = SensorI2c::new(state.clone());
        fn write(w: &mut SensorI2c, r: u8, bytes: &[u8]) {
            assert!(w.start(false));
            assert!(w.write(r));
            for b in bytes {
                assert!(w.write(*b));
            }
            w.stop();
        }
        fn select(w: &mut SensorI2c, r: u8) {
            assert!(w.start(false));
            assert!(w.write(r));
            assert!(w.start(true));
        }
        clock.store(2400, Ordering::Relaxed);
        write(&mut wire, 1, &[0xe8]);
        write(&mut wire, 0xa, &[2]);
        write(&mut wire, 0xc, &[0, 250]);
        write(&mut wire, 4, &[0, 0, 0, 200]);
        write(&mut wire, 0, &[4]);
        clock.store(102400, Ordering::Relaxed);
        select(&mut wire, 0x13);
        assert_eq!(wire.read(), 2);
        wire.stop();
        select(&mut wire, 0x1e);
        assert_eq!(wire.read(), 3);
        state.lock().unwrap().set(58, 1234.);
        clock.store(302400, Ordering::Relaxed);
        state.lock().unwrap().generation();
        assert_eq!(wire.read(), 232);
        wire.stop();
        select(&mut wire, 0x1e);
        assert_eq!([wire.read(), wire.read()], 1234u16.to_be_bytes());
        wire.stop();
        write(&mut wire, 0xb, &[1]);
        write(&mut wire, 0xc, &[3, 232]);
        let n = state.lock().unwrap().generation();
        clock.store(402399, Ordering::Relaxed);
        assert_eq!(state.lock().unwrap().generation(), n);
        clock.store(502400, Ordering::Relaxed);
        assert_eq!(state.lock().unwrap().generation(), n + 1);
        select(&mut wire, 0x13);
        assert_eq!(wire.read(), 0);
        wire.stop();
        write(&mut wire, 0, &[0]);
        clock.store(902400, Ordering::Relaxed);
        assert_eq!(state.lock().unwrap().generation(), n + 1);
    }
    #[test]
    fn vl53l0x_banks_nvm_calibration_timing_range_and_reset() {
        let clock = Arc::new(AtomicU64::new(0));
        let mut d = Vl53l0x::new(clock.clone(), 1_000_000);
        assert_eq!(d.registers()[0xc0], 0);
        clock.store(1200, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.registers()[0xc0], 0xee);
        d.write(255, 7);
        d.write(0x94, 0x24);
        d.write(0x83, 0);
        clock.store(1299, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.registers()[0x83], 0);
        clock.store(1300, Ordering::Relaxed);
        d.sync();
        assert_eq!(&d.registers()[0x90..0x94], &[255; 4]);
        d.write(255, 0);
        d.write(1, 1);
        d.write(0xb0, 7);
        d.write(0xa, 4);
        d.write(0, 0x41);
        let at = d.next.unwrap();
        clock.store(at - 1, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.registers()[0x13], 0);
        clock.store(at, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.word(1, 0xb6), 3 * 512);
        assert_eq!(d.generation(), 0);
        assert_eq!(d.registers()[0x13], 4);
        d.write(0xb, 1);
        assert_eq!(d.registers()[0x13], 0);
        d.write(1, 0xe8);
        let budget = d.budget();
        assert!(budget > 20000 && budget < 100000, "{budget}");
        d.set(58, 345.25);
        d.write(9, 1);
        d.write(0x28, 0);
        d.write(0x29, 40);
        d.write(0, 1);
        let at = d.next.unwrap();
        clock.store(at - 1, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.generation(), 0);
        clock.store(at, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.word(0, 0x1e), 1421);
        assert_eq!(d.registers()[0x14] >> 3, 11);
        assert_eq!(d.value(58), 355.25);
        d.write(0x28, 0x0f);
        d.write(0x29, 0xd8);
        d.write(9, 0);
        d.set(58, 0.);
        d.write(0, 2);
        let at = d.next.unwrap();
        clock.store(at, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.word(0, 0x1e), 0);
        assert_eq!(d.registers()[0x14] >> 3, 11);
        d.set(58, 777.);
        let at = d.next.unwrap();
        clock.store(at, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.word(0, 0x1e), 767);
        d.write(0, 0);
        let n = d.generation();
        clock.store(at + 1_000_000, Ordering::Relaxed);
        assert_eq!(d.generation(), n);
        d.write(0x8a, 0x30);
        assert_eq!(d.address(0x29), 0x30);
        d.write(0xbf, 0);
        assert_eq!(d.address(0x30), 0x29);
        assert_eq!(d.registers()[0xc0], 0);
        d.write(0, 1);
        assert!(d.next.is_none());
        d.write(0xbf, 1);
        clock.fetch_add(1199, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.registers()[0xc0], 0);
        clock.fetch_add(1, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.registers()[0xc0], 0xee);
        assert!(!d.set(58, f64::NAN));
        assert!(!d.set(58, 2001.));
        assert!(!d.set(0, 1.));
    }
}
