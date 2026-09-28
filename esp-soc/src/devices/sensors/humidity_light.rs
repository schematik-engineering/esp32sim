use super::*;

pub(super) struct Humidity {
    s: SampleState,
    model: u8,
    first: Option<u8>,
    next: Option<u64>,
    busy: u64,
    sleep: bool,
    status: u16,
    length: u8,
    humidity_first: bool,
    sample: [f64; 2],
}
impl Humidity {
    pub fn new(clock: Arc<AtomicU64>, hz: u32, model: u8) -> Self {
        let mut s = SampleState::new(clock, hz);
        s.inputs[0] = 25.;
        s.inputs[1] = 50.;
        Self {
            s,
            model,
            first: None,
            next: None,
            busy: 0,
            sleep: false,
            status: 0x8010,
            length: 0,
            humidity_first: false,
            sample: [25., 50.],
        }
    }
    fn word(&mut self, offset: usize, value: u16) {
        let bytes = value.to_be_bytes();
        self.s.regs[offset..offset + 2].copy_from_slice(&bytes);
        self.s.regs[offset + 2] = temperature::crc8(&bytes, 0xff, 0x31);
    }
    fn command(&mut self, cmd: u16) -> bool {
        self.sync();
        if self.s.now < self.busy || self.next.is_some() {
            return false;
        }
        if self.model == 27 && cmd == 0x3517 {
            self.sleep = false;
            self.busy = self.s.now + self.s.ticks(240);
            return true;
        }
        if (self.model == 26 && cmd == 0x30a2) || (self.model == 27 && cmd == 0x805d && !self.sleep)
        {
            self.next = None;
            self.length = 0;
            self.status = 0x8010;
            self.busy = self.s.now + self.s.ticks(if self.model == 26 { 1500 } else { 240 });
            return true;
        }
        if self.sleep {
            return false;
        }
        self.length = 0;
        let us = if self.model == 26 {
            match cmd {
                0x3041 => {
                    self.status &= !0x8c10;
                    return true;
                }
                0x306d => {
                    self.status |= 0x2000;
                    return true;
                }
                0x3066 => {
                    self.status &= !0x2000;
                    return true;
                }
                0xf32d => {
                    self.word(0, self.status);
                    self.length = 3;
                    return true;
                }
                0x2400 => 15500,
                0x240b => 6500,
                0x2416 => 4500,
                _ => return false,
            }
        } else {
            match cmd {
                0xb098 => {
                    self.sleep = true;
                    return true;
                }
                0xefc8 => {
                    self.word(0, 0x0807);
                    self.length = 3;
                    return true;
                }
                0x7866 | 0x58e0 => 12100,
                0x609c | 0x401a => 800,
                _ => return false,
            }
        };
        self.humidity_first = matches!(cmd, 0x58e0 | 0x401a);
        self.sample = [self.s.inputs[0], self.s.inputs[1]];
        self.next = Some(self.s.now + self.s.ticks(us));
        true
    }
}
impl RegisterSensor for Humidity {
    fn format(&self) -> WireFormat {
        WireFormat::Command
    }
    fn start(&mut self, _read: bool) {
        self.first = None;
    }
    fn read_ready(&self) -> bool {
        !self.sleep && self.s.now >= self.busy && self.length > 0
    }
    fn sync(&mut self) {
        self.s.time();
        if self.next.is_some_and(|at| at <= self.s.now) {
            self.next = None;
            let denominator = if self.model == 27 { 65536. } else { 65535. };
            let t = ((self.sample[0] + 45.) * denominator / 175.)
                .round()
                .clamp(0., 65535.) as u16;
            let h = (self.sample[1] * denominator / 100.)
                .round()
                .clamp(0., 65535.) as u16;
            self.word(0, if self.humidity_first { h } else { t });
            self.word(3, if self.humidity_first { t } else { h });
            self.length = 6;
            self.s.readings[0] = -45. + 175. * t as f64 / denominator;
            self.s.readings[1] = 100. * h as f64 / denominator;
            self.s.publish(1);
        }
    }
    fn registers(&self) -> [u8; 256] {
        self.s.regs
    }
    fn write(&mut self, _reg: u8, value: u16) -> bool {
        if let Some(hi) = self.first.take() {
            self.command(u16::from_be_bytes([hi, value as u8]))
        } else {
            self.first = Some(value as u8);
            true
        }
    }
    fn read_done(&mut self, reg: u8) {
        if self.length > 0 && reg == self.length - 1 {
            self.length = 0;
        }
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        if !value.is_finite()
            || !match field {
                0 => (-40.0..=125.).contains(&value),
                1 => (0.0..=100.).contains(&value),
                _ => false,
            }
        {
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

pub(super) struct Veml7700 {
    s: SampleState,
    words: [u16; 7],
    next: Option<u64>,
    above: u64,
    below: u64,
}
impl Veml7700 {
    pub fn new(clock: Arc<AtomicU64>, hz: u32) -> Self {
        let mut s = SampleState::new(clock, hz);
        s.inputs[3] = 100.;
        Self {
            s,
            words: [1, 0, 0, 0, 0, 0, 0],
            next: None,
            above: 0,
            below: 0,
        }
    }
    fn integration_us(&self) -> u64 {
        match (self.words[0] >> 6) & 15 {
            0 => 100000,
            1 => 200000,
            2 => 400000,
            3 => 800000,
            8 => 50000,
            12 => 25000,
            _ => 100000,
        }
    }
    fn sensitivity(&self) -> f64 {
        0.0576 / [1., 2., 0.125, 0.25][((self.words[0] >> 11) & 3) as usize] * 100000.
            / self.integration_us() as f64
    }
    fn period(&self) -> u64 {
        self.s.ticks(
            self.integration_us()
                + if self.words[3] & 1 != 0 {
                    [500000, 1000000, 2000000, 4000000][((self.words[3] >> 1) & 3) as usize]
                } else {
                    0
                },
        )
    }
}
impl RegisterSensor for Veml7700 {
    fn format(&self) -> WireFormat {
        WireFormat::Word
    }
    fn sync(&mut self) {
        self.s.time();
        let Some(at) = self.next else { return };
        if at > self.s.now {
            return;
        }
        let period = self.period().max(1);
        let count = 1 + (self.s.now - at) / period;
        self.next = Some(at + count * period);
        let sensitivity = self.sensitivity();
        self.words[4] = (self.s.inputs[3] / sensitivity).round().clamp(0., 65535.) as u16;
        // Broadband spectrum held at a fixed white/ALS ratio; both channels saturate independently.
        self.words[5] = (self.s.inputs[3] * 1.2 / sensitivity)
            .round()
            .clamp(0., 65535.) as u16;
        self.s.readings[3] = self.words[4] as f64 * sensitivity;
        self.s.publish(count);
        self.above = if self.words[4] > self.words[1] {
            self.above.saturating_add(count)
        } else {
            0
        };
        self.below = if self.words[4] < self.words[2] {
            self.below.saturating_add(count)
        } else {
            0
        };
        let persistence = 1u64 << ((self.words[0] >> 4) & 3);
        if self.words[0] & 2 != 0 {
            if self.above >= persistence {
                self.words[6] |= 0x4000;
            }
            if self.below >= persistence {
                self.words[6] |= 0x8000;
            }
        }
    }
    fn registers(&self) -> [u8; 256] {
        let mut bytes = [0; 256];
        for (i, word) in self.words.iter().enumerate() {
            bytes[i * 2..i * 2 + 2].copy_from_slice(&word.to_le_bytes());
        }
        bytes
    }
    fn write(&mut self, reg: u8, value: u16) -> bool {
        self.sync();
        let value = value.swap_bytes();
        match reg {
            0 => {
                if !matches!((value >> 6) & 15, 0 | 1 | 2 | 3 | 8 | 12) {
                    return false;
                }
                let before = self.words[0];
                self.words[0] = value & 0x1bf3;
                if (before ^ self.words[0]) & 0x32 != 0 {
                    self.above = 0;
                    self.below = 0;
                }
                if (before ^ self.words[0]) & 0x1bc1 != 0 {
                    self.next = if value & 1 != 0 {
                        None
                    } else {
                        Some(
                            self.s.now
                                + self.s.ticks(
                                    self.integration_us() + if before & 1 != 0 { 2500 } else { 0 },
                                ),
                        )
                    };
                    self.above = 0;
                    self.below = 0;
                }
            }
            1 | 2 => self.words[reg as usize] = value,
            3 => {
                self.words[3] = value & 7;
                if self.words[0] & 1 == 0 {
                    self.next = Some(self.s.now + self.period());
                }
            }
            _ => return false,
        }
        true
    }
    fn read_done(&mut self, reg: u8) {
        if reg == 6 {
            self.words[6] = 0;
        }
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        if field != 3 || !value.is_finite() || !(0.0..=120000.).contains(&value) {
            return false;
        }
        self.sync();
        self.s.inputs[3] = value;
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
    fn device(model: u8, address: u8) -> (Arc<AtomicU64>, Arc<Mutex<Sensor>>, SensorI2c) {
        let clock = Arc::new(AtomicU64::new(0));
        let state = Arc::new(Mutex::new(Sensor::new(
            SensorConfig {
                id: 0,
                sda: 4,
                scl: 5,
                address,
                model,
                shunt_milliohms: 0,
            },
            clock.clone(),
            1_000_000,
        )));
        let wire = SensorI2c::new(state.clone());
        (clock, state, wire)
    }
    fn command(w: &mut SensorI2c, cmd: u16) -> bool {
        assert!(w.start(false));
        let b = cmd.to_be_bytes();
        assert!(w.write(b[0]));
        let ok = w.write(b[1]);
        w.stop();
        ok
    }
    fn sample(w: &mut SensorI2c) -> [u8; 6] {
        assert!(w.start(true));
        let b = std::array::from_fn(|_| w.read());
        w.stop();
        b
    }
    fn write(w: &mut SensorI2c, reg: u8, value: u16) -> bool {
        assert!(w.start(false));
        assert!(w.write(reg));
        let b = value.to_le_bytes();
        assert!(w.write(b[0]));
        let ok = w.write(b[1]);
        w.stop();
        ok
    }
    fn read(w: &mut SensorI2c, reg: u8) -> u16 {
        assert!(w.start(false));
        assert!(w.write(reg));
        assert!(w.start(true));
        let word = u16::from_le_bytes([w.read(), w.read()]);
        w.stop();
        word
    }
    #[test]
    fn humidity_crc_timing_snapshot_sleep_and_instance_boundaries() {
        for (model, address, cmd, us) in [(26, 0x44, 0x2400, 15500), (27, 0x70, 0x609c, 800)] {
            let (clock, state, mut w) = device(model, address);
            assert_eq!(w.pins(), Some((4, 5)));
            assert!(!w.start(true));
            assert!(command(&mut w, cmd));
            assert!(!command(&mut w, cmd));
            assert!(state.lock().unwrap().set(0, 40.));
            clock.store(us - 1, Ordering::Relaxed);
            assert!(!w.start(true));
            clock.store(us, Ordering::Relaxed);
            let bytes = sample(&mut w);
            for word in bytes.chunks_exact(3) {
                assert_eq!(temperature::crc8(&word[..2], 0xff, 0x31), word[2]);
            }
            assert!((state.lock().unwrap().value(0) - 25.).abs() < 0.01);
            assert!(!w.start(true));
            for _ in 0..256 {
                w.read();
            }
            assert!(command(&mut w, cmd));
            clock.store(us * 2, Ordering::Relaxed);
            sample(&mut w);
            assert!((state.lock().unwrap().value(0) - 40.).abs() < 0.01);
            let (_, other, _) = device(model, address);
            assert!(other.lock().unwrap().value(0).is_nan());
            if model == 27 {
                assert!(command(&mut w, 0xb098));
                assert!(!command(&mut w, 0xefc8));
                assert!(command(&mut w, 0x3517));
                assert!(!command(&mut w, 0xefc8));
                clock.store(us * 2 + 240, Ordering::Relaxed);
                assert!(command(&mut w, 0xefc8));
                assert!(w.start(true));
                assert_eq!([w.read(), w.read()], [8, 7]);
            } else {
                assert!(command(&mut w, 0x306d));
                assert!(command(&mut w, 0xf32d));
                assert!(w.start(true));
                assert_eq!([w.read(), w.read()], [0xa0, 0x10]);
                assert!(command(&mut w, 0x30a2));
                assert!(!command(&mut w, 0xf32d));
                clock.store(us * 2 + 1500, Ordering::Relaxed);
                assert!(command(&mut w, 0xf32d));
                assert!(w.start(true));
                assert_eq!([w.read(), w.read()], [0x80, 0x10]);
            }
            if model == 27 {
                command(&mut w, 0x3517);
                clock.fetch_add(240, Ordering::Relaxed);
            }
            assert!(command(&mut w, cmd));
            let reset = if model == 26 { 0x30a2 } else { 0x805d };
            assert!(!command(&mut w, reset));
            clock.fetch_add(us, Ordering::Relaxed);
            sample(&mut w);
            assert!(command(&mut w, reset));
            clock.fetch_add(1500, Ordering::Relaxed);
            assert!(!w.start(true));
            if model == 26 {
                assert!(command(&mut w, 0x3041));
                assert!(command(&mut w, 0xf32d));
                assert!(w.start(true));
                assert_eq!([w.read(), w.read()], [0, 0]);
            }
            assert!(!state.lock().unwrap().set(0, 126.));
            assert!(!state.lock().unwrap().set(1, f64::NAN));
        }
    }
    #[test]
    fn light_register_endianness_gain_integration_shutdown_saturation_and_irq() {
        let (clock, state, mut w) = device(28, 0x10);
        assert_eq!(read(&mut w, 0), 1);
        assert!(write(&mut w, 0, 0));
        clock.store(102499, Ordering::Relaxed);
        assert_eq!(read(&mut w, 4), 0);
        clock.store(102500, Ordering::Relaxed);
        assert_eq!(read(&mut w, 4), 1736);
        assert_eq!(read(&mut w, 5), 2083);
        assert!(write(&mut w, 0, 0x840));
        clock.store(302500, Ordering::Relaxed);
        assert_eq!(read(&mut w, 4), 6944);
        assert!(write(&mut w, 0, 0x841));
        assert!(state.lock().unwrap().set(3, 200.));
        clock.store(1000000, Ordering::Relaxed);
        assert_eq!(read(&mut w, 4), 6944);
        assert!(write(&mut w, 0, 0x840));
        clock.store(1202500, Ordering::Relaxed);
        assert_eq!(read(&mut w, 4), 13889);
        assert!(!write(&mut w, 4, 0));
        assert!(!write(&mut w, 0, 0x1c0));
        assert!(write(&mut w, 1, 100));
        assert!(write(&mut w, 0, 0x842));
        clock.store(1402500, Ordering::Relaxed);
        assert_eq!(read(&mut w, 6), 0x4000);
        assert_eq!(read(&mut w, 6), 0);
        assert!(state.lock().unwrap().set(3, 120000.));
        clock.store(1602500, Ordering::Relaxed);
        assert_eq!(read(&mut w, 4), 65535);
        assert_eq!(read(&mut w, 5), 65535);
        assert!(!state.lock().unwrap().set(3, f64::INFINITY));
        assert!(!state.lock().unwrap().set(0, 1.));
    }
    #[test]
    fn humidity_sensor_addresses_and_parameters_are_validated() {
        for (model, valid, bad) in [(26, 0x45, 0x46), (27, 0x70, 0x71), (28, 0x10, 0x11)] {
            let mut c = SensorConfig {
                id: 0,
                sda: 4,
                scl: 5,
                address: valid,
                model,
                shunt_milliohms: 0,
            };
            assert!(c.valid());
            c.address = bad;
            assert!(!c.valid());
            c.address = valid;
            c.shunt_milliohms = 1;
            assert!(!c.valid());
        }
    }
}
