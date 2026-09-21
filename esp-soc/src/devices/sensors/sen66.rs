use super::*;
const FIELDS: [usize; 9] = [77, 78, 79, 80, 1, 0, 81, 82, 47];
const SCALES: [f64; 9] = [10., 10., 10., 10., 100., 200., 10., 10., 1.];
const INVALID: [u16; 9] = [
    0xffff, 0xffff, 0xffff, 0xffff, 0x7fff, 0x7fff, 0x7fff, 0x7fff, 0xffff,
];
pub(super) struct Sen66 {
    s: SampleState,
    tx: [u8; 2],
    count: usize,
    reading: bool,
    response: usize,
    command: u16,
    response_generation: u32,
    busy: u64,
    reset_at: u64,
    started: Option<u64>,
    next: Option<u64>,
    ready: bool,
    values: [u16; 9],
}
impl Sen66 {
    pub fn new(clock: Arc<AtomicU64>, hz: u32) -> Self {
        let mut s = SampleState::new(clock, hz);
        s.time();
        for (f, v) in FIELDS
            .into_iter()
            .zip([5., 8., 10., 12., 50., 25., 100., 1., 400.])
        {
            s.inputs[f] = v;
        }
        let now = s.now;
        let busy = now + s.ticks(100_000);
        Self {
            s,
            tx: [0; 2],
            count: 0,
            reading: false,
            response: 0,
            command: 0,
            response_generation: 0,
            busy,
            reset_at: now,
            started: None,
            next: None,
            ready: false,
            values: INVALID,
        }
    }
    fn valid_command(&self, cmd: u16) -> bool {
        match cmd {
            0x0021 => self.started.is_none(),
            0x0104 | 0x0202 | 0x0300 => self.started.is_some(),
            0xd304 => true,
            _ => false,
        }
    }
    fn word(&mut self, value: u16) {
        let b = value.to_be_bytes();
        let n = self.response;
        self.s.regs[n..n + 2].copy_from_slice(&b);
        self.s.regs[n + 2] = temperature::crc8(&b, 0xff, 0x31);
        self.response += 3;
    }
    fn execute(&mut self) {
        if self.count != 2 || self.reading {
            return;
        }
        let cmd = u16::from_be_bytes(self.tx);
        self.count = 0;
        if !self.valid_command(cmd) {
            return;
        }
        self.response = 0;
        self.command = cmd;
        self.s.regs.fill(0xff);
        let delay = match cmd {
            0x0021 => {
                self.started = Some(self.s.now);
                self.next = Some(self.s.now + self.s.ticks(1_100_000));
                self.values = INVALID;
                self.ready = false;
                self.s.readings.fill(f64::NAN);
                50_000
            }
            0x0104 => {
                self.started = None;
                self.next = None;
                self.ready = false;
                1_400_000
            }
            0x0202 => {
                self.word(self.ready as u16);
                20_000
            }
            0x0300 => {
                self.response_generation = self.s.generation;
                for v in self.values {
                    self.word(v);
                }
                20_000
            }
            0xd304 => {
                self.started = None;
                self.next = None;
                self.ready = false;
                self.values = INVALID;
                self.s.readings.fill(f64::NAN);
                self.reset_at = self.s.now;
                1_200_000
            }
            _ => unreachable!(),
        };
        self.busy = self.s.now + self.s.ticks(delay);
    }
}
impl RegisterSensor for Sen66 {
    fn format(&self) -> WireFormat {
        WireFormat::Command
    }
    fn start(&mut self, read: bool) {
        self.execute();
        self.count = 0;
        self.reading = read;
        if !read {
            self.response = 0;
        }
    }
    fn stop(&mut self) {
        self.execute();
        self.count = 0;
        if self.reading {
            self.response = 0;
        }
    }
    fn address_ready(&self) -> bool {
        self.s.now >= self.busy
    }
    fn read_ready(&self) -> bool {
        self.response > 0 && self.s.now >= self.busy
    }
    fn sync(&mut self) {
        self.s.time();
        let Some(at) = self.next else { return };
        if self.s.now < at {
            return;
        }
        let period = self.s.ticks(1_000_000);
        let n = 1 + (self.s.now - at) / period;
        let sampled_at = at + (n - 1) * period;
        for i in 0..9 {
            let unknown = i == 7 && sampled_at < self.reset_at + self.s.ticks(11_000_000)
                || i == 8 && sampled_at < self.started.unwrap() + self.s.ticks(6_000_000);
            if unknown {
                self.values[i] = INVALID[i];
                self.s.readings[FIELDS[i]] = f64::NAN;
            } else {
                let raw = (self.s.inputs[FIELDS[i]] * SCALES[i]).round() as i32;
                self.values[i] = raw as u16;
                self.s.readings[FIELDS[i]] = raw as f64 / SCALES[i];
            }
        }
        self.ready = true;
        self.next = Some(at + n * period);
        self.s.publish(n);
    }
    fn registers(&self) -> [u8; 256] {
        self.s.regs
    }
    fn read_done(&mut self, reg: u8) {
        if self.command == 0x0300 && reg == 26 && self.response_generation == self.s.generation {
            self.ready = false;
        }
    }
    fn write(&mut self, _reg: u8, value: u16) -> bool {
        self.sync();
        if self.count >= 2 {
            self.count = 3;
            return false;
        }
        self.tx[self.count] = value as u8;
        self.count += 1;
        if self.count == 2 && !self.valid_command(u16::from_be_bytes(self.tx)) {
            self.count = 3;
            return false;
        }
        true
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        self.sync();
        let valid = match field {
            0 => (-10. ..=50.).contains(&value),
            1 => (0. ..=90.).contains(&value),
            47 => (0. ..=40_000.).contains(&value) && value.fract() == 0.,
            77..=80 => (0. ..=1000.).contains(&value),
            81 | 82 => (1. ..=500.).contains(&value),
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
    fn cmd(d: &mut Sen66, command: u16) -> bool {
        d.start(false);
        let [a, b] = command.to_be_bytes();
        let accepted = d.write(0, a as u16) && d.write(0, b as u16);
        d.stop();
        accepted
    }
    fn words(d: &Sen66) -> Vec<u16> {
        d.s.regs[..d.response]
            .chunks_exact(3)
            .map(|b| {
                assert_eq!(b[2], temperature::crc8(&b[..2], 0xff, 0x31));
                u16::from_be_bytes([b[0], b[1]])
            })
            .collect()
    }
    #[test]
    fn sen66_crc_framing_readiness_quantization_warmup_stop_reset() {
        assert_eq!(temperature::crc8(&[0xbe, 0xef], 0xff, 0x31), 0x92);
        let c = Arc::new(AtomicU64::new(0));
        let mut d = Sen66::new(c.clone(), 1_000_000);
        assert!(!d.address_ready());
        c.store(99_999, Ordering::Relaxed);
        d.sync();
        assert!(!d.address_ready());
        c.store(100_000, Ordering::Relaxed);
        d.sync();
        assert!(d.address_ready());
        d.start(false);
        assert!(d.write(0, 0));
        d.stop();
        assert!(d.started.is_none());
        d.start(false);
        assert!(d.write(0, 0));
        assert!(d.write(0, 0x21));
        assert!(!d.write(0, 0));
        d.stop();
        assert!(d.started.is_none());
        assert!(!cmd(&mut d, 0x0300));
        assert!(!cmd(&mut d, 0x5607));
        assert!(!cmd(&mut d, 0x0020));
        assert!(cmd(&mut d, 0x0021));
        assert!(!d.address_ready());
        c.store(149_999, Ordering::Relaxed);
        d.sync();
        assert!(!d.address_ready());
        c.store(150_000, Ordering::Relaxed);
        d.sync();
        assert!(d.address_ready());
        assert!(cmd(&mut d, 0x0300));
        assert_eq!(words(&d), INVALID);
        assert!(!d.read_ready());
        c.store(170_000, Ordering::Relaxed);
        d.sync();
        assert!(d.read_ready());
        assert!(d.set(0, -5.555));
        assert!(d.set(1, 67.894));
        assert!(d.set(78, 123.46));
        assert!(d.set(81, 154.76));
        assert!(d.set(47, 876.));
        assert!(!d.set(0, f64::NAN));
        assert!(!d.set(77, 1001.));
        assert!(!d.set(81, 0.));
        assert!(!d.set(47, 123.5));
        assert!(!d.set(18, 1.));
        c.store(1_199_999, Ordering::Relaxed);
        assert_eq!(d.generation(), 0);
        c.store(1_200_000, Ordering::Relaxed);
        assert_eq!(d.generation(), 1);
        assert!(cmd(&mut d, 0x0202));
        assert_eq!(words(&d), [1]);
        c.store(1_220_000, Ordering::Relaxed);
        d.sync();
        assert!(cmd(&mut d, 0x0300));
        assert_eq!(
            words(&d),
            [
                50,
                1235,
                100,
                120,
                6789,
                (-1111i16) as u16,
                1548,
                0x7fff,
                0xffff
            ]
        );
        assert_eq!(d.value(78), 123.5);
        assert_eq!(d.value(0), -5.555);
        assert!(d.value(82).is_nan());
        assert!(d.value(47).is_nan());
        d.read_done(26);
        assert!(!d.ready);
        c.store(6_200_000, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.value(47), 876.);
        assert!(d.value(82).is_nan());
        c.store(11_200_000, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.value(82), 1.);
        assert!(cmd(&mut d, 0x0300));
        c.store(12_200_000, Ordering::Relaxed);
        d.sync();
        d.read_done(26);
        assert!(d.ready, "reading old snapshot must not clear newer sample");
        assert!(cmd(&mut d, 0x0104));
        let g = d.generation();
        c.store(13_599_999, Ordering::Relaxed);
        d.sync();
        assert!(!d.address_ready());
        c.store(13_600_000, Ordering::Relaxed);
        assert_eq!(d.generation(), g);
        assert!(d.address_ready());
        assert!(!cmd(&mut d, 0x0300));
        assert!(cmd(&mut d, 0xd304));
        c.store(14_799_999, Ordering::Relaxed);
        d.sync();
        assert!(!d.address_ready());
        c.store(14_800_000, Ordering::Relaxed);
        d.sync();
        assert!(d.address_ready());
        assert!(d.value(0).is_nan());
        assert!(cmd(&mut d, 0x0021));
        c.store(20_900_000, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.value(47), 876.);
        assert!(d.value(82).is_nan());
    }
    #[test]
    fn sen66_i2c_address_route_crc_and_partial_reads() {
        let conf = SensorConfig {
            id: 0,
            sda: 4,
            scl: 5,
            address: 0x6b,
            model: 48,
            shunt_milliohms: 0,
        };
        assert!(conf.valid());
        assert!(!SensorConfig {
            address: 0x6a,
            ..conf
        }
        .valid());
        let c = Arc::new(AtomicU64::new(0));
        let state = Arc::new(Mutex::new(Sensor::new(conf, c.clone(), 1_000_000)));
        let mut b = SensorI2c::new(state.clone());
        assert_eq!(b.pins(), Some((4, 5)));
        assert!(!b.start(false));
        c.store(100_000, Ordering::Relaxed);
        assert!(b.start(false));
        assert!(b.write(0));
        assert!(b.write(0x21));
        b.stop();
        assert!(!b.start(false));
        c.store(2_000_000, Ordering::Relaxed);
        assert!(b.start(false));
        assert!(b.write(3));
        assert!(b.write(0));
        b.stop();
        assert!(!b.start(true));
        c.store(2_020_000, Ordering::Relaxed);
        assert!(b.start(true));
        assert_eq!(
            [b.read(), b.read(), b.read()],
            [0, 50, temperature::crc8(&[0, 50], 0xff, 0x31)]
        );
        b.stop();
        assert!(b.start(false));
        assert!(b.write(2));
        assert!(b.write(2));
        b.stop();
        c.store(2_040_000, Ordering::Relaxed);
        assert!(b.start(true));
        assert_eq!([b.read(), b.read()], [0, 1]);
        b.stop();
    }
}
