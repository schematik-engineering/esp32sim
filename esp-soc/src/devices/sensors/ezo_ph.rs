use super::*;

// Atlas EZO-pH datasheet v6.1, I2C command/response protocol, pp. 39-60.
pub(super) struct EzoPh {
    s: SampleState,
    address: u8,
    command: Vec<u8>,
    pending: Option<(u64, Vec<u8>)>,
    reading: bool,
    overflow: bool,
    asleep: bool,
    extended: bool,
    led: bool,
    compensation: f64,
    name: String,
}
impl EzoPh {
    pub fn new(clock: Arc<AtomicU64>, hz: u32, address: u8) -> Self {
        let mut s = SampleState::new(clock, hz);
        s.inputs[76] = 7.;
        s.inputs[0] = 25.;
        s.regs[0] = 255;
        Self {
            s,
            address,
            command: Vec::new(),
            pending: None,
            reading: false,
            overflow: false,
            asleep: false,
            extended: false,
            led: true,
            compensation: 25.,
            name: String::new(),
        }
    }
    fn response(&mut self, code: u8, text: &str) {
        self.s.regs.fill(0);
        self.s.regs[0] = code;
        let bytes = text.as_bytes();
        self.s.regs[1..1 + bytes.len()].copy_from_slice(bytes);
    }
    fn number(text: &str) -> Option<f64> {
        if text.is_empty()
            || !text
                .bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'+'))
        {
            return None;
        }
        text.parse::<f64>().ok().filter(|n| n.is_finite())
    }
    fn address_command(bytes: &[u8]) -> Option<u8> {
        if !bytes.get(..4)?.eq_ignore_ascii_case(b"i2c,") {
            return None;
        }
        std::str::from_utf8(&bytes[4..])
            .ok()?
            .parse::<u8>()
            .ok()
            .filter(|a| (1..=127).contains(a))
    }
    fn execute(&mut self, bytes: &[u8]) {
        let Ok(text) = std::str::from_utf8(bytes) else {
            self.response(2, "");
            return;
        };
        let lower = text.to_ascii_lowercase();
        let parts: Vec<_> = lower.split(',').collect();
        self.response(1, "");
        match parts.as_slice() {
            ["r"] => self.capture(),
            ["rt", n] | ["t", n] if *n != "?" => {
                if let Some(value) = Self::number(n).filter(|n| *n > -273.15) {
                    self.compensation = value;
                    if parts[0] == "rt" {
                        self.capture();
                    }
                } else {
                    self.response(2, "");
                }
            }
            ["t", "?"] => self.response(1, &format!("?T,{:.3}", self.compensation)),
            ["phext", "0"] => self.extended = false,
            ["phext", "1"] => self.extended = true,
            ["phext", "?"] => self.response(1, &format!("?pHext,{}", self.extended as u8)),
            ["l", "0"] => self.led = false,
            ["l", "1"] => self.led = true,
            ["l", "?"] => self.response(1, &format!("?L,{}", self.led as u8)),
            ["name", "?"] => self.response(1, &format!("?Name,{}", self.name)),
            ["name", _]
                if text.len() <= 21
                    && text[5..].bytes().all(|b| b.is_ascii_graphic() && b != b',') =>
            {
                self.name = text[5..].into()
            }
            ["i"] => self.response(1, "?i,pH,1.98"),
            ["i2c", _] => {
                if let Some(address) = Self::address_command(bytes) {
                    self.address = address;
                    self.compensation = 25.;
                    self.response(255, "");
                } else {
                    self.response(2, "");
                }
            }
            ["factory"] => {
                self.led = true;
                self.extended = false;
                self.compensation = 25.;
                self.name.clear();
                self.response(255, "");
            }
            _ => self.response(2, ""),
        }
    }
    fn capture(&mut self) {
        // ponytail: ideal pH7 probe; calibration fixtures require slope/offset modeling.
        let ph = 7.
            + (self.s.inputs[76] - 7.) * (self.s.inputs[0] + 273.15) / (self.compensation + 273.15);
        let (min, max) = if self.extended {
            (-1.6, 15.6)
        } else {
            (0., 14.)
        };
        let ph = (ph.clamp(min, max) * 1000.).round() / 1000.;
        self.s.readings[76] = ph;
        self.s.publish(1);
        self.response(1, &format!("{ph:.3}"));
    }
}
impl RegisterSensor for EzoPh {
    fn format(&self) -> WireFormat {
        WireFormat::Command
    }
    fn address(&self, _configured: u8) -> u8 {
        self.pending
            .as_ref()
            .and_then(|(at, command)| {
                (self.s.clock.load(Ordering::Relaxed) >= *at)
                    .then(|| Self::address_command(command))
                    .flatten()
            })
            .unwrap_or(self.address)
    }
    fn start(&mut self, read: bool) {
        self.reading = read;
        if !read {
            self.command.clear();
            self.overflow = false;
        }
    }
    fn write(&mut self, _reg: u8, value: u16) -> bool {
        if self.command.len() < 40 {
            self.command.push(value as u8);
        } else {
            self.overflow = true;
        }
        true
    }
    fn stop(&mut self) {
        if self.reading || self.command.is_empty() {
            return;
        }
        self.s.time();
        self.asleep = false;
        if self.command.eq_ignore_ascii_case(b"sleep") && !self.overflow {
            self.asleep = true;
            self.pending = None;
            self.response(255, "");
            self.command.clear();
            return;
        }
        let cmd = std::mem::take(&mut self.command);
        let micros = if cmd.eq_ignore_ascii_case(b"r")
            || cmd.get(..3).is_some_and(|p| p.eq_ignore_ascii_case(b"rt,"))
        {
            900_000
        } else {
            300_000
        };
        self.pending = Some((
            self.s.now + self.s.ticks(micros),
            if self.overflow { vec![255] } else { cmd },
        ));
        self.response(254, "");
    }
    fn sync(&mut self) {
        self.s.time();
        if self
            .pending
            .as_ref()
            .is_some_and(|(at, _)| self.s.now >= *at)
        {
            let (_, cmd) = self.pending.take().unwrap();
            self.execute(&cmd);
        }
    }
    fn registers(&self) -> [u8; 256] {
        self.s.regs
    }
    fn read_ready(&self) -> bool {
        !self.asleep
    }
    fn read_done(&mut self, reg: u8) {
        if reg == 0 && self.s.regs[0] != 254 {
            self.response(255, "");
        }
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        let valid = value.is_finite()
            && match field {
                0 => (-40. ..=125.).contains(&value),
                76 => (-1.6..=15.6).contains(&value),
                _ => false,
            };
        if valid {
            self.s.inputs[field as usize] = value;
        }
        valid
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
    fn device() -> (Arc<AtomicU64>, Arc<Mutex<Sensor>>, SensorI2c) {
        let clock = Arc::new(AtomicU64::new(0));
        let config = SensorConfig {
            id: 0,
            sda: 4,
            scl: 5,
            address: 99,
            model: 45,
            shunt_milliohms: 0,
        };
        assert!(config.valid());
        let state = Arc::new(Mutex::new(Sensor::new(config, clock.clone(), 1_000_000)));
        (clock, state.clone(), SensorI2c::new(state))
    }
    fn command(d: &mut SensorI2c, text: &[u8]) {
        assert!(d.start(false));
        for b in text {
            assert!(d.write(*b));
        }
        d.stop();
    }
    fn read(d: &mut SensorI2c) -> (u8, String) {
        assert!(d.start(true));
        let status = d.read();
        let mut bytes = Vec::new();
        for _ in 0..40 {
            let b = d.read();
            if b == 0 {
                break;
            }
            bytes.push(b);
        }
        d.stop();
        (status, String::from_utf8(bytes).unwrap())
    }
    #[test]
    fn ezo_readiness_quantization_and_consumption() {
        let (clock, state, mut d) = device();
        assert_eq!(read(&mut d), (255, "".into()));
        assert!(state.lock().unwrap().value(76).is_nan());
        command(&mut d, b"r");
        clock.store(899_999, Ordering::Relaxed);
        assert_eq!(read(&mut d), (254, "".into()));
        assert!(state.lock().unwrap().set(76, 6.5434));
        clock.store(900_000, Ordering::Relaxed);
        assert_eq!(read(&mut d), (1, "6.543".into()));
        assert_eq!(state.lock().unwrap().value(76), 6.543);
        assert_eq!(state.lock().unwrap().generation(), 1);
        assert_eq!(read(&mut d), (255, "".into()));
        command(&mut d, b"T,nan");
        clock.store(1_200_000, Ordering::Relaxed);
        assert_eq!(read(&mut d).0, 2);
        command(&mut d, &[b'x'; 41]);
        clock.store(1_500_000, Ordering::Relaxed);
        assert_eq!(read(&mut d).0, 2);
        assert!(!state.lock().unwrap().set(76, f64::NAN));
        assert!(!state.lock().unwrap().set(76, 15.601));
        assert!(!state.lock().unwrap().set(75, 1.));
    }
    #[test]
    fn ezo_address_change_does_not_require_host_polling() {
        let (clock, _, mut d) = device();
        command(&mut d, b"i2c,100");
        clock.store(299_999, Ordering::Relaxed);
        assert!(d.matches_address(99, 99, false));
        assert!(!d.matches_address(99, 100, false));
        clock.store(300_000, Ordering::Relaxed);
        assert!(!d.matches_address(99, 99, false));
        assert!(d.matches_address(99, 100, false));
        assert!(d.start_address(100, false));
        d.stop();
        assert_eq!(d.address(99), 100);
    }
    #[test]
    fn ezo_configuration_sleep_address_and_compensation() {
        let (clock, state, mut d) = device();
        let mut now = 0;
        let mut run = |d: &mut SensorI2c, cmd: &[u8], delay| {
            command(d, cmd);
            now += delay;
            clock.store(now, Ordering::Relaxed);
            read(d)
        };
        assert!(state.lock().unwrap().set(76, 4.));
        assert!(state.lock().unwrap().set(0, 35.));
        assert_eq!(run(&mut d, b"rt,35", 900_000), (1, "4.000".into()));
        assert_eq!(run(&mut d, b"T,?", 300_000), (1, "?T,35.000".into()));
        assert_eq!(run(&mut d, b"I2C,100", 300_000).0, 255);
        assert_eq!(d.address(99), 100);
        assert_eq!(run(&mut d, b"T,?", 300_000), (1, "?T,25.000".into()));
        assert_eq!(run(&mut d, b"L,0", 300_000).0, 1);
        assert_eq!(run(&mut d, b"Factory", 300_000).0, 255);
        assert_eq!(d.address(99), 100);
        assert_eq!(run(&mut d, b"L,?", 300_000), (1, "?L,1".into()));
        command(&mut d, b"sleep");
        assert!(!d.start(true));
        assert_eq!(run(&mut d, b"I", 300_000), (1, "?i,pH,1.98".into()));
        assert!(state.lock().unwrap().set(76, 15.6));
        assert!(state.lock().unwrap().set(0, 25.));
        assert_eq!(run(&mut d, b"r", 900_000), (1, "14.000".into()));
        assert_eq!(run(&mut d, b"pHext,1", 300_000).0, 1);
        assert_eq!(run(&mut d, b"r", 900_000), (1, "15.600".into()));
        assert_eq!(run(&mut d, b"Name,Probe_A", 300_000).0, 1);
        assert_eq!(run(&mut d, b"Name,?", 300_000), (1, "?Name,Probe_A".into()));
        assert_eq!(run(&mut d, b"Name,bad name", 300_000).0, 2);
        assert_eq!(run(&mut d, b"T,3garbage", 300_000).0, 2);
        assert_eq!(run(&mut d, b"T,-273.15", 300_000).0, 2);
        assert_eq!(run(&mut d, b"T,2000", 300_000).0, 1);
        assert_eq!(run(&mut d, b"T,?", 300_000), (1, "?T,2000.000".into()));
        assert_eq!(run(&mut d, b"Cal,mid,7", 900_000).0, 2);
    }
}
