use super::*;
const G: f64 = 9.80665;
const DEG: f64 = std::f64::consts::PI / 180.;
pub(super) struct Qmi8658 {
    s: SampleState,
    next: [Option<u64>; 2],
    boot: u64,
    stamp: u32,
}
impl Qmi8658 {
    pub fn new(clock: Arc<AtomicU64>, hz: u32) -> Self {
        let mut s = SampleState::new(clock, hz);
        s.inputs[0] = 25.;
        s.inputs[6] = G;
        let mut d = Self {
            s,
            next: [None; 2],
            boot: 0,
            stamp: 0,
        };
        d.reset();
        d
    }
    fn reset(&mut self) {
        self.s.time();
        self.s.regs = [0; 256];
        self.s.regs[0] = 5;
        self.s.regs[1] = 0x79;
        self.s.regs[2] = 0x20;
        self.s.readings.fill(f64::NAN);
        self.next = [None; 2];
        self.stamp = 0;
        self.boot = self.s.now + self.s.ticks(150_000);
    }
    fn period(&self, axis: usize) -> Option<u64> {
        let code = self.s.regs[3 + axis] & 15;
        let enabled = self.s.regs[8] & (1 << axis) != 0 && self.s.regs[2] & 1 == 0;
        if !enabled {
            return None;
        }
        let hz = if axis == 0 && self.s.regs[8] & 2 == 0 {
            match code {
                3..=8 => 8000. / (1u32 << code) as f64,
                12 => 128.,
                13 => 21.,
                14 => 11.,
                15 => 3.,
                _ => return None,
            }
        } else {
            if code > 8 {
                return None;
            }
            7520. / (1u32 << code) as f64
        };
        Some((self.s.hz as f64 / hz).round().max(1.) as u64)
    }
    fn schedule(&mut self) {
        for axis in 0..2 {
            self.next[axis] = self.period(axis).map(|p| {
                self.s.now.max(self.boot)
                    + self.s.ticks(if axis == 0 { 3000 } else { 60000 })
                    + 3 * p
            });
        }
    }
    fn encode(&mut self, field: usize, reg: usize, scale: f64) {
        let raw = (self.s.inputs[field] / scale)
            .round()
            .clamp(-32768., 32767.) as i16;
        self.s.regs[reg..reg + 2].copy_from_slice(&raw.to_le_bytes());
        self.s.readings[field] = raw as f64 * scale;
    }
}
impl RegisterSensor for Qmi8658 {
    fn sync(&mut self) {
        self.s.time();
        let mut samples = 0;
        for axis in 0..2 {
            let Some(at) = self.next[axis] else { continue };
            if self.s.now < at {
                continue;
            }
            let p = self.period(axis).unwrap();
            let n = 1 + (self.s.now - at) / p;
            let fs = (self.s.regs[3 + axis] >> 4) & 7;
            let scale = if axis == 0 {
                (2u32 << fs) as f64 * G / 32768.
            } else {
                (16u32 << fs) as f64 * DEG / 32768.
            };
            for i in 0..3 {
                self.encode(4 + axis * 3 + i, 0x35 + axis * 6 + i * 2, scale);
            }
            self.s.regs[0x2e] |= 1 << axis;
            self.next[axis] = Some(at + n * p);
            samples = samples.max(n);
        }
        if samples > 0 {
            self.encode(0, 0x33, 1. / 256.);
            self.stamp = self.stamp.wrapping_add(samples as u32) & 0xffffff;
            self.s.regs[0x30..0x33].copy_from_slice(&self.stamp.to_le_bytes()[..3]);
            self.s.publish(samples);
        }
    }
    fn registers(&self) -> [u8; 256] {
        let mut regs = self.s.regs;
        if regs[2] & 0x20 != 0 {
            for i in (0x33..=0x3f).step_by(2) {
                regs.swap(i, i + 1);
            }
        }
        regs
    }
    fn next_address(&self, reg: u8) -> u8 {
        if self.s.regs[2] & 0x40 != 0 {
            reg.wrapping_add(1)
        } else {
            reg
        }
    }
    fn address_ready(&self) -> bool {
        self.s.now >= self.boot
    }
    fn read_done(&mut self, reg: u8) {
        if reg == 0x3a {
            self.s.regs[0x2e] &= !1;
        }
        if reg == 0x40 {
            self.s.regs[0x2e] &= !2;
        }
    }
    fn write(&mut self, reg: u8, value: u16) -> bool {
        self.sync();
        let v = value as u8;
        match reg {
            0x60 if v == 0xb0 => {
                self.reset();
                return true;
            }
            0x60 => return true,
            2 if v & 0x1e == 0 => {}
            3 if v & 0xc0 == 0 && !matches!(v & 15, 9..=11) => {}
            4 if v & 0x80 == 0 && v & 15 <= 8 => {}
            6 => {}
            8 if v & !3 == 0 => {}
            0x14 if v == 0 => {}
            _ => return false,
        }
        let changed = self.s.regs[reg as usize] != v;
        self.s.regs[reg as usize] = v;
        if changed && matches!(reg, 2..=4 | 8) {
            self.schedule();
        }
        true
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        self.sync();
        let valid = match field {
            0 => (-40. ..=85.).contains(&value),
            4..=6 => (-16. * G..=16. * G).contains(&value),
            7..=9 => (-2048. * DEG..=2048. * DEG).contains(&value),
            _ => false,
        };
        if !valid || !value.is_finite() {
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
    fn qmi8658_scaling_timing_modes_status_signed_temperature_and_reset() {
        let c = Arc::new(AtomicU64::new(0));
        let mut d = Qmi8658::new(c.clone(), 1_000_000);
        assert!(!d.address_ready());
        c.store(150_000, Ordering::Relaxed);
        d.sync();
        assert!(d.address_ready());
        assert!(d.set(4, G));
        assert!(d.set(7, 64. * DEG));
        assert!(d.set(0, -10.5));
        assert!(!d.set(4, f64::NAN));
        assert!(!d.set(4, 157.));
        assert!(!d.set(7, 36.));
        assert!(!d.set(29, 1.));
        assert!(d.write(2, 0x40));
        assert!(d.write(3, 0x34));
        assert!(d.write(4, 0x74));
        assert!(d.write(8, 3));
        assert_eq!(d.period(0), Some(2128));
        assert_eq!(d.period(1), Some(2128));
        c.store(159_383, Ordering::Relaxed);
        assert_eq!(d.generation(), 0);
        c.store(159_384, Ordering::Relaxed);
        assert_eq!(d.generation(), 1);
        assert_eq!(d.s.regs[0x2e], 1);
        c.store(216_384, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.s.regs[0x2e], 3);
        assert_eq!(&d.s.regs[0x35..0x37], &2048i16.to_le_bytes());
        assert_eq!(&d.s.regs[0x3b..0x3d], &1024i16.to_le_bytes());
        assert_eq!(d.value(0), -10.5);
        assert_eq!(&d.s.regs[0x33..0x35], &(-2688i16).to_le_bytes());
        d.read_done(0x3a);
        assert_eq!(d.s.regs[0x2e], 2);
        d.read_done(0x40);
        assert_eq!(d.s.regs[0x2e], 0);
        assert!(d.write(0x60, 0xff));
        assert_eq!(d.s.regs[3], 0x34);
        assert!(d.write(8, 0));
        let n = d.generation();
        c.store(1_000_000, Ordering::Relaxed);
        assert_eq!(d.generation(), n);
        assert!(d.write(3, 0x08));
        assert!(d.write(8, 1));
        assert_eq!(d.period(0), Some(32000));
        assert!(d.set(4, 16. * G));
        c.store(1_200_000, Ordering::Relaxed);
        d.sync();
        assert_eq!(&d.s.regs[0x35..0x37], &32767i16.to_le_bytes());
        assert!(d.write(2, 0x60));
        assert_eq!(&d.registers()[0x35..0x37], &32767i16.to_be_bytes());
        assert_eq!(d.next_address(0x35), 0x36);
        assert!(d.write(2, 0x20));
        assert_eq!(d.next_address(0x35), 0x35);
        assert!(d.write(0x60, 0xb0));
        assert_eq!(d.s.regs[8], 0);
        assert_eq!(d.s.regs[3], 0);
        assert!(!d.address_ready());
        assert!(d.value(4).is_nan());
    }
    #[test]
    fn qmi8658_address_and_wire_snapshot() {
        let config = SensorConfig {
            id: 0,
            sda: 4,
            scl: 5,
            address: 0x6b,
            model: 46,
            shunt_milliohms: 0,
        };
        assert!(config.valid());
        assert!(SensorConfig {
            address: 0x6a,
            ..config
        }
        .valid());
        assert!(!SensorConfig {
            address: 0x69,
            ..config
        }
        .valid());
        let c = Arc::new(AtomicU64::new(200_000));
        let state = Arc::new(Mutex::new(Sensor::new(config, c.clone(), 1_000_000)));
        let mut b = SensorI2c::new(state.clone());
        assert!(!b.start(false));
        c.store(400_000, Ordering::Relaxed);
        assert!(b.start(false));
        assert!(b.write(0));
        assert!(b.start(true));
        assert_eq!(b.read(), 5);
        b.stop();
        for (reg, v) in [(2, 0x40), (3, 0x34), (4, 0x74), (8, 3)] {
            assert!(b.start(false));
            assert!(b.write(reg));
            assert!(b.write(v));
            b.stop();
        }
        assert!(state.lock().unwrap().set(4, G));
        c.store(600_000, Ordering::Relaxed);
        assert!(b.start(false));
        assert!(b.write(0x35));
        assert!(b.start(true));
        assert_eq!(b.read(), 0);
        assert!(state.lock().unwrap().set(4, -G));
        c.store(700_000, Ordering::Relaxed);
        assert_eq!(b.read(), 8);
        b.stop();
        assert!(b.start(false));
        assert!(b.write(0x35));
        assert!(b.start(true));
        assert_eq!([b.read(), b.read()], (-2048i16).to_le_bytes());
    }
}
