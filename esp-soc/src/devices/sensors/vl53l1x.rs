use super::*;
pub(super) struct Vl53l1x {
    s: SampleState,
    regs: [u8; 512],
    snapshot: [u8; 512],
    boot: Option<u64>,
    next: Option<u64>,
    ready: bool,
}
impl Vl53l1x {
    pub fn new(clock: Arc<AtomicU64>, hz: u32) -> Self {
        let mut d = Self {
            s: SampleState::new(clock, hz),
            regs: [0; 512],
            snapshot: [0; 512],
            boot: None,
            next: None,
            ready: false,
        };
        d.s.inputs[58] = 1000.;
        d.reset();
        d
    }
    fn reset(&mut self) {
        self.regs = [0; 512];
        self.regs[0] = 1;
        self.regs[1] = 0x29;
        self.regs[0x30] = 1;
        self.regs[0x46] = 0x20;
        self.regs[0x4b] = 0x0a;
        self.put16(0x5e, 0x01cc);
        // Ideal oscillator trim, used by the unmodified ULD's period conversion.
        self.put16(0xde, 36);
        self.put16(0x10f, 0xeacc);
        self.regs[0x13e] = 199;
        self.boot = Some(self.s.now + self.s.ticks(1200));
        self.next = None;
        self.ready = false;
        self.s.readings = [f64::NAN; FIELD_COUNT];
        self.s.generation = 0;
        self.interrupt();
    }
    fn put16(&mut self, r: usize, v: u16) {
        self.regs[r..r + 2].copy_from_slice(&v.to_be_bytes());
    }
    fn word(&self, r: usize) -> u16 {
        u16::from_be_bytes([self.regs[r], self.regs[r + 1]])
    }
    fn budget_us(&self) -> u64 {
        match self.word(0x5e) {
            0x001d => 15000,
            0x0051 | 0x001e => 20000,
            0x00d6 | 0x0060 => 33000,
            0x01ae | 0x00ad => 50000,
            0x03e1 | 0x02d9 => 200000,
            0x0591 | 0x048f => 500000,
            _ => 100000,
        }
    }
    fn period(&self) -> u64 {
        let raw = u32::from_be_bytes(self.regs[0x6c..0x70].try_into().unwrap());
        let us = (raw as f64 * 1000. / (self.word(0xde) as f64 * 1.075)).round() as u64;
        let budget = self.budget_us();
        self.s
            .ticks(if us == 0 {
                budget
            } else if us < budget {
                us * budget.div_ceil(us)
            } else {
                us
            })
            .max(1)
    }
    fn interrupt(&mut self) {
        let active = self.regs[0x30] & 0x10 == 0;
        self.regs[0x31] =
            (self.regs[0x31] & !1) | u8::from(if self.ready { active } else { !active });
    }
    fn capture(&mut self, n: u64) {
        let offset = ((self.word(0x1e) << 3) as i16 >> 3) as f64 / 4.;
        let distance = (self.s.inputs[58] + offset).round().clamp(0., 65535.) as u16;
        let valid = self.s.inputs[58] >= 40.
            && self.s.inputs[58]
                <= if self.regs[0x4b] == 0x14 {
                    1300.
                } else {
                    4000.
                };
        self.put16(0x96, distance);
        self.regs[0x89] = if valid { 9 } else { 4 };
        self.s.readings[58] = distance as f64;
        self.s.publish(n);
        let lo = self.word(0x74);
        let hi = self.word(0x72);
        if self.regs[0x46] & 0x20 != 0
            || (valid
                && match self.regs[0x46] & 3 {
                    0 => distance < lo,
                    1 => distance > hi,
                    2 => distance < lo || distance > hi,
                    _ => distance >= lo && distance <= hi,
                })
        {
            self.ready = true;
        }
        self.interrupt();
    }
}
impl RegisterSensor for Vl53l1x {
    fn format(&self) -> WireFormat {
        WireFormat::Address16
    }
    fn address(&self, _configured: u8) -> u8 {
        self.regs[1]
    }
    fn start(&mut self, read: bool) {
        if read {
            self.snapshot = self.regs;
        }
    }
    fn sync(&mut self) {
        self.s.time();
        if self.boot.is_some_and(|at| self.s.now >= at) {
            self.boot = None;
            self.regs[0xe5] = 1;
        }
        if let Some(at) = self.next {
            if self.s.now >= at {
                let continuous = self.regs[0x87] & 0x40 != 0;
                let period = self.period();
                let n = if continuous {
                    1 + (self.s.now - at) / period
                } else {
                    1
                };
                self.capture(n);
                self.next = if continuous {
                    Some(at + n * period)
                } else {
                    None
                };
                if !continuous {
                    self.regs[0x87] = 0;
                }
            }
        }
    }
    fn registers(&self) -> [u8; 256] {
        self.regs[..256].try_into().unwrap()
    }
    fn read_extended(&self, reg: u16) -> u8 {
        self.snapshot.get(reg as usize).copied().unwrap_or(0)
    }
    fn write(&mut self, _reg: u8, _value: u16) -> bool {
        false
    }
    fn write_extended(&mut self, reg: u16, value: u8) -> bool {
        self.sync();
        match reg {
            0 if value & 1 == 0 => {
                self.reset();
                self.regs[0] = 0;
                self.boot = None;
            }
            0 => {
                self.regs[0] = 1;
                self.boot = Some(self.s.now + self.s.ticks(1200));
            }
            1 => {
                if (8..=0x77).contains(&value) {
                    self.regs[1] = value;
                }
            }
            0x31 | 0x88..=0x10e | 0x10f..=0x110 | 0x13e => {}
            0x86 => {
                if value & 1 != 0 {
                    self.ready = false;
                    self.interrupt();
                }
            }
            0x87 => {
                self.regs[0x87] = value;
                if value & 0x50 != 0 && self.boot.is_none() {
                    self.next = Some(self.s.now + self.s.ticks(self.budget_us()));
                }
            }
            r if (r as usize) < self.regs.len() => {
                self.regs[r as usize] = value;
                if r == 0x30 {
                    self.interrupt();
                }
            }
            _ => {}
        }
        true
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        if field != 58 || !value.is_finite() || !(0.0..=4000.0).contains(&value) {
            return false;
        }
        self.sync();
        self.s.inputs[58] = value;
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
    fn wide_bus_address_timing_latched_interrupt_and_snapshot() {
        let clock = Arc::new(AtomicU64::new(0));
        let state = Arc::new(Mutex::new(Sensor::new(
            SensorConfig {
                id: 0,
                sda: 4,
                scl: 5,
                address: 0x29,
                model: 22,
                shunt_milliohms: 0,
            },
            clock.clone(),
            1_000_000,
        )));
        let mut wire = SensorI2c::new(state.clone());
        fn read(w: &mut SensorI2c, r: u16) -> u16 {
            assert!(w.start(false));
            w.write((r >> 8) as u8);
            w.write(r as u8);
            assert!(w.start(true));
            let v = u16::from_be_bytes([w.read(), w.read()]);
            w.stop();
            v
        }
        fn write(w: &mut SensorI2c, r: u16, v: u8) {
            assert!(w.start(false));
            w.write((r >> 8) as u8);
            w.write(r as u8);
            w.write(v);
            w.stop();
        }
        assert_eq!(read(&mut wire, 0x10f), 0xeacc);
        clock.store(1200, Ordering::Relaxed);
        write(&mut wire, 0x87, 0x40);
        clock.store(101199, Ordering::Relaxed);
        assert_eq!(state.lock().unwrap().generation(), 0);
        clock.store(101200, Ordering::Relaxed);
        assert_eq!(read(&mut wire, 0x96), 1000);
        assert_eq!(read(&mut wire, 0x31) >> 8 & 1, 1);
        write(&mut wire, 0x86, 1);
        assert_eq!(read(&mut wire, 0x31) >> 8 & 1, 0);
        state.lock().unwrap().set(58, 1234.);
        clock.store(201200, Ordering::Relaxed);
        assert_eq!(read(&mut wire, 0x96), 1234);
        write(&mut wire, 1, 0x30);
        assert_eq!(wire.address(0x29), 0x30);
        write(&mut wire, 0x87, 0);
        clock.store(301200, Ordering::Relaxed);
        let n = state.lock().unwrap().generation();
        clock.store(901200, Ordering::Relaxed);
        assert_eq!(state.lock().unwrap().generation(), n);
        write(&mut wire, 0, 0);
        assert_eq!(wire.address(0x30), 0x29);
        write(&mut wire, 0, 1);
        clock.store(902400, Ordering::Relaxed);
        write(&mut wire, 0x87, 0x10);
        clock.store(1002400, Ordering::Relaxed);
        assert_eq!(read(&mut wire, 0x96), 1234);
        let n = state.lock().unwrap().generation();
        clock.store(2000000, Ordering::Relaxed);
        assert_eq!(state.lock().unwrap().generation(), n);
    }
}
