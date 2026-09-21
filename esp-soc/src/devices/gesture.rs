use esp_periph::i2c::I2cDevice;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug)]
pub struct GestureConfig {
    pub id: u8,
    pub sda: u8,
    pub scl: u8,
    pub irq: u8,
}
impl GestureConfig {
    pub fn valid(&self) -> bool {
        self.sda < 49
            && self.scl < 49
            && self.sda != self.scl
            && (self.irq == 255 || (self.irq < 49 && self.irq != self.sda && self.irq != self.scl))
    }
}
struct Motion {
    direction: u8,
    start: u64,
}
pub struct GestureSensor {
    pub config: GestureConfig,
    regs: [u8; 256],
    fifo: VecDeque<[u8; 4]>,
    queued: VecDeque<u8>,
    motion: Option<Motion>,
    hz: u64,
    cycle: u64,
    next: u64,
    overflow: bool,
    valid: bool,
    exit_count: u8,
    reflected_ir: f64,
    proximity_sampled: bool,
    proximity_next: u64,
    proximity_persistence: u8,
}
impl GestureSensor {
    pub fn new(config: GestureConfig, hz: u64) -> Self {
        let mut regs = [0; 256];
        regs[0x81] = 255;
        regs[0x83] = 255;
        regs[0x8d] = 0x60;
        regs[0x8e] = 0x40;
        regs[0x90] = 1;
        regs[0x92] = 0xab;
        regs[0xa6] = 0x40;
        Self {
            config,
            regs,
            fifo: VecDeque::new(),
            queued: VecDeque::new(),
            motion: None,
            hz,
            cycle: 0,
            next: 0,
            overflow: false,
            valid: false,
            exit_count: 0,
            reflected_ir: 0.,
            proximity_sampled: false,
            proximity_next: 0,
            proximity_persistence: 0,
        }
    }
    pub fn proximity_reading(&self) -> u32 { if self.proximity_sampled {self.regs[0x9c] as u32} else {u32::MAX} }
    pub fn proximity(&mut self, value: f64) -> bool {
        if !value.is_finite() || !(0.0..=255.0).contains(&value) { return false; }
        self.reflected_ir = value;
        true
    }
    fn proximity_period(&self) -> u64 {
        let width = (self.regs[0x8e] >> 6) as usize;
        let us = [40.8,44.9,53.0,69.4][width] + 796.6
            + (1 + (self.regs[0x8e] & 63)) as f64 * [28.6,36.73,53.1,85.7][width];
        let wait = if self.regs[0x80] & 8 != 0 {
            (256 - self.regs[0x83] as u64) * 2780 * if self.regs[0x8d] & 2 != 0 {12} else {1}
        } else {0};
        ((us.ceil() as u64 + wait) * self.hz / 1_000_000).max(1)
    }
    fn advance_proximity(&mut self, cycle: u64) {
        if self.regs[0x80] & 5 != 5 {
            self.proximity_next = 0;
            return;
        }
        if self.proximity_next == 0 { self.proximity_next = cycle.saturating_add(self.proximity_period()); }
        while self.proximity_next <= cycle {
            self.proximity_next = self.proximity_next.saturating_add(self.proximity_period());
            // Ideal reflected energy at 4x gain, eight 8us pulses and 100mA drive.
            let gain = [1.,2.,4.,8.][((self.regs[0x8f] >> 2) & 3) as usize];
            let drive = [1.,0.5,0.25,0.125][(self.regs[0x8f] >> 6) as usize];
            let boost = [1.,1.5,2.,3.][((self.regs[0x90] >> 4) & 3) as usize];
            let pulses = (1 + (self.regs[0x8e] & 63)) as f64;
            let width = [4.,8.,16.,32.][(self.regs[0x8e] >> 6) as usize];
            let offset = |byte:u8| if byte & 128 != 0 { -((byte & 127) as f64) } else {byte as f64};
            let value = self.reflected_ir * gain / 4. * drive * boost * pulses / 8. * width / 8.
                - (offset(self.regs[0x9d]) + offset(self.regs[0x9e])) / 2.;
            self.proximity_sampled = true;
            self.regs[0x9c] = value.round().clamp(0.,255.) as u8;
            self.regs[0x93] |= 2;
            if value > 255. { self.regs[0x93] |= 64; }
            let outside = self.regs[0x9c] < self.regs[0x89] || self.regs[0x9c] > self.regs[0x8b];
            self.proximity_persistence = if outside { self.proximity_persistence.saturating_add(1) } else {0};
            let persistence = self.regs[0x8c] >> 4;
            if persistence == 0 || self.proximity_persistence >= persistence { self.regs[0x93] |= 32; }
        }
    }
    fn enabled(&self) -> bool {
        self.regs[0x80] & 0x41 == 0x41
    }
    fn period(&self) -> u64 {
        let wait =
            [0, 2800, 5600, 8400, 14000, 22400, 30800, 39200][(self.regs[0xa3] & 7) as usize];
        let pulse =
            [4, 8, 16, 32][(self.regs[0xa6] >> 6) as usize] * (1 + (self.regs[0xa6] & 63) as u64);
        (self.hz * (wait + 4 * pulse) / 1_000_000).max(1)
    }
    pub fn gesture(&mut self, direction: u8) -> bool {
        if !(1..=4).contains(&direction) || self.queued.len() >= 32 {
            return false;
        }
        if !self.enabled()
            || (self.regs[0xab] & 1 == 0 && (self.regs[0x80] & 4 == 0 || self.regs[0xa0] > 150))
        {
            return true;
        }
        if self.motion.is_none() {
            self.motion = Some(Motion {
                direction,
                start: self.cycle,
            });
            self.regs[0xab] |= 1;
            self.next = self.cycle.saturating_add(self.period());
        } else {
            self.queued.push_back(direction);
        }
        true
    }
    fn sample(&self, cycle: u64) -> [u8; 4] {
        let Some(motion) = &self.motion else {
            return [0; 4];
        };
        let ms = cycle.saturating_sub(motion.start) * 1000 / self.hz;
        let mut sample = [80; 4];
        let sign = if ms < 120 {
            1i16
        } else if ms < 240 {
            -1
        } else {
            return [0; 4];
        };
        let (first, second) = match motion.direction {
            1 => (0, 1),
            2 => (1, 0),
            3 => (2, 3),
            _ => (3, 2),
        };
        sample[first] = (100 + sign * 70) as u8;
        sample[second] = (100 - sign * 70) as u8;
        if self.regs[0xaa] & 3 == 1 {
            sample[2] = 0;
            sample[3] = 0;
        }
        if self.regs[0xaa] & 3 == 2 {
            sample[0] = 0;
            sample[1] = 0;
        }
        sample
    }
    pub fn advance(&mut self, cycle: u64) {
        self.advance_proximity(cycle);
        self.cycle = cycle;
        if !self.enabled() || self.regs[0xab] & 1 == 0 {
            self.next = cycle.saturating_add(self.period());
            return;
        }
        if self.next == 0 {
            self.next = cycle.saturating_add(self.period());
        }
        while self.next <= cycle {
            let at = self.next;
            self.next = self.next.saturating_add(self.period());
            if self.regs[0xab] & 1 != 0 {
                let sample = self.sample(at);
                if self.fifo.len() < 32 {
                    self.fifo.push_back(sample);
                    if self.fifo.len() >= [1, 4, 8, 16][(self.regs[0xa2] >> 6) as usize] {
                        self.valid = true;
                    }
                } else {
                    self.overflow = true;
                }
                let mask = (self.regs[0xa2] >> 2) & 15;
                if self.regs[0xa1] != 0
                    && sample
                        .iter()
                        .enumerate()
                        .all(|(i, value)| mask & (8 >> i) != 0 || *value < self.regs[0xa1])
                {
                    self.exit_count = self.exit_count.saturating_add(1);
                    if self.exit_count >= [1, 1, 2, 4][(self.regs[0xa2] & 3) as usize] {
                        self.regs[0xab] &= !1;
                    }
                } else {
                    self.exit_count = 0;
                }
            }
            if self
                .motion
                .as_ref()
                .is_some_and(|motion| at.saturating_sub(motion.start) >= self.hz * 400 / 1000)
            {
                self.motion = self.queued.pop_front().map(|direction| Motion {
                    direction,
                    start: at,
                });
            }
        }
    }
    pub fn irq_high(&self) -> bool {
        (self.regs[0xab] & 2 == 0 || self.fifo.len() < [1, 4, 8, 16][(self.regs[0xa2] >> 6) as usize])
            && !(self.regs[0x80] & 32 != 0 && self.regs[0x93] & 32 != 0)
            && !(self.regs[0x90] & 128 != 0 && self.regs[0x93] & 64 != 0)
    }
    fn read(&mut self, reg: u8) -> u8 {
        match reg {
            0x92 => 0xab,
            0x93 => self.regs[0x93] | if self.valid {4} else {0},
            0x9c => {
                self.regs[0x93] &= !2;
                if self.motion.is_some() {150} else {self.regs[0x9c]}
            }
            0xae => self.fifo.len() as u8,
            0xaf => u8::from(self.valid) | (u8::from(self.overflow) << 1),
            0xfc..=0xff => {
                let value = self
                    .fifo
                    .front()
                    .map_or(0, |sample| sample[(reg - 0xfc) as usize]);
                if reg == 0xff {
                    self.fifo.pop_front();
                    if self.fifo.is_empty() {
                        self.overflow = false;
                        if self.regs[0xab] & 1 == 0 {
                            self.valid = false;
                        }
                    }
                }
                value
            }
            _ => self.regs[reg as usize],
        }
    }
    fn write(&mut self, reg: u8, value: u8) {
        match reg {
            0x92 | 0x93 | 0x9c | 0xae | 0xaf | 0xfc..=0xff => {}
            0xab => {
                self.regs[0xab] = value & 3;
                if value & 1 == 0 && self.fifo.is_empty() {
                    self.valid = false;
                }
                if value & 4 != 0 {
                    self.fifo.clear();
                    self.overflow = false;
                    self.valid = false;
                }
            }
            0x80 => {
                let previous = self.regs[reg as usize];
                self.regs[reg as usize] = value;
                if value & 5 != 5 { self.regs[0x93] &= !66; self.proximity_next = 0; }
                else if previous & 5 != 5 { self.proximity_next = self.cycle.saturating_add(self.proximity_period()); }
                if !self.enabled() {
                    self.motion = None;
                    self.queued.clear();
                }
            }
            0xe4 => self.regs[0x93] |= 32,
            0xe5 | 0xe7 => { self.regs[0x93] &= !96; self.proximity_persistence = 0; }
            0xe6 => {}
            _ => self.regs[reg as usize] = value,
        }
    }
}
pub struct GestureI2c {
    state: Arc<Mutex<GestureSensor>>,
    ptr: u8,
    first: bool,
}
impl GestureI2c {
    pub fn new(state: Arc<Mutex<GestureSensor>>) -> Self {
        Self {
            state,
            ptr: 0,
            first: true,
        }
    }
}
impl I2cDevice for GestureI2c {
    fn pins(&self) -> Option<(u8, u8)> {
        let c = self.state.lock().unwrap().config;
        Some((c.sda, c.scl))
    }
    fn start(&mut self, read: bool) -> bool {
        if !read {
            self.first = true;
        }
        true
    }
    fn write(&mut self, byte: u8) -> bool {
        if self.first {
            self.ptr = byte;
            if (0xe4..=0xe7).contains(&byte) { self.state.lock().unwrap().write(byte,0); }
            self.first = false;
        } else {
            self.state.lock().unwrap().write(self.ptr, byte);
            self.ptr = self.ptr.wrapping_add(1);
        }
        true
    }
    fn read(&mut self) -> u8 {
        let value = self.state.lock().unwrap().read(self.ptr);
        self.ptr = if self.ptr == 0xff {
            0xfc
        } else {
            self.ptr.wrapping_add(1)
        };
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sensor() -> GestureSensor {
        GestureSensor::new(
            GestureConfig {
                id: 0,
                sda: 4,
                scl: 5,
                irq: 6,
            },
            1_000_000,
        )
    }
    #[test]
    fn proximity_requires_conversion_and_obeys_gain_persistence_and_clear() {
        let mut s=sensor();
        assert_eq!(s.proximity_reading(),u32::MAX);
        assert!(s.proximity(80.)); assert!(!s.proximity(f64::NAN));
        s.write(0x8e,0x47); s.write(0x8f,8); s.write(0x80,0x25);
        s.write(0x89,10); s.write(0x8b,70); s.write(0x8c,0x20);
        s.advance(1000); assert_eq!(s.read(0x9c),0);
        s.advance(1200); assert_eq!(s.read(0x93)&2,2); assert_eq!(s.read(0x9c),80); assert_eq!(s.proximity_reading(),80);
        assert_eq!(s.read(0x93)&2,0); assert!(s.irq_high());
        s.advance(2400); assert!(!s.irq_high());
        let state=Arc::new(Mutex::new(s)); let mut wire=GestureI2c::new(state.clone());
        wire.start(false); wire.write(0xe5); assert!(state.lock().unwrap().irq_high());
        let mut s=state.lock().unwrap();
        s.write(0x8f,4); s.advance(3600); assert_eq!(s.read(0x9c),40);
        s.write(0x80,1); assert!(s.proximity(200.)); s.advance(10000); assert_eq!(s.read(0x9c),40);
        s.write(0x80,5); s.advance(10500); assert_eq!(s.read(0x9c),40);
        s.advance(12000); assert_eq!(s.read(0x9c),100);
    }

    #[test]
    fn zero_exit_threshold_keeps_sampling_and_nonzero_threshold_obeys_persistence() {
        let c = GestureConfig {
            id: 0,
            sda: 4,
            scl: 5,
            irq: 6,
        };
        let mut sensor = GestureSensor::new(c, 1_000_000);
        sensor.write(0x80, 0x41);
        sensor.write(0xab, 1);
        sensor.write(0xa6, 0xc9);
        assert!(sensor.gesture(1));
        sensor.advance(700_000);
        assert_eq!(sensor.read(0xab) & 1, 1);
        sensor.write(0xab, 5);
        sensor.write(0xa1, 10);
        sensor.write(0xa2, 3);
        sensor.advance(703_000);
        assert_eq!(sensor.read(0xab) & 1, 1);
        sensor.advance(707_000);
        assert_eq!(sensor.read(0xab) & 1, 0);
        while sensor.read(0xae) > 0 {
            for reg in 0xfc..=0xff {
                sensor.read(reg);
            }
        }
        assert_eq!(sensor.read(0xaf) & 1, 0);
    }

    #[test]
    fn fifo_is_four_byte_datasets_with_irq_threshold_and_overflow() {
        let mut s = sensor();
        s.write(0x80, 0x41);
        s.write(0xa2, 0x40);
        s.write(0xab, 3);
        s.write(0xa6, 0xc9);
        assert!(s.gesture(1));
        s.advance(6000);
        assert_eq!(s.read(0xae), 4);
        assert!(!s.irq_high());
        assert_eq!([s.read(0xfc), s.read(0xfd), s.read(0xfe)], [170, 30, 80]);
        assert_eq!(s.read(0xae), 4);
        assert_eq!(s.read(0xff), 80);
        assert_eq!(s.read(0xae), 3);
        assert!(s.irq_high());
        s.advance(100_000);
        assert_eq!(s.read(0xae), 32);
        assert_eq!(s.read(0xaf), 3);
        s.write(0xab, 7);
        assert_eq!(s.read(0xae), 0);
        assert_eq!(s.read(0xaf), 0);
    }
    #[test]
    fn physical_motion_requires_enable_and_produces_opposite_diode_lobes() {
        let mut s = sensor();
        assert!(s.gesture(1));
        s.advance(1000);
        assert_eq!(s.read(0xae), 0);
        s.write(0x80, 0x41);
        assert!(s.gesture(1));
        assert!(s.motion.is_none());
        s.write(0xab, 1);
        assert!(s.gesture(1));
        assert_eq!(s.sample(2000), [170, 30, 80, 80]);
        assert_eq!(s.sample(150000), [30, 170, 80, 80]);
        assert_eq!(s.sample(300000), [0; 4]);
        assert!(!s.gesture(5));
        s.write(0x80, 1);
        assert!(s.motion.is_none());
    }
    #[test]
    fn i2c_register_pointer_wraps_fifo_and_preserves_physical_route() {
        let mut s = sensor();
        s.fifo.push_back([1, 2, 3, 4]);
        s.fifo.push_back([5, 6, 7, 8]);
        let state = Arc::new(Mutex::new(s));
        let mut bus = GestureI2c::new(state.clone());
        assert_eq!(bus.pins(), Some((4, 5)));
        bus.start(false);
        bus.write(0xfc);
        bus.start(true);
        assert_eq!(
            (0..8).map(|_| bus.read()).collect::<Vec<_>>(),
            [1, 2, 3, 4, 5, 6, 7, 8]
        );
        assert_eq!(state.lock().unwrap().read(0xae), 0);
    }
}
