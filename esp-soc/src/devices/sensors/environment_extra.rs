use super::*;

pub(super) struct Mcp9600 {
    s: SampleState,
    next: Option<u64>,
    burst: u16,
}
impl Mcp9600 {
    pub fn new(clock: Arc<AtomicU64>, hz: u32) -> Self {
        let mut d = Self {
            s: SampleState::new(clock, hz),
            next: None,
            burst: 0,
        };
        d.s.inputs[0] = 100.;
        d.s.inputs[46] = 25.;
        d.s.put16(0x80, 0x4010);
        d.next = Some(d.period());
        d
    }
    fn period(&self) -> u64 {
        self.s.ticks(
            [320000, 80000, 20000, 5000][((self.s.regs[24] >> 5) & 3) as usize]
                + if self.s.regs[24] & 0x80 != 0 {
                    16000
                } else {
                    63000
                },
        )
    }
    fn capture(&mut self, count: u64) {
        let hot = (self.s.inputs[0] * 16.).round() as i32;
        let step = if self.s.regs[24] & 0x80 != 0 {
            0.25
        } else {
            0.0625
        };
        let cold = (self.s.inputs[46] / step).round() * step;
        self.s.put16(0, hot);
        self.s.put16(4, hot - (cold * 16.) as i32);
        self.s.put16(8, (cold * 16.) as i32);
        self.s.readings[0] = hot as f64 / 16.;
        self.s.readings[46] = cold;
        self.s.regs[16] |= 0x40;
        self.s.publish(count)
    }
}
impl RegisterSensor for Mcp9600 {
    fn format(&self) -> WireFormat {
        WireFormat::Block
    }
    fn register_width(&self, reg: u8) -> u8 {
        match reg {
            0..=2 | 0x10..=0x13 | 0x20 => 2,
            3 => 3,
            _ => 1,
        }
    }
    fn sync(&mut self) {
        self.s.time();
        let Some(next) = self.next else { return };
        if self.s.now < next {
            return;
        }
        let period = self.period();
        let count = 1 + (self.s.now - next) / period;
        let mode = self.s.regs[24] & 3;
        if mode == 2 {
            let n = count.min(self.burst as u64);
            self.capture(n);
            self.burst -= n as u16;
            if self.burst == 0 {
                self.s.regs[16] |= 0x80;
                self.next = None
            } else {
                self.next = Some(next + n * period)
            }
        } else {
            self.capture(count);
            self.next = Some(next + count * period)
        }
    }
    fn registers(&self) -> [u8; 256] {
        self.s.regs
    }
    fn write(&mut self, reg: u8, value: u16) -> bool {
        self.sync();
        match reg {
            4 => self.s.regs[16] &= !(value as u8 & 0xc0),
            5 => self.s.regs[20] = value as u8 & 0x77,
            6 => {
                self.s.regs[24] = value as u8;
                self.burst = 1 << ((value >> 2) & 7);
                self.next = if value & 3 == 1 {
                    None
                } else {
                    Some(self.s.now + self.period())
                };
            }
            8..=15 => self.s.regs[reg as usize * 4] = value as u8,
            0x10..=0x13 => self.s.put16(reg as usize * 4, (value & !3) as i32),
            _ => {}
        }
        true
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        if !value.is_finite()
            || !match field {
                0 => (-200.0..=1372.).contains(&value),
                46 => (-40.0..=125.).contains(&value),
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

pub(super) struct Opt3001 {
    s: SampleState,
    next: Option<u64>,
}
impl Opt3001 {
    pub fn new(clock: Arc<AtomicU64>, hz: u32) -> Self {
        let mut d = Self {
            s: SampleState::new(clock, hz),
            next: None,
        };
        d.s.inputs[3] = 100.;
        d.s.put16(2, 0xc810);
        d.s.put16(6, 0xbfff);
        d.s.put16(252, 0x5449);
        d.s.put16(254, 0x3001);
        d
    }
    fn period(&self) -> u64 {
        self.s.ticks(if self.s.get16(2) & 0x800 != 0 {
            800000
        } else {
            100000
        })
    }
    fn capture(&mut self, count: u64) {
        let config = self.s.get16(2);
        let mut range = (config >> 12).min(12);
        if range == 12 {
            range = 0;
            while range < 11 && self.s.inputs[3] > 40.95 * (1u32 << range) as f64 {
                range += 1
            }
        }
        let lsb = 0.01 * (1u32 << range) as f64;
        let over = self.s.inputs[3] > 4095. * lsb;
        let raw = (self.s.inputs[3] / lsb).round().min(4095.) as u16;
        self.s.put16(0, ((range << 12) | raw) as i32);
        self.s.put16(
            2,
            ((config & !0x180) | 0x80 | if over { 0x100 } else { 0 }) as i32,
        );
        self.s.readings[3] = raw as f64 * lsb;
        self.s.publish(count)
    }
}
impl RegisterSensor for Opt3001 {
    fn format(&self) -> WireFormat {
        WireFormat::Word
    }
    fn sync(&mut self) {
        self.s.time();
        let Some(next) = self.next else { return };
        if self.s.now < next {
            return;
        }
        let mode = (self.s.get16(2) >> 9) & 3;
        let count = if mode >= 2 {
            1 + (self.s.now - next) / self.period()
        } else {
            1
        };
        self.capture(count);
        if mode >= 2 {
            self.next = Some(next + count * self.period())
        } else {
            self.next = None;
            self.s.put16(2, (self.s.get16(2) & !0x600) as i32)
        }
    }
    fn registers(&self) -> [u8; 256] {
        self.s.regs
    }
    fn write(&mut self, reg: u8, value: u16) -> bool {
        self.sync();
        match reg {
            1 => {
                if value >> 12 > 12 {
                    return false;
                }
                self.s
                    .put16(2, ((value & !0x1e0) | (self.s.get16(2) & 0x160)) as i32);
                self.next = if value & 0x600 == 0 {
                    None
                } else {
                    Some(self.s.now + self.period())
                }
            }
            2 | 3 => self.s.put16(reg as usize * 2, value as i32),
            _ => {}
        }
        true
    }
    fn read_done(&mut self, reg: u8) {
        if reg == 1 {
            self.s.put16(2, (self.s.get16(2) & !0x80) as i32)
        }
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        if field != 3 || !value.is_finite() || !(0.0..=83865.6).contains(&value) {
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

pub(super) struct Scd4x {
    s: SampleState,
    id: u8,
    bytes: Vec<u8>,
    next: Option<u64>,
    period: u64,
    ready: bool,
    response: bool,
    response_at: u64,
    measurement: bool,
    busy: u64,
    sleep: bool,
    offset: u16,
    altitude: u16,
    pressure: u16,
    asc: u16,
}
impl Scd4x {
    pub fn new(clock: Arc<AtomicU64>, hz: u32, id: u8) -> Self {
        let mut d = Self {
            s: SampleState::new(clock, hz),
            id,
            bytes: Vec::with_capacity(5),
            next: None,
            period: 0,
            ready: false,
            response: false,
            response_at: 0,
            measurement: false,
            busy: 0,
            sleep: false,
            offset: 1498,
            altitude: 0,
            pressure: 0,
            asc: 1,
        };
        d.s.inputs[0] = 25.;
        d.s.inputs[1] = 50.;
        d.s.inputs[47] = 400.;
        d
    }
    fn response(&mut self, words: &[u16]) {
        self.s.regs = [0xff; 256];
        for (i, v) in words.iter().enumerate() {
            let b = v.to_be_bytes();
            self.s.regs[i * 3..i * 3 + 2].copy_from_slice(&b);
            self.s.regs[i * 3 + 2] = temperature::crc8(&b, 0xff, 0x31)
        }
        self.response = true;
        self.response_at = self.s.now + self.s.ticks(1000);
        self.busy = self.response_at
    }
    fn command(&mut self, cmd: u16, arg: Option<u16>) -> bool {
        self.sync();
        if self.sleep && cmd != 0x36f6 {
            return false;
        }
        if self.s.now < self.busy {
            return false;
        }
        self.response = false;
        self.measurement = false;
        match cmd {
            0x21b1 | 0x21ac => {
                if self.next.is_some() {
                    return false;
                }
                self.period = self
                    .s
                    .ticks(if cmd == 0x21b1 { 5_000_000 } else { 30_000_000 });
                self.next = Some(self.s.now + self.period);
                self.ready = false
            }
            0x3f86 => {
                self.next = None;
                self.ready = false;
                self.busy = self.s.now + self.s.ticks(500000)
            }
            0xe4b8 => self.response(&[u16::from(self.ready)]),
            0xec05 => {
                if !self.ready {
                    return false;
                }
                let words = [
                    self.s.readings[47] as u16,
                    ((self.s.readings[0] + 45.) * 65535. / 175.).round() as u16,
                    (self.s.readings[1] * 65535. / 100.).round() as u16,
                ];
                self.response(&words);
                self.measurement = true
            }
            0x3682 if self.next.is_none() => self.response(&[0x5348, 0x4344, self.id as u16]),
            0x2318 if self.next.is_none() => self.response(&[self.offset]),
            0x2322 if self.next.is_none() => self.response(&[self.altitude]),
            0xe000 => {
                if let Some(value) = arg {
                    self.pressure = value;
                    self.busy = self.s.now + self.s.ticks(1000)
                } else {
                    self.response(&[self.pressure]);
                }
            }
            0x241d if self.next.is_none() => {
                self.offset = arg.unwrap();
                self.busy = self.s.now + self.s.ticks(1000)
            }
            0x2427 if self.next.is_none() => {
                self.altitude = arg.unwrap();
                self.busy = self.s.now + self.s.ticks(1000)
            }
            0x2416 if self.next.is_none() => {
                if arg.unwrap() > 1 {
                    return false;
                }
                self.asc = arg.unwrap();
                self.busy = self.s.now + self.s.ticks(1000)
            }
            0x2313 if self.next.is_none() => self.response(&[self.asc]),
            0x3646 if self.next.is_none() => {
                self.ready = false;
                self.busy = self.s.now + self.s.ticks(20000)
            }
            0x36e0 if self.next.is_none() => self.sleep = true,
            0x36f6 => {
                self.sleep = false;
                self.busy = self.s.now + self.s.ticks(20000)
            }
            _ => return false,
        }
        true
    }
}
impl RegisterSensor for Scd4x {
    fn format(&self) -> WireFormat {
        WireFormat::Command
    }
    fn start(&mut self, _read: bool) {
        self.bytes.clear()
    }
    fn read_ready(&self) -> bool {
        self.response && self.s.now >= self.response_at && !self.sleep
    }
    fn sync(&mut self) {
        self.s.time();
        let Some(next) = self.next else { return };
        if self.s.now < next {
            return;
        }
        let count = 1 + (self.s.now - next) / self.period;
        self.next = Some(next + count * self.period);
        self.ready = true;
        self.s.readings[0] = self.s.inputs[0] + (1498. - self.offset as f64) * 175. / 65535.;
        self.s.readings[1] = self.s.inputs[1];
        self.s.readings[47] = self.s.inputs[47].round();
        self.s.publish(count)
    }
    fn registers(&self) -> [u8; 256] {
        self.s.regs
    }
    fn write(&mut self, _reg: u8, value: u16) -> bool {
        self.bytes.push(value as u8);
        if self.bytes.len() < 2 {
            return true;
        }
        let cmd = u16::from_be_bytes([self.bytes[0], self.bytes[1]]);
        let has_arg = matches!(cmd, 0x241d | 0x2427 | 0xe000 | 0x2416);
        let length = if has_arg { 5 } else { 2 };
        if self.bytes.len() < length {
            return true;
        }
        let arg = if has_arg {
            if temperature::crc8(&self.bytes[2..4], 0xff, 0x31) != self.bytes[4] {
                self.bytes.clear();
                return false;
            }
            Some(u16::from_be_bytes([self.bytes[2], self.bytes[3]]))
        } else {
            None
        };
        self.bytes.clear();
        self.command(cmd, arg)
    }
    fn read_done(&mut self, reg: u8) {
        if self.measurement && reg == 8 {
            self.ready = false;
            self.measurement = false;
            self.response = false
        }
    }
    fn stop(&mut self) {
        if self.bytes.as_slice() == [0xe0, 0] {
            self.command(0xe000, None);
        }
        self.bytes.clear()
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        if !value.is_finite()
            || !match field {
                0 => (-10.0..=60.).contains(&value),
                1 => (0.0..=100.).contains(&value),
                47 => (0.0..=40000.).contains(&value),
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn thermocouple_mixed_register_widths_signed_values_and_shutdown() {
        let clock = Arc::new(AtomicU64::new(0));
        let sensor = Arc::new(Mutex::new(Sensor::new(
            SensorConfig {
                id: 0,
                sda: 4,
                scl: 5,
                address: 0x67,
                model: 17,
                shunt_milliohms: 0,
            },
            clock.clone(),
            1_000_000,
        )));
        let mut bus = SensorI2c::new(sensor.clone());
        bus.start(false);
        bus.write(0x20);
        bus.start(true);
        assert_eq!([bus.read(), bus.read()], [0x40, 0x10]);
        bus.stop();
        bus.start(false);
        bus.write(6);
        assert!(bus.write(0x80));
        bus.stop();
        sensor.lock().unwrap().set(0, -10.125);
        sensor.lock().unwrap().set(46, 22.18);
        clock.store(335999, Ordering::Relaxed);
        assert_eq!(sensor.lock().unwrap().generation(), 0);
        clock.store(336000, Ordering::Relaxed);
        assert_eq!(sensor.lock().unwrap().value(0), -10.125);
        assert_eq!(sensor.lock().unwrap().value(46), 22.25);
        bus.start(false);
        bus.write(0);
        bus.start(true);
        assert_eq!(i16::from_be_bytes([bus.read(), bus.read()]), -162);
        bus.stop();
        bus.start(false);
        bus.write(6);
        bus.write(0x81);
        bus.stop();
        sensor.lock().unwrap().set(0, 120.);
        clock.store(1000000, Ordering::Relaxed);
        assert_eq!(sensor.lock().unwrap().value(0), -10.125);
    }
    #[test]
    fn optical_shutdown_then_real_conversion_range_and_single_shot() {
        let clock = Arc::new(AtomicU64::new(0));
        let mut d = Opt3001::new(clock.clone(), 1_000_000);
        d.set(3, 1234.5);
        clock.store(1000000, Ordering::Relaxed);
        assert_eq!(d.generation(), 0);
        assert_eq!(d.s.get16(0), 0);
        d.write(1, 0xc610);
        clock.store(1099999, Ordering::Relaxed);
        assert_eq!(d.generation(), 0);
        clock.store(1100000, Ordering::Relaxed);
        assert_eq!(d.generation(), 1);
        assert!((d.value(3) - 1234.5).abs() < 0.32);
        assert_ne!(d.s.get16(2) & 0x80, 0);
        d.read_done(1);
        assert_eq!(d.s.get16(2) & 0x80, 0);
        d.write(1, 0x0210);
        clock.store(1200000, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.s.get16(0), 4095);
        assert_ne!(d.s.get16(2) & 0x100, 0);
        assert_eq!(d.s.get16(2) & 0x600, 0);
        assert!(!d.set(3, 83866.));
    }
    fn send(d: &mut Scd4x, cmd: u16, arg: Option<u16>) -> bool {
        let mut bytes = cmd.to_be_bytes().to_vec();
        if let Some(v) = arg {
            let b = v.to_be_bytes();
            bytes.extend(b);
            bytes.push(temperature::crc8(&b, 0xff, 0x31));
        }
        d.start(false);
        let mut ok = true;
        for b in bytes {
            ok &= d.write(0, b as u16)
        }
        d.stop();
        ok
    }
    #[test]
    fn co2_physical_sample_clock_crc_ready_consumption_and_stop() {
        let clock = Arc::new(AtomicU64::new(0));
        let mut d = Scd4x::new(clock.clone(), 1_000_000, 3);
        d.set(47, 1200.);
        d.set(0, 23.25);
        d.set(1, 67.5);
        assert!(send(&mut d, 0x21b1, None));
        clock.store(4999999, Ordering::Relaxed);
        assert_eq!(d.generation(), 0);
        assert!(!send(&mut d, 0xec05, None));
        clock.store(5000000, Ordering::Relaxed);
        assert_eq!(d.generation(), 1);
        assert!(send(&mut d, 0xec05, None));
        assert!(!d.read_ready());
        assert!(!send(&mut d, 0xe4b8, None));
        clock.store(5001000, Ordering::Relaxed);
        d.sync();
        assert!(d.read_ready());
        let r = d.registers();
        assert_eq!(u16::from_be_bytes([r[0], r[1]]), 1200);
        for chunk in r[..9].chunks_exact(3) {
            assert_eq!(chunk[2], temperature::crc8(&chunk[..2], 0xff, 0x31));
        }
        d.read_done(8);
        assert!(!d.ready);
        assert!(send(&mut d, 0x3f86, None));
        clock.store(20000000, Ordering::Relaxed);
        assert_eq!(d.generation(), 1);
        assert!(send(&mut d, 0x241d, Some(0)));
        clock.store(20001000, Ordering::Relaxed);
        assert!(send(&mut d, 0x21b1, None));
        clock.store(25001000, Ordering::Relaxed);
        assert!((d.value(0) - 27.25015).abs() < 0.001);
        assert!(!d.set(47, 40001.));
        assert!(!d.set(0, -11.));
    }
    #[test]
    fn co2_rejects_bad_crc_and_requires_complete_commands() {
        let clock = Arc::new(AtomicU64::new(0));
        let mut d = Scd4x::new(clock, 1_000_000, 0);
        for b in [0x24, 0x1d, 0, 0] {
            assert!(d.write(0, b));
        }
        assert!(!d.write(0, 0));
        assert_eq!(d.offset, 1498);
        d.write(0, 0x21);
        d.stop();
        assert!(d.next.is_none());
    }
}
