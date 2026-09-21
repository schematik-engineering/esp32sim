use super::*;
pub(super) fn crc8(bytes: &[u8], mut crc: u8, polynomial: u8) -> u8 {
    for &byte in bytes {
        crc ^= byte;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ polynomial
            } else {
                crc << 1
            };
        }
    }
    crc
}

pub(super) struct Sht4x {
    s: SampleState,
    serial: u32,
    next: Option<u64>,
    ready: bool,
    sample: [f64; 2],
}
impl Sht4x {
    pub fn new(clock: Arc<AtomicU64>, hz: u32, id: u8) -> Self {
        let mut s = SampleState::new(clock, hz);
        s.inputs[0] = 25.;
        s.inputs[1] = 50.;
        Self {
            s,
            serial: 0x53480000 | id as u32,
            next: None,
            ready: false,
            sample: [25., 50.],
        }
    }
    fn reply(&mut self, a: u16, b: u16) {
        let a = a.to_be_bytes();
        let b = b.to_be_bytes();
        self.s.regs[..6].copy_from_slice(&[
            a[0],
            a[1],
            crc8(&a, 0xff, 0x31),
            b[0],
            b[1],
            crc8(&b, 0xff, 0x31),
        ]);
        self.ready = true;
    }
}
impl RegisterSensor for Sht4x {
    fn format(&self) -> WireFormat {
        WireFormat::Command
    }
    fn read_ready(&self) -> bool {
        self.ready
    }
    fn sync(&mut self) {
        self.s.time();
        if self.next.is_some_and(|at| at <= self.s.now) {
            self.next = None;
            let t = ((self.sample[0] + 45.) * 65535. / 175.)
                .round()
                .clamp(0., 65535.) as u16;
            let h = ((self.sample[1] + 6.) * 65535. / 125.)
                .round()
                .clamp(0., 65535.) as u16;
            self.reply(t, h);
            self.s.readings[0] = -45. + 175. * t as f64 / 65535.;
            self.s.readings[1] = (-6. + 125. * h as f64 / 65535.).clamp(0., 100.);
            self.s.publish(1);
        }
    }
    fn registers(&self) -> [u8; 256] {
        self.s.regs
    }
    fn write(&mut self, _reg: u8, value: u16) -> bool {
        self.sync();
        let cmd = value as u8;
        if cmd == 0x94 {
            self.next = None;
            self.ready = false;
            return true;
        }
        if self.next.is_some() {
            return false;
        }
        if cmd == 0x89 {
            self.reply((self.serial >> 16) as u16, self.serial as u16);
            return true;
        }
        let us = match cmd {
            0xfd => 8300,
            0xf6 => 4500,
            0xe0 => 1600,
            _ => return false,
        };
        self.ready = false;
        self.sample = [self.s.inputs[0], self.s.inputs[1]];
        self.next = Some(self.s.now + self.s.ticks(us));
        true
    }
    fn read_done(&mut self, reg: u8) {
        if reg == 5 {
            self.ready = false;
        }
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        self.sync();
        let range = match field {
            0 => (-40., 125.),
            1 => (0., 100.),
            _ => return false,
        };
        if !value.is_finite() || !(range.0..=range.1).contains(&value) {
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

pub(super) struct Tmp117 {
    s: SampleState,
    next: Option<u64>,
}
impl Tmp117 {
    pub fn new(clock: Arc<AtomicU64>, hz: u32) -> Self {
        let mut d = Self {
            s: SampleState::new(clock, hz),
            next: None,
        };
        d.s.inputs[0] = 25.;
        d.reset();
        d
    }
    fn conversion(&self) -> u64 {
        self.s
            .ticks([15500, 125000, 500000, 1_000_000][((self.s.get16(2) >> 5) & 3) as usize])
    }
    fn period(&self) -> u64 {
        self.conversion().max(self.s.ticks(
            [
                0, 125000, 250000, 500000, 1_000_000, 4_000_000, 8_000_000, 16_000_000,
            ][((self.s.get16(2) >> 7) & 7) as usize],
        ))
    }
    fn reset(&mut self) {
        self.s.regs = [0; 256];
        self.s.put16(0, 0x8000);
        self.s.put16(2, 0x0220);
        self.s.put16(4, 0x6000);
        self.s.put16(6, 0x8000);
        self.s.put16(30, 0x0117);
        self.next = Some(self.s.now + self.s.ticks(2000) + self.conversion());
    }
    fn sample(&mut self, count: u64) {
        let offset = self.s.get16(14) as i16;
        let raw = ((self.s.inputs[0] * 128.).round() as i32 + offset as i32).clamp(-32768, 32767);
        self.s.put16(0, raw);
        self.s.readings[0] = raw as f64 / 128.;
        let mut cfg = self.s.get16(2) | 0x2000;
        cfg &= !0xc000;
        if raw > (self.s.get16(4) as i16 as i32) {
            cfg |= 0x8000;
        }
        if raw < (self.s.get16(6) as i16 as i32) {
            cfg |= 0x4000;
        }
        self.s.put16(2, cfg as i32);
        self.s.publish(count);
    }
}
impl RegisterSensor for Tmp117 {
    fn format(&self) -> WireFormat {
        WireFormat::Word
    }
    fn sync(&mut self) {
        self.s.time();
        if let Some(at) = self.next {
            if at <= self.s.now {
                let mode = (self.s.get16(2) >> 10) & 3;
                if mode == 3 {
                    self.sample(1);
                    self.s.put16(2, ((self.s.get16(2) & !0xc00) | 0x400) as i32);
                    self.next = None;
                } else {
                    let period = self.period();
                    let count = 1 + (self.s.now - at) / period;
                    self.sample(count);
                    self.next = Some(at + count * period);
                }
            }
        }
    }
    fn registers(&self) -> [u8; 256] {
        self.s.regs
    }
    fn write(&mut self, reg: u8, value: u16) -> bool {
        self.sync();
        match reg {
            1 => {
                if value & 2 != 0 {
                    self.reset();
                    return true;
                }
                let cfg = (value & 0x0ffd) | (self.s.get16(2) & 0xe000);
                self.s.put16(2, cfg as i32);
                let mode = (cfg >> 10) & 3;
                self.next = if mode == 1 {
                    None
                } else {
                    Some(self.s.now + self.conversion())
                };
                true
            }
            2 | 3 | 7 => {
                self.s.put16(reg as usize * 2, value as i32);
                true
            }
            _ => false,
        }
    }
    fn read_done(&mut self, reg: u8) {
        if reg == 0 || reg == 1 {
            self.s.put16(2, (self.s.get16(2) & !0xe000) as i32);
        }
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        self.sync();
        if field != 0 || !value.is_finite() || !(-55.0..=150.).contains(&value) {
            return false;
        }
        self.s.inputs[0] = value;
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

pub(super) struct Mlx90614 {
    s: SampleState,
    next: u64,
}
impl Mlx90614 {
    pub fn new(clock: Arc<AtomicU64>, hz: u32) -> Self {
        let mut s = SampleState::new(clock, hz);
        s.inputs[0] = 25.;
        s.inputs[16] = 32.;
        s.put16(0x24 * 2, 0xffff);
        let next = s.ticks(94000);
        Self { s, next }
    }
}
impl RegisterSensor for Mlx90614 {
    fn format(&self) -> WireFormat {
        WireFormat::SmBus
    }
    fn sync(&mut self) {
        self.s.time();
        if self.next <= self.s.now {
            let period = self.s.ticks(94000);
            let count = 1 + (self.s.now - self.next) / period;
            self.next += count * period;
            for (reg, field) in [(6, 0), (7, 16)] {
                let raw = ((self.s.inputs[field] + 273.15) / 0.02)
                    .round()
                    .clamp(0., 32767.) as i32;
                self.s.put16(reg * 2, raw);
                self.s.readings[field] = raw as f64 * 0.02 - 273.15;
            }
            self.s.publish(count);
        }
    }
    fn registers(&self) -> [u8; 256] {
        self.s.regs
    }
    fn write(&mut self, _reg: u8, _value: u16) -> bool {
        false
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        self.sync();
        let bounds = match field {
            0 => (-40., 125.),
            16 => (-70., 380.),
            _ => return false,
        };
        if !value.is_finite() || !(bounds.0..=bounds.1).contains(&value) {
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
    fn sht_crc_ready_and_snapshot_are_driven_by_conversion() {
        let clock = Arc::new(AtomicU64::new(0));
        let mut s = Sht4x::new(clock.clone(), 1_000_000, 3);
        assert!(s.set(0, -10.25));
        assert!(s.set(1, 73.5));
        assert!(s.write(0, 0xfd));
        assert!(!s.read_ready());
        clock.store(8299, Ordering::Relaxed);
        s.sync();
        assert_eq!(s.generation(), 0);
        clock.store(8300, Ordering::Relaxed);
        s.sync();
        assert!(s.read_ready());
        let r = s.registers();
        assert_eq!(crc8(&r[..2], 0xff, 0x31), r[2]);
        assert_eq!(crc8(&r[3..5], 0xff, 0x31), r[5]);
        assert!((s.value(0) + 10.25).abs() < 0.003);
        assert!((s.value(1) - 73.5).abs() < 0.003);
        s.read_done(5);
        assert!(!s.read_ready());
        assert!(!s.write(0, 0x39));
    }
    #[test]
    fn tmp_shutdown_one_shot_ready_and_offset() {
        let clock = Arc::new(AtomicU64::new(0));
        let mut s = Tmp117::new(clock.clone(), 1_000_000);
        s.write(1, 0x400);
        assert!(s.set(0, -5.25));
        clock.store(1_000_000, Ordering::Relaxed);
        s.sync();
        assert_eq!(s.generation(), 0);
        s.write(7, 128);
        s.write(1, 0xc00);
        clock.store(1_015_499, Ordering::Relaxed);
        s.sync();
        assert_eq!(s.generation(), 0);
        clock.store(1_015_500, Ordering::Relaxed);
        s.sync();
        assert_eq!(s.value(0), -4.25);
        assert_eq!(s.s.get16(2) & 0x2c00, 0x2400);
        s.read_done(0);
        assert_eq!(s.s.get16(2) & 0x2000, 0);
        clock.store(10_000_000, Ordering::Relaxed);
        s.sync();
        assert_eq!(s.generation(), 1);
    }
    #[test]
    fn mlx_words_are_signed_kelvin_scaled_and_pec_matches_datasheet() {
        assert_eq!(crc8(&[0xb4, 0x07, 0xb5, 0xd2, 0x3a], 0, 7), 0x30);
        let clock = Arc::new(AtomicU64::new(0));
        let mut s = Mlx90614::new(clock.clone(), 1_000_000);
        assert!(s.set(0, -5.));
        assert!(s.set(16, 88.5));
        clock.store(93999, Ordering::Relaxed);
        assert_eq!(s.generation(), 0);
        clock.store(94000, Ordering::Relaxed);
        assert_eq!(s.generation(), 1);
        assert!((s.value(16) - 88.5).abs() < 0.011);
        assert!((s.value(0) + 5.).abs() < 0.011);
        assert!(!s.set(16, 381.));
        assert!(!s.write(0x24, 0));
    }
    #[test]
    fn framed_i2c_crc_and_word_endianness_are_distinct() {
        let clock = Arc::new(AtomicU64::new(1_000_000));
        let config = SensorConfig {
            id: 0,
            sda: 4,
            scl: 5,
            address: 0x5a,
            model: 9,
            shunt_milliohms: 0,
        };
        let state = Arc::new(Mutex::new(Sensor::new(config, clock.clone(), 1_000_000)));
        let mut wire = SensorI2c::new(state);
        assert!(wire.start(false));
        assert!(wire.write(7));
        assert!(wire.start(true));
        let lo = wire.read();
        let hi = wire.read();
        let pec = wire.read();
        assert_eq!(pec, crc8(&[0xb4, 7, 0xb5, lo, hi], 0, 7));
        let raw = u16::from_le_bytes([lo, hi]);
        assert!((raw as f64 * 0.02 - 273.15 - 32.).abs() < 0.011);
        let config = SensorConfig {
            address: 0x48,
            model: 8,
            ..config
        };
        let state = Arc::new(Mutex::new(Sensor::new(config, clock, 1_000_000)));
        let mut wire = SensorI2c::new(state);
        assert!(wire.start(false));
        assert!(wire.write(0x0f));
        assert!(wire.start(true));
        assert_eq!([wire.read(), wire.read()], [1, 0x17]);
    }
}
