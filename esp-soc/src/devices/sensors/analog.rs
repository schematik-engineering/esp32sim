use super::*;

pub(super) struct Ina260 {
    s: SampleState,
    next: Option<u64>,
}
impl Ina260 {
    pub fn new(clock: Arc<AtomicU64>, hz: u32) -> Self {
        let mut d = Self {
            s: SampleState::new(clock, hz),
            next: None,
        };
        d.s.inputs[10] = 5.;
        d.s.inputs[12] = 100.;
        d.reset();
        d
    }
    fn reset(&mut self) {
        self.s.regs = [0; 256];
        self.s.readings = [f64::NAN; FIELD_COUNT];
        self.s.generation = 0;
        self.s.put16(0, 0x6127);
        self.s.put16(252, 0x5449);
        self.s.put16(254, 0x2270);
        self.next = Some(self.s.now + self.period());
    }
    fn period(&self) -> u64 {
        let c = self.s.get16(0);
        let mode = c & 7;
        let times = [140, 204, 332, 588, 1100, 2116, 4156, 8244];
        let us = if mode & 1 != 0 {
            times[((c >> 3) & 7) as usize]
        } else {
            0
        } + if mode & 2 != 0 {
            times[((c >> 6) & 7) as usize]
        } else {
            0
        };
        self.s
            .ticks(us * [1, 4, 16, 64, 128, 256, 512, 1024][((c >> 9) & 7) as usize])
            .max(1)
    }
    fn capture(&mut self, count: u64) {
        let mode = self.s.get16(0) & 7;
        if mode & 1 != 0 {
            let raw = (self.s.inputs[12] / 1.25).round() as i32;
            self.s.put16(2, raw);
            self.s.readings[12] = raw as f64 * 1.25;
        }
        if mode & 2 != 0 {
            let raw = (self.s.inputs[10] / 0.00125).round() as i32;
            self.s.put16(4, raw);
            self.s.readings[10] = raw as f64 * 0.00125;
        }
        let power =
            ((self.s.get16(2) as i16 as f64 * 1.25 * self.s.get16(4) as f64 * 0.00125).abs() / 10.)
                .round() as i32;
        self.s.put16(6, power);
        self.s.readings[13] = power as f64 * 10.;
        self.s.put16(12, (self.s.get16(12) | 8) as i32);
        self.s.publish(count);
    }
}
impl RegisterSensor for Ina260 {
    fn format(&self) -> WireFormat {
        WireFormat::Word
    }
    fn sync(&mut self) {
        self.s.time();
        let Some(next) = self.next else { return };
        if self.s.now < next {
            return;
        }
        let continuous = self.s.get16(0) & 4 != 0;
        let period = self.period();
        let count = if continuous {
            1 + (self.s.now - next) / period
        } else {
            1
        };
        self.capture(count);
        self.next = continuous.then_some(next + count * period);
    }
    fn registers(&self) -> [u8; 256] {
        self.s.regs
    }
    fn write(&mut self, reg: u8, value: u16) -> bool {
        self.sync();
        match reg {
            0 if value & 0x8000 != 0 => self.reset(),
            0 => {
                self.s.put16(0, (value & 0x0fff | 0x6000) as i32);
                self.s.put16(12, (self.s.get16(12) & !8) as i32);
                self.next = (value & 3 != 0).then(|| self.s.now + self.period());
            }
            6 => self
                .s
                .put16(12, ((value & 0xfc03) | (self.s.get16(12) & 0x1c)) as i32),
            7 => self.s.put16(14, value as i32),
            _ => {}
        }
        true
    }
    fn read_done(&mut self, reg: u8) {
        if reg == 6 {
            self.s.put16(12, (self.s.get16(12) & !8) as i32)
        }
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        if !value.is_finite()
            || !match field {
                10 => (0.0..=36.).contains(&value),
                12 => value.abs() <= 15000.,
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

pub(super) struct Ads1115 {
    s: SampleState,
    next: Option<u64>,
    active: u16,
}
impl Ads1115 {
    pub fn new(clock: Arc<AtomicU64>, hz: u32) -> Self {
        let mut d = Self {
            s: SampleState::new(clock, hz),
            next: None,
            active: 0,
        };
        d.s.inputs[10] = 5.;
        d.s.put16(2, 0x8583);
        d.s.put16(4, 0x8000);
        d.s.put16(6, 0x7fff);
        d
    }
    fn period(&self) -> u64 {
        self.s.hz / [8, 16, 32, 64, 128, 250, 475, 860][((self.active >> 5) & 7) as usize]
    }
    fn start(&mut self) {
        self.active = self.s.get16(2);
        self.s.put16(2, (self.active & !0x8000) as i32);
        self.next = Some(self.s.now + self.period());
    }
    fn capture(&mut self, count: u64) {
        let v = &self.s.inputs[18..22];
        let mux = (self.active >> 12) & 7;
        let input = match mux {
            0 => v[0] - v[1],
            1 => v[0] - v[3],
            2 => v[1] - v[3],
            3 => v[2] - v[3],
            _ => v[(mux - 4) as usize],
        };
        let range = [6.144, 4.096, 2.048, 1.024, 0.512, 0.256, 0.256, 0.256]
            [((self.active >> 9) & 7) as usize];
        self.s.put16(
            0,
            (input / range * 32768.).round().clamp(-32768., 32767.) as i32,
        );
        self.s.readings[18..22].copy_from_slice(&self.s.inputs[18..22]);
        self.s.readings[10] = self.s.inputs[10];
        self.s.publish(count);
    }
}
impl RegisterSensor for Ads1115 {
    fn format(&self) -> WireFormat {
        WireFormat::Word
    }
    fn sync(&mut self) {
        self.s.time();
        let Some(next) = self.next else { return };
        if self.s.now < next {
            return;
        }
        let continuous = self.s.get16(2) & 0x100 == 0;
        let period = self.period();
        let count = if continuous {
            1 + (self.s.now - next) / period
        } else {
            1
        };
        self.capture(count);
        if continuous {
            self.active = self.s.get16(2);
            self.next = Some(next + count * period)
        } else {
            self.next = None;
            self.s.put16(2, (self.s.get16(2) | 0x8000) as i32)
        }
    }
    fn registers(&self) -> [u8; 256] {
        self.s.regs
    }
    fn write(&mut self, reg: u8, value: u16) -> bool {
        self.sync();
        match reg {
            1 => {
                let busy = self.next.is_some();
                self.s.put16(
                    2,
                    ((value & !0x8000) | if busy { 0 } else { 0x8000 }) as i32,
                );
                if !busy && (value & 0x100 == 0 || value & 0x8000 != 0) {
                    self.start()
                }
            }
            2 | 3 => self.s.put16(reg as usize * 2, value as i32),
            _ => {}
        }
        true
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        if !value.is_finite()
            || !match field {
                10 => {
                    (2.0..=5.5).contains(&value)
                        && self.s.inputs[18..22].iter().all(|v| *v <= value)
                }
                18..=21 => (0.0..=self.s.inputs[10]).contains(&value),
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

pub(super) struct Mcp4725 {
    s: SampleState,
    bytes: Vec<u8>,
    code: u16,
    pd: u8,
    eeprom: u16,
    eeprom_pd: u8,
    busy: Option<u64>,
    settle: Option<u64>,
}
impl Mcp4725 {
    pub fn new(clock: Arc<AtomicU64>, hz: u32) -> Self {
        let mut d = Self {
            s: SampleState::new(clock, hz),
            bytes: Vec::with_capacity(3),
            code: 2048,
            pd: 0,
            eeprom: 2048,
            eeprom_pd: 0,
            busy: None,
            settle: None,
        };
        d.s.inputs[10] = 3.3;
        d.output();
        d
    }
    fn output(&mut self) {
        self.s.readings[10] = self.s.inputs[10];
        self.s.readings[24] = self.code as f64;
        self.s.readings[25] = if self.pd == 0 {
            self.code as f64 * self.s.inputs[10] / 4096.
        } else {
            0.
        };
        self.s.publish(1)
    }
}
impl RegisterSensor for Mcp4725 {
    fn format(&self) -> WireFormat {
        WireFormat::Command
    }
    fn start(&mut self, _read: bool) {
        self.bytes.clear()
    }
    fn sync(&mut self) {
        self.s.time();
        if self.busy.is_some_and(|t| self.s.now >= t) {
            self.busy = None;
            self.eeprom = self.code;
            self.eeprom_pd = self.pd
        }
        if self.settle.is_some_and(|t| self.s.now >= t) {
            self.settle = None;
            self.output()
        }
    }
    fn registers(&self) -> [u8; 256] {
        let mut r = [0; 256];
        r[0] = if self.busy.is_none() { 0x80 } else { 0 } | (self.pd << 1);
        r[1] = (self.code >> 4) as u8;
        r[2] = (self.code << 4) as u8;
        r[3] = (self.eeprom_pd << 5) | (self.eeprom >> 8) as u8;
        r[4] = self.eeprom as u8;
        r
    }
    fn write(&mut self, _reg: u8, value: u16) -> bool {
        self.sync();
        if self.busy.is_some() {
            return true;
        }
        self.bytes.push(value as u8);
        let command = self.bytes[0];
        let length = match command >> 5 {
            0 | 1 => 2,
            2 | 3 => 3,
            _ => {
                self.bytes.clear();
                return false;
            }
        };
        if self.bytes.len() == length {
            if length == 2 {
                self.pd = (command >> 4) & 3;
                self.code = ((command as u16 & 15) << 8) | self.bytes[1] as u16
            } else {
                self.pd = (command >> 1) & 3;
                self.code = ((self.bytes[1] as u16) << 4) | (self.bytes[2] as u16 >> 4)
            }
            if command >> 5 == 3 {
                self.busy = Some(self.s.now + self.s.ticks(25000))
            }
            self.settle = Some(self.s.now + self.s.ticks(6));
            self.bytes.clear();
        }
        true
    }
    fn stop(&mut self) {
        self.bytes.clear()
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        if field != 10 || !value.is_finite() || !(2.7..=5.5).contains(&value) {
            return false;
        }
        self.sync();
        self.s.inputs[10] = value;
        self.settle = Some(self.s.now + self.s.ticks(6));
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

pub(super) struct As5600 {
    s: SampleState,
    next: u64,
}
impl As5600 {
    pub fn new(clock: Arc<AtomicU64>, hz: u32) -> Self {
        let mut d = Self {
            s: SampleState::new(clock, hz),
            next: hz as u64 / 100,
        };
        d.s.regs[0xb] = 0x20;
        d.s.regs[0x1a] = 128;
        d.s.put16(0x1b, 2048);
        d
    }
    fn period(&self) -> u64 {
        self.s
            .ticks([150, 5000, 20000, 100000][(self.s.regs[8] & 3) as usize])
            .max(1)
    }
}
impl RegisterSensor for As5600 {
    fn sync(&mut self) {
        self.s.time();
        if self.s.now < self.next {
            return;
        }
        let period = self.period();
        let count = 1 + (self.s.now - self.next) / period;
        self.next += count * period;
        let raw = ((self.s.inputs[22] % 360.) * 4096. / 360.).floor() as u16;
        let zero = self.s.get16(1) & 4095;
        let stop = self.s.get16(3) & 4095;
        let max = self.s.get16(5) & 4095;
        let range = if max != 0 {
            max
        } else if stop != 0 {
            stop.wrapping_sub(zero) & 4095
        } else {
            4096
        };
        let relative = (raw.wrapping_sub(zero) & 4095) as u32;
        let mut angle = if range < 4096 && relative > range as u32 + (4096 - range as u32) / 2 {
            0
        } else {
            (relative * 4096 / range.max(1) as u32).min(4095)
        };
        if range == 4096 && self.s.generation != 0 {
            let previous = self.s.get16(0xe);
            if previous > 2048 && angle < 10 {
                angle = 4095;
            }
            if previous < 2048 && angle > 4085 {
                angle = 0;
            }
        }
        self.s.put16(0xc, raw as i32);
        self.s.put16(0xe, angle as i32);
        self.s.readings[22] = raw as f64 * 360. / 4096.;
        self.s.readings[23] = raw as f64;
        self.s.publish(count)
    }
    fn registers(&self) -> [u8; 256] {
        self.s.regs
    }
    fn write(&mut self, reg: u8, value: u16) -> bool {
        self.sync();
        match reg {
            1 | 3 | 5 => self.s.regs[reg as usize] = value as u8 & 15,
            2 | 4 | 6 | 8 => self.s.regs[reg as usize] = value as u8,
            7 => self.s.regs[7] = value as u8 & 0x3f,
            _ => {}
        }
        true
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        if field != 22 || !value.is_finite() || !(0.0..=360.).contains(&value) {
            return false;
        }
        self.sync();
        self.s.inputs[22] = value;
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
    fn ina_signed_current_power_and_triggered_conversion() {
        let clock = Arc::new(AtomicU64::new(0));
        let mut d = Ina260::new(clock.clone(), 1_000_000);
        assert_eq!(d.s.get16(252), 0x5449);
        assert_eq!(d.s.get16(254), 0x2270);
        d.set(10, 12.);
        d.set(12, -250.);
        clock.store(2199, Ordering::Relaxed);
        assert_eq!(d.generation(), 0);
        clock.store(2200, Ordering::Relaxed);
        assert_eq!(d.generation(), 1);
        assert_eq!(d.s.get16(2) as i16, -200);
        assert_eq!(d.s.get16(4), 9600);
        assert_eq!(d.value(13), 3000.);
        d.read_done(6);
        assert_eq!(d.s.get16(12) & 8, 0);
        d.write(0, 0x6120);
        d.set(12, 750.);
        clock.store(50000, Ordering::Relaxed);
        assert_eq!(d.value(12), -250.);
        d.write(0, 0x6123);
        clock.store(52200, Ordering::Relaxed);
        assert_eq!(d.value(12), 750.);
        let n = d.generation();
        clock.store(100000, Ordering::Relaxed);
        assert_eq!(d.generation(), n);
        assert!(!d.set(13, 100.));
        assert!(!d.set(12, 15001.));
    }
    #[test]
    fn ads_mux_gain_signed_clipping_and_real_rate() {
        let clock = Arc::new(AtomicU64::new(0));
        let mut d = Ads1115::new(clock.clone(), 1_000_000);
        d.set(18, 1.21875);
        d.set(19, 3.65625);
        d.write(1, 0xc183);
        assert_eq!(d.s.get16(2) & 0x8000, 0);
        clock.store(7811, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.s.get16(0), 0);
        clock.store(7812, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.s.get16(0), 6500);
        assert_ne!(d.s.get16(2) & 0x8000, 0);
        d.write(1, 0x8183);
        clock.store(15624, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.s.get16(0) as i16, -13000);
        d.write(1, 0xc583);
        clock.store(23436, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.s.get16(0), 19500);
        d.write(1, 0xd583);
        clock.store(31248, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.s.get16(0), 32767);
        assert!(!d.set(10, 3.3));
        assert!(!d.set(18, -0.01));
    }
    #[test]
    fn dac_commands_settle_eeprom_busy_and_truncated_transaction() {
        let clock = Arc::new(AtomicU64::new(0));
        let mut d = Mcp4725::new(clock.clone(), 1_000_000);
        assert_eq!(d.value(25), 1.65);
        for b in [0x40, 0x40, 0] {
            d.write(0, b);
        }
        assert_eq!(d.value(24), 2048.);
        clock.store(6, Ordering::Relaxed);
        assert_eq!(d.value(24), 1024.);
        for b in [0x60, 0xc0, 0] {
            d.write(0, b);
        }
        assert_eq!(d.registers()[0] & 0x80, 0);
        for b in [0x40, 0, 0] {
            d.write(0, b);
        }
        assert_eq!(d.code, 3072);
        clock.store(25006, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.registers()[3..5], [12, 0]);
        assert_ne!(d.registers()[0] & 0x80, 0);
        d.write(0, 0x40);
        d.start(false);
        for b in [0, 0] {
            d.write(0, b);
        }
        assert_eq!(d.code, 0);
        for b in [0x30, 0x80] {
            d.write(0, b);
        }
        clock.store(25012, Ordering::Relaxed);
        assert_eq!(d.value(25), 0.);
        assert_eq!(d.value(24), 128.);
        assert!(!d.set(24, 4095.));
    }
    #[test]
    fn angle_sensor_preserves_raw_and_latches_at_its_sample_clock() {
        let clock = Arc::new(AtomicU64::new(0));
        let mut d = As5600::new(clock.clone(), 1_000_000);
        d.set(22, 90.);
        clock.store(10000, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.s.get16(0xc), 1024);
        assert_eq!(d.s.get16(0xe), 1024);
        assert_eq!(d.s.regs[0xb], 0x20);
        d.write(1, 2);
        d.write(2, 0);
        clock.store(10300, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.s.get16(0xc), 1024);
        assert_eq!(d.s.get16(0xe), 512);
        d.set(22, 270.);
        clock.store(10600, Ordering::Relaxed);
        assert_eq!(d.value(23), 3072.);
        assert!(!d.set(23, 0.));
        assert!(!d.set(22, 361.));
    }
}
