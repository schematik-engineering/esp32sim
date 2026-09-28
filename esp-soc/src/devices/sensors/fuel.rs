use super::*;
pub(super) struct Max1704x {
    s: SampleState,
    model: u8,
    next: u64,
    pending: Option<(u8, u8)>,
    below: bool,
}
impl Max1704x {
    pub fn new(clock: Arc<AtomicU64>, hz: u32, model: u8) -> Self {
        let mut d = Self {
            s: SampleState::new(clock, hz),
            model,
            next: 0,
            pending: None,
            below: false,
        };
        d.s.inputs[48] = 50.;
        d.s.inputs[49] = if matches!(model, 24 | 25) { 7.4 } else { 3.7 };
        d.reset();
        d
    }
    fn modern(&self) -> bool {
        matches!(self.model, 20 | 25)
    }
    fn cells(&self) -> f64 {
        if matches!(self.model, 24 | 25) {
            2.
        } else {
            1.
        }
    }
    fn reset(&mut self) {
        self.s.regs = [0; 256];
        self.s.put16(8, if self.modern() { 0x10 } else { 0 });
        self.s.put16(0xc, 0x971c);
        self.s.put16(0xfe, 0xffff);
        if self.modern() {
            self.s.put16(0xa, 0x8030);
            self.s.put16(0x14, 0x00ff);
            self.s.put16(0x18, 0x9600);
            self.s.put16(0x1a, 0x0100);
        }
        self.s.readings = [f64::NAN; FIELD_COUNT];
        self.s.generation = 0;
        self.next = self.s.now + self.s.ticks(250000);
        self.pending = None;
        self.below = false;
    }
    fn asleep(&self) -> bool {
        self.s.get16(0xc) & 0x80 != 0 && (!self.modern() || self.s.get16(6) & 0x2000 != 0)
    }
    fn period(&self) -> u64 {
        self.s
            .ticks(if self.modern() && self.s.get16(0xa) == 0xffff {
                45_000_000
            } else {
                250_000
            })
    }
    fn capture(&mut self, count: u64) {
        let quantum = if self.modern() { 0.000078125 } else { 0.00125 } * self.cells();
        let raw = (self.s.inputs[49] / quantum)
            .round()
            .clamp(0., if self.modern() { 65535. } else { 4095. }) as i32;
        self.s.put16(2, if self.modern() { raw } else { raw << 4 });
        self.s.readings[49] = raw as f64 * quantum;
        let soc = (self.s.inputs[48] * 256.).round() as i32;
        self.s.put16(4, soc);
        self.s.readings[48] = soc as f64 / 256.;
        if self.modern() {
            let rate = (self.s.inputs[50] / 0.208).round().clamp(-32768., 32767.) as i32;
            self.s.put16(0x16, rate);
            self.s.readings[50] = rate as f64 * 0.208;
        }
        let low = self.s.readings[48] < (32 - (self.s.regs[0xd] & 31)) as f64;
        if low && !self.below {
            self.s.regs[0xd] |= 0x20;
            if self.modern() {
                self.s.regs[0x1a] |= 0x10;
            }
        }
        self.below = low;
        self.s.publish(count);
    }
}
impl RegisterSensor for Max1704x {
    fn sync(&mut self) {
        self.s.time();
        if self.asleep() {
            return;
        }
        if self.s.now >= self.next {
            let period = self.period().max(1);
            let n = 1 + (self.s.now - self.next) / period;
            self.capture(n);
            self.next += n * period;
        }
    }
    fn registers(&self) -> [u8; 256] {
        let mut regs = self.s.regs;
        if self.modern() && self.s.get16(0xa) == 0xffff && !self.asleep() {
            regs[6] |= 0x10;
        }
        regs
    }
    fn start(&mut self, _read: bool) {
        self.pending = None;
    }
    fn stop(&mut self) {
        self.pending = None;
    }
    fn write(&mut self, reg: u8, value: u16) -> bool {
        self.sync();
        if reg & 1 == 0 {
            self.pending = Some((reg, value as u8));
            return true;
        }
        let Some((address, hi)) = self.pending.take() else {
            return true;
        };
        if address != reg - 1 {
            return true;
        }
        let v = u16::from_be_bytes([hi, value as u8]);
        match address {
            0xfe if v == 0x54 => {
                self.reset();
                return self.modern();
            }
            6 => {
                self.s.put16(
                    6,
                    if self.modern() {
                        (v & 0x2000) as i32
                    } else {
                        0
                    },
                );
                if v & 0x4000 != 0 {
                    self.s.readings = [f64::NAN; FIELD_COUNT];
                    self.s.generation = 0;
                    self.next = self.s.now + self.s.ticks(250000);
                }
            }
            0xc => {
                let was = self.asleep();
                self.s.put16(0xc, v as i32);
                if was && !self.asleep() {
                    self.next = self.s.now + self.period();
                }
            }
            0xa | 0x14 if self.modern() => {
                self.s.put16(address as usize, v as i32);
                self.next = self.s.now + self.period();
            }
            0x18 if self.modern() => self.s.regs[0x18] = hi,
            0x1a if self.modern() => {
                self.s.regs[0x1a] = (self.s.regs[0x1a] & hi & 0x3f) | (hi & 0x40)
            }
            _ => {}
        }
        true
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        let range = match field {
            48 => 0.0..=100.0,
            49 => 0.0..=(5.12 * self.cells()),
            50 if self.modern() => -6800.0..=6800.0,
            _ => return false,
        };
        if !value.is_finite() || !range.contains(&value) {
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
    fn word(d: &mut Max1704x, r: u8, v: u16) {
        d.start(false);
        assert!(d.write(r, (v >> 8) as u16));
        d.write(r + 1, v & 255);
        d.stop();
    }
    #[test]
    fn variants_quantization_sleep_reset_and_atomic_register_writes() {
        for model in [20, 23, 24, 25] {
            let clock = Arc::new(AtomicU64::new(0));
            let mut d = Max1704x::new(clock.clone(), 1_000_000, model);
            d.set(49, 4.1);
            d.set(48, 62.5);
            assert_eq!(d.set(50, -10.4), d.modern());
            assert_eq!(d.generation(), 0);
            clock.store(250000, Ordering::Relaxed);
            assert_eq!(d.generation(), 1);
            assert!((d.value(49) - 4.1).abs() < 0.003);
            assert_eq!(d.value(48), 62.5);
            let old = d.s.get16(0xc);
            d.write(0xc, 0);
            d.stop();
            assert_eq!(d.s.get16(0xc), old);
            word(&mut d, 6, 0x2000);
            word(&mut d, 0xc, old | 0x80);
            d.set(49, 3.);
            clock.store(1000000, Ordering::Relaxed);
            assert!((d.value(49) - 4.1).abs() < 0.003);
            word(&mut d, 0xc, old);
            clock.store(1250000, Ordering::Relaxed);
            assert!((d.value(49) - 3.).abs() < 0.003);
            d.write(0xfe, 0);
            assert_eq!(d.write(0xff, 0x54), d.modern());
            assert_eq!(d.generation(), 0);
            clock.store(1500000, Ordering::Relaxed);
            assert_eq!(d.value(48), 62.5);
        }
    }
}
