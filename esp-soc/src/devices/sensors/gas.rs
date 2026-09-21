use super::*;

pub(super) struct Gas {
    s: SampleState,
    model: u8,
    id: u8,
    tx: [u8; 8],
    count: usize,
    response: usize,
    reading: bool,
    busy: u64,
    initialized: Option<u64>,
    heater: bool,
    baseline: [u16; 2],
    compensation: [u16; 2],
    pending: Vec<(usize, u16)>,
}
impl Gas {
    pub fn new(clock: Arc<AtomicU64>, hz: u32, model: u8, id: u8) -> Self {
        let mut s = SampleState::new(clock, hz);
        for (field, value) in [
            (47, 400.),
            (59, 0.),
            (60, 13000.),
            (61, 18000.),
            (62, 25000.),
            (63, 15000.),
        ] {
            s.inputs[field] = value;
        }
        let busy = s.ticks(600);
        Self {
            s,
            model,
            id,
            tx: [0; 8],
            count: 0,
            response: 0,
            reading: false,
            busy,
            initialized: None,
            heater: false,
            baseline: [0; 2],
            compensation: [0; 2],
            pending: Vec::new(),
        }
    }
    fn length(&self, cmd: u16) -> Option<usize> {
        match (self.model, cmd) {
            (_, 0x3682) => Some(2),
            (32, 0x2003 | 0x2008 | 0x2015 | 0x202f | 0x2032 | 0x2050 | 0x20b3) => Some(2),
            (32, 0x201e) | (33, 0x2612 | 0x2619) => Some(8),
            (32, 0x2061 | 0x2077) => Some(5),
            (33, 0x280e | 0x3615) => Some(2),
            _ => None,
        }
    }
    fn word(&mut self, value: u16) {
        let offset = self.response;
        let b = value.to_be_bytes();
        self.s.regs[offset..offset + 2].copy_from_slice(&b);
        self.s.regs[offset + 2] = temperature::crc8(&b, 0xff, 0x31);
        self.response += 3;
    }
    fn sample(&mut self, field: usize, value: u16) {
        self.word(value);
        self.pending.push((field, value));
    }
    fn execute(&mut self, cmd: u16) -> bool {
        self.s.regs.fill(0xff);
        self.response = 0;
        self.pending.clear();
        let arg = |offset| u16::from_be_bytes([self.tx[offset], self.tx[offset + 1]]);
        let args = [arg(2), arg(5)];
        let delay = match (self.model, cmd) {
            (_, 0x3682) => {
                if self.model == 33 && self.heater {
                    return false;
                }
                self.word(0x1234);
                self.word(self.model as u16);
                self.word(0x100 + self.id as u16);
                if self.model == 32 {
                    500
                } else {
                    1000
                }
            }
            (32, 0x202f) => {
                self.word(0x0022);
                10000
            }
            (32, 0x2003) => {
                self.initialized = Some(self.s.now);
                10000
            }
            (32, 0x2008) => {
                let Some(at) = self.initialized else {
                    return false;
                };
                let warm = self.s.now - at < self.s.ticks(15_000_000);
                self.sample(47, if warm { 400 } else { self.s.inputs[47] as u16 });
                self.sample(59, if warm { 0 } else { self.s.inputs[59] as u16 });
                12000
            }
            (32, 0x2050) => {
                self.sample(60, self.s.inputs[60] as u16);
                self.sample(61, self.s.inputs[61] as u16);
                25000
            }
            (32, 0x2015) => {
                self.word(self.baseline[0]);
                self.word(self.baseline[1]);
                10000
            }
            (32, 0x201e) => {
                self.baseline = [args[1], args[0]];
                10000
            }
            (32, 0x2061) => {
                self.compensation[0] = args[0];
                10000
            }
            (32, 0x2032) => {
                if self.initialized.is_some() {
                    return false;
                }
                self.word(0xd400);
                220000
            }
            (32, 0x20b3) => {
                self.word(0x8000);
                10000
            }
            (32, 0x2077) => {
                self.baseline[1] = args[0];
                10000
            }
            (33, 0x2612) => {
                if args != [0x8000, 0x6666] {
                    return false;
                }
                self.heater = true;
                self.sample(62, self.s.inputs[62] as u16);
                50000
            }
            (33, 0x2619) => {
                self.heater = true;
                self.compensation = args;
                self.sample(62, self.s.inputs[62] as u16);
                self.sample(63, self.s.inputs[63] as u16);
                50000
            }
            (33, 0x280e) => {
                self.word(0xd400);
                320000
            }
            (33, 0x3615) => {
                self.heater = false;
                1000
            }
            _ => return false,
        };
        self.busy = self.s.now + self.s.ticks(delay);
        true
    }
}
impl RegisterSensor for Gas {
    fn format(&self) -> WireFormat {
        WireFormat::Command
    }
    fn general_reset(&mut self) -> bool {
        self.sync();
        self.initialized = None;
        self.heater = false;
        self.baseline = [0; 2];
        self.compensation = [0; 2];
        self.response = 0;
        self.pending.clear();
        self.count = 0;
        self.busy = self.s.now + self.s.ticks(600);
        true
    }
    fn start(&mut self, read: bool) {
        self.count = 0;
        self.reading = read;
    }
    fn stop(&mut self) {
        if self.reading {
            self.response = 0;
        }
        self.count = 0;
    }
    fn address_ready(&self) -> bool {
        self.s.now >= self.busy
    }
    fn read_ready(&self) -> bool {
        self.response > 0 && self.s.now >= self.busy
    }
    fn sync(&mut self) {
        self.s.time();
        if self.s.now >= self.busy && !self.pending.is_empty() {
            for (field, value) in self.pending.drain(..) {
                self.s.readings[field] = value as f64;
            }
            self.s.publish(1);
        }
    }
    fn registers(&self) -> [u8; 256] {
        self.s.regs
    }
    fn next_address(&self, reg: u8) -> u8 {
        reg.saturating_add(1)
    }
    fn write(&mut self, _reg: u8, value: u16) -> bool {
        self.sync();
        if self.s.now < self.busy || self.count >= self.tx.len() {
            return false;
        }
        self.tx[self.count] = value as u8;
        self.count += 1;
        if self.count < 2 {
            return true;
        }
        let cmd = u16::from_be_bytes([self.tx[0], self.tx[1]]);
        let Some(length) = self.length(cmd) else {
            return false;
        };
        if self.count > length {
            return false;
        }
        if self.count >= 5 && (self.count - 2) % 3 == 0 {
            let i = self.count - 3;
            if temperature::crc8(&self.tx[i..i + 2], 0xff, 0x31) != self.tx[i + 2] {
                self.count = 8;
                return false;
            }
        }
        self.count < length || self.execute(cmd)
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        let valid = match (self.model, field) {
            (32, 47) => (400.0..=60000.).contains(&value),
            (32, 59) => (0.0..=60000.).contains(&value),
            (32, 60 | 61) | (33, 62 | 63) => (0.0..=65535.).contains(&value),
            _ => false,
        };
        if !valid || !value.is_finite() {
            return false;
        }
        self.sync();
        self.s.inputs[field as usize] = value.round();
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
    fn device(model: u8) -> (Arc<AtomicU64>, Arc<Mutex<Sensor>>, SensorI2c) {
        let clock = Arc::new(AtomicU64::new(10000));
        let state = Arc::new(Mutex::new(Sensor::new(
            SensorConfig {
                id: 1,
                sda: 4,
                scl: 5,
                address: if model == 32 { 0x58 } else { 0x59 },
                model,
                shunt_milliohms: 0,
            },
            clock.clone(),
            1_000_000,
        )));
        let wire = SensorI2c::new(state.clone());
        (clock, state, wire)
    }
    fn command(w: &mut SensorI2c, cmd: u16, args: &[u16]) -> bool {
        if !w.start_address(0x58, false) {
            return false;
        }
        let mut bytes = cmd.to_be_bytes().to_vec();
        for arg in args {
            let b = arg.to_be_bytes();
            bytes.extend(b);
            bytes.push(temperature::crc8(&b, 0xff, 0x31));
        }
        let ok = bytes.into_iter().all(|b| w.write(b));
        w.stop();
        ok
    }
    fn read(w: &mut SensorI2c, count: usize) -> Vec<u16> {
        assert!(w.start(true));
        let out = (0..count)
            .map(|_| {
                let b = [w.read(), w.read()];
                assert_eq!(w.read(), temperature::crc8(&b, 0xff, 0x31));
                u16::from_be_bytes(b)
            })
            .collect();
        assert_eq!(w.read(), 0xff);
        w.stop();
        assert!(!w.start(true));
        out
    }
    #[test]
    fn sgp30_warmup_snapshot_baseline_crc_and_general_reset() {
        let (clock, state, mut w) = device(32);
        assert!(!command(&mut w, 0x2008, &[]));
        assert!(command(&mut w, 0x2003, &[]));
        assert!(!command(&mut w, 0x2008, &[]));
        clock.store(20000, Ordering::Relaxed);
        assert!(state.lock().unwrap().set(47, 900.));
        assert!(state.lock().unwrap().set(59, 123.));
        assert!(command(&mut w, 0x2008, &[]));
        clock.store(31999, Ordering::Relaxed);
        assert!(!w.start(true));
        clock.store(32000, Ordering::Relaxed);
        assert_eq!(read(&mut w, 2), [400, 0]);
        clock.store(15_010_000, Ordering::Relaxed);
        assert!(command(&mut w, 0x2008, &[]));
        state.lock().unwrap().set(47, 1000.);
        clock.fetch_add(12000, Ordering::Relaxed);
        assert_eq!(read(&mut w, 2), [900, 123]);
        assert_eq!(state.lock().unwrap().value(47), 900.);
        assert!(command(&mut w, 0x201e, &[0xabcd, 0x1234]));
        clock.fetch_add(10000, Ordering::Relaxed);
        assert!(command(&mut w, 0x2015, &[]));
        clock.fetch_add(10000, Ordering::Relaxed);
        assert_eq!(read(&mut w, 2), [0x1234, 0xabcd]);
        assert!(w.start(false));
        for b in [0x20, 0x61, 0x12, 0x34] {
            assert!(w.write(b));
        }
        assert!(!w.write(0));
        w.stop();
        assert!(w.matches_address(0x58, 0, false));
        assert!(!w.matches_address(0x58, 0, true));
        assert!(w.start_address(0, false));
        assert!(w.write(6));
        assert!(!w.write(6));
        w.stop();
        clock.fetch_add(599, Ordering::Relaxed);
        assert!(!w.start_address(0x58, false));
        clock.fetch_add(1, Ordering::Relaxed);
        assert!(!command(&mut w, 0x2008, &[]));
        assert!(command(&mut w, 0x2015, &[]));
        clock.fetch_add(10000, Ordering::Relaxed);
        assert_eq!(read(&mut w, 2), [0, 0]);
        assert!(!state.lock().unwrap().set(47, 399.));
        assert!(!state.lock().unwrap().set(59, 60001.));
        assert!(!state.lock().unwrap().set(62, 1.));
    }
    #[test]
    fn sgp41_crc_busy_reads_heater_and_instances() {
        let (clock, state, mut w) = device(33);
        assert!(command(&mut w, 0x3682, &[]));
        clock.fetch_add(1000, Ordering::Relaxed);
        assert_eq!(read(&mut w, 3), [0x1234, 33, 0x101]);
        assert!(command(&mut w, 0x280e, &[]));
        clock.fetch_add(319999, Ordering::Relaxed);
        assert!(!w.start(true));
        clock.fetch_add(1, Ordering::Relaxed);
        assert_eq!(read(&mut w, 1), [0xd400]);
        assert!(!command(&mut w, 0x2612, &[0, 0]));
        assert!(command(&mut w, 0x2612, &[0x8000, 0x6666]));
        clock.fetch_add(50000, Ordering::Relaxed);
        assert_eq!(read(&mut w, 1), [25000]);
        assert!(command(&mut w, 0x2619, &[0x8000, 0x6666]));
        state.lock().unwrap().set(62, 33000.);
        clock.fetch_add(50000, Ordering::Relaxed);
        assert_eq!(read(&mut w, 2), [25000, 15000]);
        assert!(command(&mut w, 0x2619, &[0, 0]));
        clock.fetch_add(50000, Ordering::Relaxed);
        assert_eq!(read(&mut w, 2), [33000, 15000]);
        assert!(command(&mut w, 0x3615, &[]));
        clock.fetch_add(1000, Ordering::Relaxed);
        assert!(!w.start(true));
        assert!(command(&mut w, 0x3682, &[]));
        clock.fetch_add(1000, Ordering::Relaxed);
        assert!(w.start(true));
        w.read();
        w.stop();
        assert!(!w.start(true));
        let (_, other, _) = device(33);
        assert!(other.lock().unwrap().value(62).is_nan());
        assert!(!state.lock().unwrap().set(62, 65536.));
        assert!(!state.lock().unwrap().set(63, f64::NAN));
    }
}
