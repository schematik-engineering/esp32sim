use super::*;

const RATES: [u64; 8] = [50, 100, 200, 400, 800, 1000, 1600, 3200];
#[derive(Clone, Copy, Default)]
struct Frame {
    bytes: [u8; 12],
    len: usize,
}
pub(super) struct Max30105 {
    s: SampleState,
    fifo: [Frame; 32],
    full: bool,
    latch: Frame,
    byte: usize,
    next: Option<u64>,
    temperature_ready: Option<u64>,
    reset_ready: Option<u64>,
    sum: [u64; 4],
    averaged: u8,
}
impl Max30105 {
    pub fn new(clock: Arc<AtomicU64>, hz: u32) -> Self {
        let mut s = SampleState::new(clock, hz);
        s.inputs[0] = 25.;
        s.inputs[83..86].copy_from_slice(&[1000., 2000., 500.]);
        let mut d = Self {
            s,
            fifo: [Frame::default(); 32],
            full: false,
            latch: Frame::default(),
            byte: 0,
            next: None,
            temperature_ready: None,
            reset_ready: None,
            sum: [0; 4],
            averaged: 0,
        };
        d.reset();
        d.s.regs[0] = 1;
        d
    }
    fn reset(&mut self) {
        self.s.regs.fill(0);
        self.s.regs[0x13..=0x17].fill(255);
        self.s.regs[0xfe] = 1;
        self.s.regs[0xff] = 0x15;
        self.s.readings.fill(f64::NAN);
        self.fifo.fill(Frame::default());
        self.full = false;
        self.byte = 0;
        self.next = None;
        self.temperature_ready = None;
        self.reset_ready = None;
        self.sum = [0; 4];
        self.averaged = 0;
    }
    fn slots(&self) -> Vec<u8> {
        match self.s.regs[9] & 7 {
            2 => vec![1],
            3 => vec![1, 2],
            7 => [
                self.s.regs[0x11] & 7,
                (self.s.regs[0x11] >> 4) & 7,
                self.s.regs[0x12] & 7,
                (self.s.regs[0x12] >> 4) & 7,
            ]
            .into_iter()
            .take_while(|s| *s != 0)
            .collect(),
            _ => vec![],
        }
    }
    fn active(&self) -> bool {
        self.s.regs[9] & 0xc0 == 0 && !self.slots().is_empty()
    }
    fn period(&self) -> u64 {
        self.s
            .hz
            .div_ceil(RATES[((self.s.regs[10] >> 2) & 7) as usize])
    }
    fn range(&self) -> f64 {
        (2048u32 << ((self.s.regs[10] >> 5) & 3)) as f64
    }
    fn led_current(code: u8) -> f64 {
        // Nominal anchors from MAX30105 Rev 1 table8. Trim and optical coupling vary per board.
        let anchors = [
            (0, 0.),
            (1, 0.2),
            (2, 0.4),
            (15, 3.1),
            (31, 6.4),
            (63, 12.5),
            (127, 25.4),
            (255, 50.),
        ];
        for pair in anchors.windows(2) {
            let [(a, x), (b, y)] = [pair[0], pair[1]];
            if code <= b {
                return x + (y - x) * (code - a) as f64 / (b - a) as f64;
            }
        }
        50.
    }
    fn raw(&self, slot: u8) -> u32 {
        let channel = (slot & 3) as usize;
        if channel == 0 {
            return 0;
        }
        let pa = if slot & 4 != 0 {
            self.s.regs[0x10]
        } else {
            self.s.regs[0x0b + channel]
        };
        // ponytail: ideal linear reflected light; measured coupling/noise requires an optical fixture.
        let current = self.s.inputs[82 + channel] * Self::led_current(pa) / 50.;
        let shift = 3 - (self.s.regs[10] & 3);
        let levels = 1u32 << (18 - shift);
        ((current / self.range() * levels as f64)
            .floor()
            .clamp(0., (levels - 1) as f64) as u32)
            << shift
    }
    fn sample(&mut self) {
        let slots = self.slots();
        for (i, slot) in slots.iter().enumerate() {
            self.sum[i] += self.raw(*slot) as u64;
        }
        self.averaged += 1;
        let average = 1u8 << ((self.s.regs[8] >> 5).min(5));
        if self.averaged < average {
            return;
        }
        let mut frame = Frame {
            len: slots.len() * 3,
            ..Frame::default()
        };
        let mut values = [f64::NAN; 3];
        for (i, slot) in slots.iter().enumerate() {
            let shift = 3 - (self.s.regs[10] & 3);
            let raw = (self.sum[i] / u64::from(average)) as u32 & !((1 << shift) - 1);
            frame.bytes[i * 3..i * 3 + 3].copy_from_slice(&raw.to_be_bytes()[1..]);
            let ch = (*slot & 3) as usize;
            if ch != 0 {
                values[ch - 1] = raw as f64 * self.range() / 262144.;
            }
        }
        self.averaged = 0;
        self.sum = [0; 4];
        if self.full && self.s.regs[8] & 0x10 == 0 {
            self.s.regs[5] = (self.s.regs[5] + 1).min(15);
            return;
        }
        self.fifo[self.s.regs[4] as usize] = frame;
        self.s.regs[4] = (self.s.regs[4] + 1) & 31;
        self.full = self.s.regs[4] == self.s.regs[6];
        let count = if self.full {
            32
        } else {
            self.s.regs[4].wrapping_sub(self.s.regs[6]) & 31
        };
        self.s.regs[0] |= 0x40;
        if count >= 32 - (self.s.regs[8] & 15) {
            self.s.regs[0] |= 0x80;
        }
        self.s.readings[83..86].copy_from_slice(&values);
        self.s.publish(1);
    }
    fn configure(&mut self) {
        let mode = self.s.regs[9] & 7;
        let pw = (self.s.regs[10] & 3) as usize;
        let max_rate = match mode {
            2 => [7, 6, 6, 5][pw],
            3 => [6, 5, 4, 3][pw],
            _ => 7,
        };
        let requested = (self.s.regs[10] >> 2) & 7;
        self.s.regs[10] = (self.s.regs[10] & !0x1c) | (requested.min(max_rate) << 2);
        self.averaged = 0;
        self.sum = [0; 4];
        self.next = self.active().then(|| self.s.now + self.period());
    }
}
impl RegisterSensor for Max30105 {
    fn sync(&mut self) {
        self.s.time();
        if self.reset_ready.is_some_and(|at| self.s.now >= at) {
            self.reset_ready = None;
            self.s.regs[9] = 0;
        }
        if self.temperature_ready.is_some_and(|at| self.s.now >= at) {
            let temp = (self.s.inputs[0] * 16.).round() as i16;
            self.s.regs[0x1f] = temp.div_euclid(16) as i8 as u8;
            self.s.regs[0x20] = temp.rem_euclid(16) as u8;
            self.s.regs[0x21] = 0;
            self.s.regs[1] |= 2;
            self.s.readings[0] = temp as f64 / 16.;
            self.s.publish(1);
            self.temperature_ready = None;
        }
        if let Some(mut at) = self.next {
            while at <= self.s.now {
                self.sample();
                at += self.period();
            }
            self.next = Some(at);
        }
    }
    fn registers(&self) -> [u8; 256] {
        self.s.regs
    }
    fn start(&mut self, read: bool) {
        if read {
            self.byte = 0;
        }
    }
    fn next_address(&self, reg: u8) -> u8 {
        if matches!(reg, 7 | 255) {
            reg
        } else {
            reg + 1
        }
    }
    fn read_live(&mut self, reg: u8) -> Option<u8> {
        if reg != 7 {
            return None;
        }
        self.sync();
        if self.byte == 0 {
            self.latch = self.fifo[self.s.regs[6] as usize];
            if self.latch.len == 0 {
                return Some(0);
            }
            self.s.regs[6] = (self.s.regs[6] + 1) & 31;
            self.full = false;
            self.s.regs[5] = 0;
        }
        let result = self.latch.bytes[self.byte];
        self.byte = (self.byte + 1) % self.latch.len;
        Some(result)
    }
    fn read_done(&mut self, reg: u8) {
        match reg {
            0 => self.s.regs[0] = 0,
            1 | 0x20 => self.s.regs[1] = 0,
            7 => self.s.regs[0] = 0,
            _ => (),
        }
    }
    fn write(&mut self, reg: u8, value: u16) -> bool {
        self.sync();
        let value = value as u8;
        match reg {
            2 => self.s.regs[2] = value & 0xe0,
            3 => self.s.regs[3] = value & 2,
            4 | 6 => {
                self.s.regs[reg as usize] = value & 31;
                self.full = false;
                self.byte = 0;
            }
            5 => self.s.regs[5] = value & 31,
            8 => {
                self.s.regs[8] = value;
                self.configure();
            }
            9 => {
                if value & 0x40 != 0 {
                    self.reset();
                    self.s.regs[9] = 0x40;
                    // Reset latency is nominal; the datasheet specifies polling the self-clearing bit.
                    self.reset_ready = Some(self.s.now + self.s.ticks(1000));
                } else {
                    self.s.regs[9] = value & 0x87;
                    if value & 0x80 != 0 {
                        self.s.regs[0] = 0;
                        self.s.regs[1] = 0;
                        self.temperature_ready = None;
                        self.s.regs[0x21] = 0;
                    }
                    self.configure();
                }
            }
            10 => {
                self.s.regs[10] = value & 0x7f;
                self.configure();
            }
            0x0c..=0x0e | 0x10 => self.s.regs[reg as usize] = value,
            0x11 | 0x12 => {
                self.s.regs[reg as usize] = value & 0x77;
                self.configure();
            }
            0x21 => {
                if value & 1 != 0 && self.active() {
                    self.s.regs[0x21] = 1;
                    self.temperature_ready = Some(self.s.now + self.s.ticks(29000));
                }
            }
            _ => return false,
        }
        true
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        if !value.is_finite()
            || !match field {
                0 => (-40. ..=85.).contains(&value),
                83..=85 => (0. ..=16384.).contains(&value),
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
    fn device() -> (Arc<AtomicU64>, Arc<Mutex<Sensor>>, SensorI2c) {
        let clock = Arc::new(AtomicU64::new(0));
        let cfg = SensorConfig {
            id: 0,
            sda: 4,
            scl: 5,
            address: 0x57,
            model: 49,
            shunt_milliohms: 0,
        };
        assert!(cfg.valid());
        let state = Arc::new(Mutex::new(Sensor::new(cfg, clock.clone(), 1_000_000)));
        (clock, state.clone(), SensorI2c::new(state))
    }
    fn write(d: &mut SensorI2c, reg: u8, value: u8) {
        assert!(d.start(false));
        assert!(d.write(reg));
        assert!(d.write(value));
        d.stop();
    }
    fn select(d: &mut SensorI2c, reg: u8) {
        assert!(d.start(false));
        assert!(d.write(reg));
        d.stop();
        assert!(d.start(true));
    }
    fn read(d: &mut SensorI2c, reg: u8) -> u8 {
        select(d, reg);
        let v = d.read();
        d.stop();
        v
    }
    fn init(d: &mut SensorI2c) {
        for (r, v) in [
            (8, 0x10),
            (9, 7),
            (10, 0x27),
            (12, 255),
            (13, 255),
            (14, 255),
            (0x11, 0x21),
            (0x12, 3),
        ] {
            write(d, r, v);
        }
    }
    fn sample(d: &mut SensorI2c) -> [u32; 3] {
        select(d, 7);
        let mut out = [0; 3];
        for v in &mut out {
            for _ in 0..3 {
                *v = (*v << 8) | d.read() as u32;
            }
        }
        d.stop();
        out
    }
    #[test]
    fn max30105_fifo_partial_reads_and_snapshot_registers() {
        let (clock, state, mut d) = device();
        init(&mut d);
        assert_eq!(read(&mut d, 255), 0x15);
        clock.store(9999, Ordering::Relaxed);
        assert_eq!(read(&mut d, 4), 0);
        clock.store(10000, Ordering::Relaxed);
        assert_eq!(read(&mut d, 4), 1);
        assert_eq!(sample(&mut d), [64000, 128000, 32000]);
        assert_eq!(read(&mut d, 6), 1);
        clock.store(30000, Ordering::Relaxed);
        select(&mut d, 7);
        assert_eq!(d.read(), 0);
        assert_eq!(state.lock().unwrap().device.registers()[6], 2);
        clock.store(40000, Ordering::Relaxed);
        assert_eq!(d.read(), 250);
        assert_eq!(
            state.lock().unwrap().device.registers()[4],
            4,
            "FIFO producer advances during a burst"
        );
        d.stop();
        assert_eq!(sample(&mut d), [64000, 128000, 32000]);
        assert_eq!(read(&mut d, 6), 3);
        write(&mut d, 6, 2);
        assert_eq!(sample(&mut d), [64000, 128000, 32000]);
        select(&mut d, 12);
        assert_eq!(d.read(), 255);
        state.lock().unwrap().device.write(13, 0x1f);
        assert_eq!(d.read(), 255, "non-FIFO reads retain transaction snapshot");
        d.stop();
        assert_eq!(read(&mut d, 13), 0x1f);
    }
    #[test]
    fn max30105_adc_averaging_leds_modes_and_overflow() {
        let (clock, state, mut d) = device();
        init(&mut d);
        write(&mut d, 8, 0x20);
        clock.store(10000, Ordering::Relaxed);
        assert_eq!(read(&mut d, 4), 0);
        state.lock().unwrap().set(83, 2000.);
        clock.store(20000, Ordering::Relaxed);
        assert_eq!(sample(&mut d), [96000, 128000, 32000]);
        write(&mut d, 8, 0);
        write(&mut d, 12, 0);
        write(&mut d, 13, 63);
        write(&mut d, 10, 0x24);
        clock.store(30000, Ordering::Relaxed);
        assert_eq!(sample(&mut d), [0, 32000, 32000]);
        state.lock().unwrap().set(85, 16384.);
        clock.store(40000, Ordering::Relaxed);
        assert_eq!(sample(&mut d)[2], 262136, "15-bit samples left justified");
        write(&mut d, 4, 0);
        write(&mut d, 6, 0);
        write(&mut d, 5, 0);
        clock.store(1040000, Ordering::Relaxed);
        assert_eq!(read(&mut d, 4), 0);
        assert_eq!(read(&mut d, 5), 15);
        assert_ne!(read(&mut d, 0) & 0x80, 0);
        select(&mut d, 7);
        d.read();
        d.stop();
        assert_eq!(read(&mut d, 5), 0);
        write(&mut d, 8, 0x10);
        clock.store(1060000, Ordering::Relaxed);
        assert_eq!(read(&mut d, 4), 2);
        assert_eq!(read(&mut d, 6), 1, "rollover does not move reader pointer");
        write(&mut d, 9, 3);
        write(&mut d, 10, 0x3f);
        assert_eq!(read(&mut d, 10), 0x2f, "two-LED 411us rate capped at400");
        assert!(!state.lock().unwrap().set(83, 16385.));
        assert!(!state.lock().unwrap().set(0, f64::NAN));
    }
    #[test]
    fn max30105_averaging_preserves_adc_alignment() {
        let (clock, state, mut d) = device();
        init(&mut d);
        write(&mut d, 10, 0x24);
        write(&mut d, 8, 0x20);
        clock.store(10000, Ordering::Relaxed);
        state.lock().unwrap().set(83, 1000.125);
        clock.store(20000, Ordering::Relaxed);
        assert_eq!(
            sample(&mut d)[0],
            64000,
            "15-bit averaged FIFO output keeps three low bits clear"
        );
    }
    #[test]
    fn max30105_temperature_shutdown_and_reset() {
        let (clock, state, mut d) = device();
        init(&mut d);
        state.lock().unwrap().set(0, -12.3125);
        write(&mut d, 0x21, 1);
        clock.store(28999, Ordering::Relaxed);
        assert_eq!(read(&mut d, 1) & 2, 0);
        clock.store(29000, Ordering::Relaxed);
        assert_eq!(read(&mut d, 0x21), 0);
        assert_eq!(read(&mut d, 1) & 2, 2);
        assert_eq!(read(&mut d, 1) & 2, 0);
        assert_eq!(read(&mut d, 0x1f), (-13i8) as u8);
        assert_eq!(read(&mut d, 0x20), 11);
        assert_eq!(state.lock().unwrap().value(0), -12.3125);
        write(&mut d, 9, 0x87);
        let ptr = read(&mut d, 4);
        state.lock().unwrap().set(0, 35.);
        write(&mut d, 0x21, 1);
        clock.store(2_000_000, Ordering::Relaxed);
        assert_eq!(read(&mut d, 4), ptr);
        assert_eq!(state.lock().unwrap().value(0), -12.3125);
        write(&mut d, 9, 7);
        clock.store(2_010_000, Ordering::Relaxed);
        assert_ne!(read(&mut d, 4), ptr);
        write(&mut d, 9, 0x40);
        assert_eq!(read(&mut d, 9), 0x40);
        clock.store(2_011_000, Ordering::Relaxed);
        assert_eq!(read(&mut d, 9), 0);
        assert_eq!(read(&mut d, 4), 0);
        assert_eq!(read(&mut d, 12), 0);
        assert_eq!(read(&mut d, 255), 0x15);
    }
}
