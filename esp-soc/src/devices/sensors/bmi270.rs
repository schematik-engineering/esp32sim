use super::*;
const CONFIG: &[u8; 8192] = include_bytes!("bmi270-config.bin");

pub(super) struct Bmi270 {
    s: SampleState,
    image: [u8; 8192],
    written: [bool; 8192],
    ptr: usize,
    loading: bool,
    upload_valid: bool,
    completed: bool,
    reset_at: Option<u64>,
    init_at: Option<u64>,
    access_at: u64,
    wrote: bool,
    next: [Option<u64>; 2],
    ready_at: [u64; 2],
}
impl Bmi270 {
    pub fn new(clock: Arc<AtomicU64>, hz: u32) -> Self {
        let mut d = Self {
            s: SampleState::new(clock, hz),
            image: [0; 8192],
            written: [false; 8192],
            ptr: 0,
            loading: false,
            upload_valid: true,
            completed: false,
            reset_at: None,
            init_at: None,
            access_at: 0,
            wrote: false,
            next: [None; 2],
            ready_at: [0; 2],
        };
        d.s.inputs[6] = 9.80665;
        d.reset();
        d
    }
    fn reset(&mut self) {
        self.s.regs = [0; 256];
        for (r, v) in [
            (0, 0x24),
            (3, 0x10),
            (0x1b, 1),
            (0x40, 0xa8),
            (0x41, 2),
            (0x42, 0xa9),
            (0x43, 0),
            (0x7c, 3),
        ] {
            self.s.regs[r] = v;
        }
        self.s.readings = [f64::NAN; FIELD_COUNT];
        self.written.fill(false);
        self.ptr = 0;
        self.loading = false;
        self.upload_valid = true;
        self.completed = false;
        self.init_at = None;
        self.next = [None; 2];
        self.ready_at = [0; 2];
        self.access_at = 0;
        self.wrote = false;
    }
    fn period(&self, g: usize) -> Option<u64> {
        if self.s.regs[0x21] & 15 != 1 || self.s.regs[0x7d] & [4, 2][g] == 0 {
            return None;
        }
        let odr = self.s.regs[0x40 + g * 2] & 15;
        let legal = if g == 0 {
            (1..=12).contains(&odr)
        } else {
            (6..=13).contains(&odr)
        };
        if !legal || (g == 1 && self.s.regs[0x43] & 7 > 4) {
            return None;
        }
        Some((self.s.hz * 256 / (100u64 << odr)).max(1))
    }
    fn schedule(&mut self, before: [Option<u64>; 2]) {
        for g in 0..2 {
            let after = self.period(g);
            if before[g] != after {
                if before[g].is_none() && after.is_some() {
                    self.ready_at[g] = self.s.now
                        + self.s.ticks(if g == 0 {
                            2000
                        } else if self.s.regs[0x7c] & 4 != 0 {
                            2000
                        } else {
                            45000
                        });
                }
                self.next[g] = after.map(|p| (self.s.now + p).max(self.ready_at[g]));
            }
        }
    }
    fn sample(&mut self, g: usize) {
        let scale = if g == 0 {
            9.80665 * f64::from(2u32 << (self.s.regs[0x41] & 3)) / 32768.
        } else {
            2000f64.to_radians() / f64::from(1u32 << (self.s.regs[0x43] & 7)) / 32768.
        };
        for a in 0..3 {
            let f = 4 + g * 3 + a;
            let raw = (self.s.inputs[f] / scale).round().clamp(-32768., 32767.) as i16;
            let r = 0x0c + g * 6 + a * 2;
            self.s.regs[r..r + 2].copy_from_slice(&raw.to_le_bytes());
            self.s.readings[f] = f64::from(raw) * scale;
        }
        self.s.regs[3] |= [0x80, 0x40][g];
        if self.s.regs[0x58] & 0x44 != 0 {
            self.s.regs[0x1d] |= [0x80, 0x40][g];
        }
    }
}
impl RegisterSensor for Bmi270 {
    fn sync(&mut self) {
        self.s.time();
        if let Some(at) = self.reset_at {
            if self.s.now < at {
                return;
            }
            self.reset_at = None;
            self.reset();
        }
        if self.init_at.is_some_and(|at| self.s.now >= at) {
            self.init_at = None;
            self.s.regs[0x21] =
                if self.upload_valid && self.written.iter().all(|v| *v) && self.image == *CONFIG {
                    1
                } else {
                    2
                };
            self.schedule([None; 2]);
        }
        let mut n = 0;
        for g in 0..2 {
            if let (Some(at), Some(p)) = (self.next[g], self.period(g)) {
                if self.s.now >= at {
                    let count = 1 + (self.s.now - at) / p;
                    self.sample(g);
                    self.next[g] = Some(at + count * p);
                    n = n.max(count);
                }
            }
        }
        if n > 0 {
            self.s.publish(n);
        }
        let ticks = (self.s.now / self.s.hz).wrapping_mul(25600)
            + (self.s.now % self.s.hz) * 25600 / self.s.hz;
        for a in 0..3 {
            self.s.regs[0x18 + a] = (ticks >> (a * 8)) as u8;
        }
    }
    fn registers(&self) -> [u8; 256] {
        self.s.regs
    }
    fn address_ready(&self) -> bool {
        self.reset_at.is_none()
    }
    fn next_address(&self, r: u8) -> u8 {
        if r == 0x5e {
            r
        } else {
            r.wrapping_add(1)
        }
    }
    fn write(&mut self, r: u8, v: u16) -> bool {
        self.sync();
        let v = v as u8;
        if self.reset_at.is_some() {
            return true;
        }
        if r == 0x7e && v == 0xb6 {
            self.reset();
            self.reset_at = Some(self.s.now + self.s.ticks(450));
            return true;
        }
        if self.s.now < self.access_at {
            return true;
        }
        self.wrote = true;
        let before = [self.period(0), self.period(1)];
        match r {
            0x7c => {
                if self.s.regs[0x7c] & 1 != 0 && v & 1 == 0 {
                    self.access_at = self.s.now + self.s.ticks(450);
                }
                self.s.regs[r as usize] = v;
            }
            0x59 => {
                if self.completed {
                    self.s.regs[0x21] = 2;
                } else if v & 1 == 0 && self.s.regs[0x7c] & 1 == 0 {
                    self.loading = true;
                    self.upload_valid = true;
                    self.written.fill(false);
                    self.s.regs[0x21] = 0;
                } else if v & 1 != 0 && self.loading {
                    self.loading = false;
                    self.completed = true;
                    self.init_at = Some(self.s.now + self.s.ticks(20000));
                }
                self.s.regs[r as usize] = v & 1;
            }
            0x5b | 0x5c => {
                self.s.regs[r as usize] = if r == 0x5b { v & 15 } else { v };
                self.ptr =
                    (usize::from(self.s.regs[0x5c]) * 16 + usize::from(self.s.regs[0x5b])) * 2;
            }
            0x5e if self.loading => {
                if self.ptr < 8192 {
                    self.image[self.ptr] = v;
                    self.written[self.ptr] = true;
                    self.ptr += 1;
                } else {
                    self.upload_valid = false;
                }
            }
            0x2f => self.s.regs[r as usize] = v & 7,
            0x40..=0x43 | 0x53..=0x58 | 0x7d => self.s.regs[r as usize] = v,
            _ => {}
        }
        self.schedule(before);
        true
    }
    fn stop(&mut self) {
        if self.wrote {
            self.s.time();
            if self.s.regs[0x7c] & 1 != 0 {
                self.access_at = self.s.now + self.s.ticks(450);
            }
            self.wrote = false;
        }
    }
    fn read_done(&mut self, r: u8) {
        match r {
            0x0c..=0x11 => self.s.regs[3] &= !0x80,
            0x12..=0x17 => self.s.regs[3] &= !0x40,
            0x1b => self.s.regs[0x1b] &= !1,
            0x1d => self.s.regs[0x1d] = 0,
            _ => {}
        }
    }
    fn set(&mut self, f: u32, v: f64) -> bool {
        let range = match f {
            4..=6 => -156.9064..=156.9064,
            7..=9 => -2000f64.to_radians()..=2000f64.to_radians(),
            _ => return false,
        };
        if !v.is_finite() || !range.contains(&v) {
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
    fn device() -> Bmi270 {
        Bmi270::new(Arc::new(AtomicU64::new(0)), 1_000_000)
    }
    fn time(d: &mut Bmi270, at: u64) {
        d.s.clock.store(at, Ordering::Relaxed);
        d.sync();
    }
    fn upload(d: &mut Bmi270, bytes: &[u8]) {
        d.write(0x7c, 0);
        time(d, d.s.now + 450);
        d.write(0x59, 0);
        for (i, chunk) in bytes.chunks(30).enumerate() {
            let p = i * 15;
            d.write(0x5b, (p & 15) as u16);
            d.write(0x5c, (p >> 4) as u16);
            for b in chunk {
                d.write(0x5e, u16::from(*b));
            }
        }
        d.write(0x59, 1);
    }
    #[test]
    fn bmi270_configuration_identity_coverage_timing_and_reset() {
        for variant in 0..4 {
            let mut d = device();
            assert_eq!(d.registers()[0], 0x24);
            assert!(d.value(4).is_nan());
            let mut bytes = CONFIG.to_vec();
            if variant == 1 {
                bytes[111] ^= 1;
            }
            if variant == 2 {
                bytes.truncate(8190);
            }
            if variant == 3 {
                bytes.extend_from_slice(&[1, 2]);
            }
            upload(&mut d, &bytes);
            time(&mut d, 20449);
            assert_eq!(d.s.regs[0x21], 0);
            time(&mut d, 20450);
            assert_eq!(d.s.regs[0x21], if variant == 0 { 1 } else { 2 });
            d.write(0x59, 1);
            assert_eq!(d.s.regs[0x21], 2);
            d.write(0x7e, 0xb6);
            assert!(!d.address_ready());
            time(&mut d, 20899);
            assert!(!d.address_ready());
            time(&mut d, 20900);
            assert!(d.address_ready());
            assert_eq!(d.s.regs[0x21], 0);
            assert!(!d.completed);
        }
    }
    #[test]
    fn bmi270_startup_scales_power_and_data_ready() {
        for arange in 0..4 {
            for grange in 0..5 {
                let mut d = device();
                upload(&mut d, CONFIG);
                time(&mut d, 20450);
                d.write(0x41, arange);
                d.write(0x43, grange);
                d.write(0x40, 0xa8);
                d.write(0x42, 0xa8);
                d.write(0x58, 4);
                for (f, v) in [
                    (4, 9.80665),
                    (5, -4.903325),
                    (6, 2.4516625),
                    (7, 0.1),
                    (8, -0.2),
                    (9, 0.3),
                ] {
                    assert!(d.set(f, v));
                }
                d.write(0x7d, 6);
                time(&mut d, 30449);
                assert!(d.value(4).is_nan());
                time(&mut d, 30450);
                assert_eq!(d.value(4), 9.80665);
                assert_eq!(d.s.regs[3] & 0xc0, 0x80);
                time(&mut d, 65449);
                assert!(d.value(7).is_nan());
                time(&mut d, 65450);
                assert!(d.value(7).is_finite());
                assert_eq!(d.s.regs[3] & 0xc0, 0xc0);
                assert_eq!(d.s.regs[0x1d] & 0xc0, 0xc0);
                for f in 4..10 {
                    let g = usize::from(f >= 7);
                    let r = 0x0c + (f - 4) * 2;
                    let raw = i16::from_le_bytes([d.s.regs[r], d.s.regs[r + 1]]);
                    let scale = if g == 0 {
                        9.80665 * (2u32 << arange) as f64 / 32768.
                    } else {
                        2000f64.to_radians() / (1u32 << grange) as f64 / 32768.
                    };
                    assert!((d.value(f as u32) - d.s.inputs[f]).abs() <= scale / 2. + 1e-9);
                    assert_eq!(d.value(f as u32), f64::from(raw) * scale);
                }
                d.read_done(0x0c);
                assert_eq!(d.s.regs[3] & 0x80, 0);
                d.read_done(0x1d);
                assert_eq!(d.s.regs[0x1d], 0);
                d.write(0x7d, 0);
                let generation = d.generation();
                d.set(4, 0.);
                time(&mut d, 100000);
                assert_eq!(d.generation(), generation);
                assert_eq!(d.value(4), 9.80665);
            }
        }
    }
    #[test]
    fn bmi270_input_boundaries_upload_guard_and_i2c_data_port() {
        let mut d = device();
        for (f, v) in [(0, 25.), (4, 157.), (7, 35.), (8, f64::NAN)] {
            assert!(!d.set(f, v));
        }
        d.write(0x7c, 0);
        time(&mut d, 449);
        d.write(0x59, 0);
        assert!(!d.loading);
        time(&mut d, 450);
        d.write(0x59, 0);
        assert!(d.loading);
        let mut first = device();
        let mut second = device();
        upload(&mut first, CONFIG);
        time(&mut first, 20450);
        time(&mut second, 20450);
        assert_eq!(first.s.regs[0x21], 1);
        assert_eq!(second.s.regs[0x21], 0);
        first.set(4, 9.80665);
        assert_eq!(second.s.inputs[4], 0.);
        let mut aps = device();
        aps.write(0x40, 0xa7);
        aps.stop();
        aps.write(0x40, 0xa6);
        assert_eq!(aps.s.regs[0x40], 0xa7);
        time(&mut aps, 449);
        aps.write(0x40, 0xa6);
        assert_eq!(aps.s.regs[0x40], 0xa7);
        time(&mut aps, 450);
        aps.write(0x40, 0xa6);
        assert_eq!(aps.s.regs[0x40], 0xa6);
        for address in [0x68, 0x69] {
            let c = SensorConfig {
                id: 0,
                sda: 4,
                scl: 5,
                address,
                model: 51,
                shunt_milliohms: 0,
            };
            assert!(c.valid());
            let clock = Arc::new(AtomicU64::new(0));
            let state = Arc::new(Mutex::new(Sensor::new(c, clock.clone(), 1_000_000)));
            let mut bus = SensorI2c::new(state);
            assert_eq!(bus.pins(), Some((4, 5)));
            assert!(!bus.matches_address(address, 0x67, false));
            bus.start(false);
            bus.write(0x7c);
            bus.write(0);
            bus.stop();
            clock.store(450, Ordering::Relaxed);
            bus.start(false);
            bus.write(0x59);
            bus.write(0);
            bus.stop();
            bus.start(false);
            bus.write(0x5e);
            for b in &CONFIG[..30] {
                assert!(bus.write(*b));
            }
            bus.stop();
        }
    }
}
