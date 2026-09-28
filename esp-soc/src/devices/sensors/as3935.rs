use super::*;

pub(super) struct As3935 {
    s: SampleState,
    ready: u64,
    pending: Option<u64>,
    calibration: Option<u64>,
    strikes: std::collections::VecDeque<u64>,
    clear_low: bool,
    estimate_valid: bool,
}
impl As3935 {
    pub fn new(clock: Arc<AtomicU64>, hz: u32) -> Self {
        let mut s = SampleState::new(clock, hz);
        s.inputs[92] = 20.;
        let mut device = Self {
            s,
            ready: 0,
            pending: None,
            calibration: None,
            strikes: Default::default(),
            clear_low: false,
            estimate_valid: false,
        };
        device.reset();
        device
    }
    fn reset(&mut self) {
        self.s.time();
        self.s.regs.fill(0);
        self.s.regs[0] = 0x24;
        self.s.regs[1] = 0x22;
        self.s.regs[2] = 0xc2;
        self.s.readings.fill(f64::NAN);
        self.s.readings[93] = 0.;
        self.s.readings[94] = 0.;
        self.s.readings[95] = 0.;
        self.s.inputs[94] = 0.;
        self.strikes.clear();
        self.clear_low = false;
        self.calibration = None;
        self.ready = self.s.now + self.s.ticks(4000);
        self.pending = None;
        self.estimate_valid = false;
        self.s.publish(1);
    }
    fn publish(&mut self) {
        let distance = self.s.inputs[92];
        let (code, status) = if !self.estimate_valid {
            (0, 0)
        } else if distance < 5. {
            (1, 2)
        } else if distance > 40. {
            (63, 3)
        } else {
            let bins = [5u8, 6, 8, 10, 12, 14, 17, 20, 24, 27, 31, 34, 37, 40];
            (
                *bins
                    .iter()
                    .min_by(|a, b| {
                        ((**a as f64) - distance)
                            .abs()
                            .total_cmp(&((**b as f64) - distance).abs())
                    })
                    .unwrap(),
                1,
            )
        };
        self.s.regs[7] = code;
        self.s.readings[92] = if status == 1 { code as f64 } else { f64::NAN };
        self.s.readings[95] = status as f64;
        self.s.readings[93] = (self.s.regs[4] as u32
            | (self.s.regs[5] as u32) << 8
            | (self.s.regs[6] as u32) << 16) as f64;
        self.s.readings[94] = (self.s.regs[3] & 15) as f64;
        self.s.publish(1);
    }
    fn event(&mut self, event: u8) {
        if self.s.regs[0] & 1 != 0 || self.s.now < self.ready {
            return;
        }
        let reason = match event {
            0 => 0,
            1 => 1,
            4 => {
                if self.s.regs[3] & 0x20 == 0 {
                    4
                } else {
                    0
                }
            }
            8 => {
                self.estimate_valid = true;
                let window = self.s.ticks(900_000_000);
                while self
                    .strikes
                    .front()
                    .is_some_and(|t| self.s.now.saturating_sub(*t) >= window)
                {
                    self.strikes.pop_front();
                }
                self.strikes.push_back(self.s.now);
                if self.strikes.len() > 16 {
                    self.strikes.pop_front();
                }
                let energy = self.s.inputs[93] as u32;
                self.s.regs[4] = energy as u8;
                self.s.regs[5] = (energy >> 8) as u8;
                self.s.regs[6] = (energy >> 16) as u8;
                if self.strikes.len() >= [1, 5, 9, 16][((self.s.regs[2] >> 4) & 3) as usize] {
                    8
                } else {
                    0
                }
            }
            _ => unreachable!(),
        };
        // The IRQ deassertion on a register read does not imply clearing INT bits.
        self.s.regs[3] = (self.s.regs[3] & 0xf0) | reason;
        self.pending = Some(self.s.now + self.s.ticks(2000));
    }
}
impl RegisterSensor for As3935 {
    fn sync(&mut self) {
        self.s.time();
        if self.calibration.is_some_and(|t| self.s.now >= t) {
            self.calibration = None;
            self.s.regs[0x3a] = 0x80;
            self.s.regs[0x3b] = 0x80;
        }
        if self.s.regs[0] & 1 == 0 && self.pending.is_some_and(|t| self.s.now >= t) {
            self.pending = None;
            self.publish();
        }
    }
    fn registers(&self) -> [u8; 256] {
        let mut r = self.s.regs;
        if self.pending.is_some() {
            r[3] &= 0xf0;
        }
        r
    }
    fn write(&mut self, reg: u8, value: u16) -> bool {
        self.sync();
        let v = value as u8;
        match reg {
            0 => {
                let was_down = self.s.regs[0] & 1 != 0;
                self.s.regs[0] = v & 0x3f;
                if was_down && v & 1 == 0 {
                    self.ready = self.s.now + self.s.ticks(4000);
                    self.pending = Some(self.ready);
                }
                if v & 1 != 0 {
                    self.s.readings[92] = f64::NAN;
                    self.s.readings[95] = 0.;
                    self.s.publish(1);
                }
            }
            1 => self.s.regs[1] = v & 0x7f,
            2 => {
                if self.s.regs[2] & 0x40 != 0 && v & 0x40 == 0 {
                    self.clear_low = true;
                }
                if self.clear_low && v & 0x40 != 0 {
                    self.strikes.clear();
                    self.clear_low = false;
                }
                self.s.regs[2] = v | 0x80;
            }
            3 => self.s.regs[3] = (v & 0xe0) | (self.s.regs[3] & 15),
            8 => self.s.regs[8] = v & 0xef,
            0x3c if v == 0x96 => self.reset(),
            0x3d if v == 0x96 => {
                self.s.regs[0x3a] = 0;
                self.s.regs[0x3b] = 0;
                self.calibration = Some(self.ready.max(self.s.now + self.s.ticks(2000)));
            }
            _ => return false,
        }
        true
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        if !value.is_finite() {
            return false;
        }
        let valid = match field {
            92 => (0. ..=100.).contains(&value),
            93 => value.fract() == 0. && (0. ..=2097151.).contains(&value),
            94 => [0., 1., 4., 8.].contains(&value),
            _ => false,
        };
        if !valid {
            return false;
        }
        self.sync();
        self.s.inputs[field as usize] = value;
        if field == 94 {
            self.event(value as u8);
        } else if field == 92 {
            self.estimate_valid = true;
            self.pending = Some(self.ready.max(self.s.now + self.s.ticks(2000)));
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
mod as3935_tests {
    use super::*;
    fn device() -> (Arc<AtomicU64>, As3935) {
        let clock = Arc::new(AtomicU64::new(0));
        (clock.clone(), As3935::new(clock, 1_000_000))
    }
    fn time(c: &Arc<AtomicU64>, d: &mut As3935, t: u64) {
        c.store(t, Ordering::Relaxed);
        d.sync();
    }
    #[test]
    fn as3935_startup_defaults_and_address() {
        let (c, mut d) = device();
        assert!(d.address_ready());
        assert!(d.value(92).is_nan());
        assert_eq!(d.value(95), 0.);
        time(&c, &mut d, 3999);
        assert!(d.address_ready());
        time(&c, &mut d, 4000);
        assert!(d.address_ready());
        assert_eq!(&d.registers()[..3], &[0x24, 0x22, 0xc2]);
        assert!(d.value(92).is_nan());
        assert_eq!(d.registers()[7], 0);
        for address in 0..5 {
            assert_eq!(
                SensorConfig {
                    id: 0,
                    sda: 1,
                    scl: 2,
                    address,
                    model: 55,
                    shunt_milliohms: 0
                }
                .valid(),
                (1..=3).contains(&address)
            );
        }
    }
    #[test]
    fn as3935_distance_bins_and_sentinels() {
        let (c, mut d) = device();
        time(&c, &mut d, 4000);
        for (i, v, code, status) in [
            (1, 13., 12, 1),
            (2, 0., 1, 2),
            (3, 41., 63, 3),
            (4, 40., 40, 1),
        ] {
            assert!(d.set(92, v));
            time(&c, &mut d, 4000 + i * 2000);
            assert_eq!(d.registers()[7], code);
            assert_eq!(d.value(95), status as f64);
            assert_eq!(d.value(92).is_finite(), status == 1);
        }
    }
    #[test]
    fn as3935_energy_reason_latency_and_read_semantics() {
        let (c, mut d) = device();
        time(&c, &mut d, 4000);
        d.set(93, 0x1abcde as f64);
        d.set(94, 8.);
        assert_eq!(d.registers()[3] & 15, 0);
        time(&c, &mut d, 5999);
        assert_eq!(d.value(94), 0.);
        time(&c, &mut d, 6000);
        assert_eq!(&d.registers()[4..7], &[0xde, 0xbc, 0x1a]);
        assert_eq!(d.value(94), 8.);
        d.read_done(3);
        assert_eq!(d.registers()[3] & 15, 8);
        assert_eq!(d.value(93), 0x1abcde as f64);
    }
    #[test]
    fn as3935_mask_noise_and_threshold() {
        let (c, mut d) = device();
        time(&c, &mut d, 4000);
        d.write(3, 0x20);
        d.set(94, 4.);
        time(&c, &mut d, 6000);
        assert_eq!(d.value(94), 0.);
        d.set(94, 1.);
        time(&c, &mut d, 8000);
        assert_eq!(d.value(94), 1.);
        d.write(2, 0xd2);
        for i in 1..=5 {
            d.set(94, 8.);
            time(&c, &mut d, 8000 + i * 2000);
            assert_eq!(d.value(94), if i == 5 { 8. } else { 0. });
        }
        d.write(2, 0x92);
        d.write(2, 0xd2);
        d.set(94, 8.);
        time(&c, &mut d, 20000);
        assert_eq!(d.value(94), 0.);
    }
    #[test]
    fn as3935_threshold_window_expires() {
        let (c, mut d) = device();
        time(&c, &mut d, 4000);
        d.write(2, 0xd2);
        for i in 1..5 {
            d.set(94, 8.);
            time(&c, &mut d, 4000 + i * 2000);
        }
        time(&c, &mut d, 900_020_000);
        d.set(94, 8.);
        time(&c, &mut d, 900_022_000);
        assert_eq!(d.value(94), 0.);
    }
    #[test]
    fn as3935_power_and_calibration_reset() {
        let (c, mut d) = device();
        time(&c, &mut d, 4000);
        d.write(0, 0x25);
        d.set(94, 8.);
        assert!(d.value(92).is_nan());
        assert!(d.address_ready());
        d.write(0, 0x24);
        time(&c, &mut d, 8000);
        d.write(0x3d, 0x96);
        time(&c, &mut d, 9999);
        assert_eq!(d.registers()[0x3a], 0);
        time(&c, &mut d, 10000);
        assert_eq!(&d.registers()[0x3a..0x3c], &[0x80, 0x80]);
        assert!(!d.write(0x3c, 0x95));
        d.write(0x3c, 0x96);
        assert_eq!(d.value(95), 0.);
        assert_eq!(d.registers()[0x3a], 0);
    }
    #[test]
    fn as3935_readonly_reserved_and_input_validation() {
        let (_, mut d) = device();
        for r in [4, 5, 6, 7, 9, 0x3a, 0x3b, 0x3e] {
            assert!(!d.write(r, 1));
        }
        for (f, v) in [
            (92, -1.),
            (92, 101.),
            (93, 2097152.),
            (93, 0.5),
            (94, 2.),
            (95, 1.),
            (92, f64::NAN),
            (93, f64::INFINITY),
        ] {
            assert!(!d.set(f, v));
        }
        d.write(3, 0xff);
        assert_eq!(d.registers()[3], 0xe0);
        d.write(8, 0xff);
        assert_eq!(d.registers()[8], 0xef);
    }
    #[test]
    fn as3935_instances_independent() {
        let (c, mut a) = device();
        let (_, mut b) = device();
        time(&c, &mut a, 4000);
        a.set(92, 37.);
        a.set(93, 12345.);
        a.set(94, 8.);
        time(&c, &mut a, 6000);
        assert_eq!(a.value(92), 37.);
        assert_eq!(a.value(93), 12345.);
        assert!(b.value(92).is_nan());
        assert_eq!(b.value(93), 0.);
    }
    #[test]
    fn as3935_i2c_pointer_readonly_and_wrong_address() {
        let clock = Arc::new(AtomicU64::new(0));
        let state = Arc::new(Mutex::new(Sensor::new(
            SensorConfig {
                id: 0,
                sda: 4,
                scl: 5,
                address: 3,
                model: 55,
                shunt_milliohms: 0,
            },
            clock.clone(),
            1_000_000,
        )));
        let mut bus = SensorI2c::new(state.clone());
        clock.store(4000, Ordering::Relaxed);
        assert_eq!(bus.pins(), Some((4, 5)));
        assert!(bus.matches_address(3, 3, true));
        assert!(!bus.matches_address(3, 2, true));
        assert!(bus.start(false));
        assert!(bus.write(0));
        bus.stop();
        assert!(bus.start(true));
        assert_eq!(bus.read(), 0x24);
        assert_eq!(bus.read(), 0x22);
        bus.stop();
        assert!(bus.start(false));
        assert!(bus.write(4));
        assert!(!bus.write(255));
        bus.stop();
        assert!(bus.start(false));
        assert!(bus.write(4));
        bus.stop();
        assert!(bus.start(true));
        assert_eq!(bus.read(), 0);
        bus.stop();
    }
}
