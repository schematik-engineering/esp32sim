use super::*;

pub(super) struct Hmc5883l {
    s: SampleState,
    next: Option<u64>,
    gain: u8,
    read_mask: u8,
}
impl Hmc5883l {
    pub fn new(clock: Arc<AtomicU64>, hz: u32) -> Self {
        let mut s = SampleState::new(clock, hz);
        s.time();
        s.regs[0] = 0x10;
        s.regs[1] = 0x20;
        s.regs[2] = 1;
        s.regs[10..13].copy_from_slice(b"H43");
        let next = Some(s.now + s.ticks(56_000));
        Self {
            s,
            next,
            gain: 1,
            read_mask: 0,
        }
    }
    fn period(&self) -> u64 {
        self.s
            .ticks(
                [1_333_333, 666_667, 333_333, 133_333, 66_667, 33_333, 13_333]
                    [((self.s.regs[0] >> 2) & 7) as usize],
            )
            .max(1)
    }
    fn schedule(&mut self) {
        self.next = match self.s.regs[2] & 3 {
            0 => Some(self.s.now + 2 * self.period()),
            1 => Some(self.s.now + self.s.ticks(6000).max(1)),
            _ => None,
        };
    }
}
impl RegisterSensor for Hmc5883l {
    fn sync(&mut self) {
        self.s.time();
        let Some(at) = self.next else { return };
        if self.s.now < at {
            return;
        }
        let continuous = self.s.regs[2] & 3 == 0;
        let count = if continuous {
            1 + (self.s.now - at) / self.period()
        } else {
            1
        };
        if count > 1 {
            self.gain = self.s.regs[1] >> 5;
        }
        if self.s.regs[9] & 2 == 0 {
            let scale = [1370., 1090., 820., 660., 440., 390., 330., 230.][self.gain as usize];
            for (axis, offset) in [3, 7, 5].into_iter().enumerate() {
                let raw = (self.s.inputs[29 + axis] * scale / 100.).round() as i32;
                let raw = if (-2048..=2047).contains(&raw) {
                    raw
                } else {
                    -4096
                };
                self.s.put16(offset, raw);
                self.s.readings[29 + axis] = if raw == -4096 {
                    f64::NAN
                } else {
                    raw as f64 * 100. / scale
                };
            }
            self.s.regs[9] = 1;
            self.read_mask = 0;
            self.s.publish(count);
        }
        self.gain = self.s.regs[1] >> 5;
        self.next = if continuous {
            Some(at + count * self.period())
        } else {
            self.s.regs[2] |= 3;
            None
        };
    }
    fn next_address(&self, reg: u8) -> u8 {
        match reg {
            8 => 3,
            12 => 0,
            _ => reg.wrapping_add(1),
        }
    }
    fn registers(&self) -> [u8; 256] {
        self.s.regs
    }
    fn read_done(&mut self, reg: u8) {
        if (3..=8).contains(&reg) {
            self.read_mask |= 1 << (reg - 3);
            if self.read_mask == 63 {
                self.s.regs[9] = 0;
                self.read_mask = 0;
            } else {
                self.s.regs[9] = 2;
            }
        } else if reg == 2 {
            self.s.regs[9] |= 2;
        }
    }
    fn write(&mut self, reg: u8, value: u16) -> bool {
        self.sync();
        let value = value as u8;
        match reg {
            0 if value & 0x83 == 0 && (value >> 2) & 7 != 7 => {
                self.s.regs[0] = value;
                self.s.regs[9] &= !2;
                self.read_mask = 0;
            }
            1 if value & 31 == 0 => self.s.regs[1] = value,
            2 if value & 0x7c == 0 => {
                self.s.regs[2] = value;
                self.s.regs[9] = 0;
                self.read_mask = 0;
            }
            _ => return false,
        }
        self.schedule();
        true
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        self.sync();
        if !(29..=31).contains(&field) || !value.is_finite() || !(-810.0..=810.).contains(&value) {
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

pub(super) struct Lis3mdl {
    s: SampleState,
    next: Option<u64>,
    read_mask: u8,
}
impl Lis3mdl {
    pub fn new(clock: Arc<AtomicU64>, hz: u32) -> Self {
        let mut s = SampleState::new(clock, hz);
        s.time();
        s.inputs[0] = 25.;
        let mut d = Self {
            s,
            next: None,
            read_mask: 0,
        };
        d.reset();
        d.next = Some(d.s.now + d.startup());
        d
    }
    fn reset(&mut self) {
        self.s.regs = [0; 256];
        self.s.regs[0xf] = 0x3d;
        self.s.regs[0x20] = 0x10;
        self.s.regs[0x22] = 3;
        self.s.regs[0x30] = 0xe8;
        self.s.readings = [f64::NAN; FIELD_COUNT];
        self.next = None;
        self.read_mask = 0;
    }
    fn operating_mode(&self) -> usize {
        if self.s.regs[0x22] & 0x20 != 0 {
            0
        } else {
            ((self.s.regs[0x20] >> 5) & 3).max((self.s.regs[0x23] >> 2) & 3) as usize
        }
    }
    fn startup(&self) -> u64 {
        self.s
            .ticks([1200, 1910, 3480, 6650][self.operating_mode()])
            .max(1)
    }
    fn conversion(&self) -> u64 {
        self.s
            .ticks([900, 1650, 3230, 6400][self.operating_mode()])
            .max(1)
    }
    fn period(&self) -> u64 {
        let ctrl = self.s.regs[0x20];
        let us = if self.s.regs[0x22] & 0x20 != 0 {
            1_600_000
        } else if ctrl & 2 != 0 {
            [1000, 1786, 3333, 6452][((ctrl >> 5) & 3) as usize]
        } else {
            1_600_000 >> ((ctrl >> 2) & 7)
        };
        self.s.ticks(us).max(1)
    }
    fn put_output(&mut self, offset: usize, raw: i16) {
        let bytes = if self.s.regs[0x23] & 2 != 0 {
            raw.to_be_bytes()
        } else {
            raw.to_le_bytes()
        };
        self.s.regs[offset..offset + 2].copy_from_slice(&bytes);
    }
}
impl RegisterSensor for Lis3mdl {
    fn sync(&mut self) {
        self.s.time();
        let Some(at) = self.next else { return };
        if self.s.now < at {
            return;
        }
        let continuous = self.s.regs[0x22] & 3 == 0;
        let count = if continuous {
            1 + (self.s.now - at) / self.period()
        } else {
            1
        };
        let scale = [6842., 3421., 2281., 1711.][((self.s.regs[0x21] >> 5) & 3) as usize];
        let mut updated = false;
        for axis in 0..3 {
            let bit = 1 << axis;
            let unread = self.s.regs[0x27] & bit != 0;
            if self.s.regs[0x24] & 0x40 != 0 && matches!((self.read_mask >> (axis * 2)) & 3, 1 | 2)
            {
                continue;
            }
            let offset =
                i16::from_le_bytes([self.s.regs[5 + axis * 2], self.s.regs[6 + axis * 2]]) as f64;
            let raw = (self.s.inputs[29 + axis] * scale / 100. - offset)
                .round()
                .clamp(-32768., 32767.) as i16;
            self.put_output(0x28 + axis * 2, raw);
            self.s.readings[29 + axis] = raw as f64 * 100. / scale;
            self.s.regs[0x27] |= bit | 8;
            if unread || count > 1 {
                self.s.regs[0x27] |= (bit << 4) | 0x80;
            }
            self.read_mask &= !(3 << (axis * 2));
            updated = true;
        }
        if self.s.regs[0x20] & 0x80 != 0 {
            let raw = ((self.s.inputs[0] - 25.) * 8.).round() as i16;
            self.put_output(0x2e, raw);
            self.s.readings[0] = raw as f64 / 8. + 25.;
            updated = true;
        }
        if updated {
            self.s.publish(count);
        }
        self.next = if continuous {
            Some(at + count * self.period())
        } else {
            self.s.regs[0x22] |= 3;
            None
        };
    }
    fn registers(&self) -> [u8; 256] {
        let mut regs = self.s.regs;
        let (lo, hi) = regs.split_at_mut(128);
        hi.copy_from_slice(lo);
        regs
    }
    fn next_address(&self, reg: u8) -> u8 {
        if reg & 0x80 != 0 {
            0x80 | reg.wrapping_add(1)
        } else {
            reg
        }
    }
    fn read_done(&mut self, reg: u8) {
        let reg = reg & 0x7f;
        if (0x28..=0x2d).contains(&reg) {
            let byte = reg - 0x28;
            self.read_mask |= 1 << byte;
            let axis = byte / 2;
            let high = if self.s.regs[0x23] & 2 == 0 { 1 } else { 0 };
            if byte % 2 == high {
                self.s.regs[0x27] &= !8;
                self.s.regs[0x27] &= !((1 << axis) | (0x10 << axis));
                if self.s.regs[0x27] & 0x70 == 0 {
                    self.s.regs[0x27] &= !0x80;
                }
            }
        }
    }
    fn write(&mut self, reg: u8, value: u16) -> bool {
        self.sync();
        let old_mode = self.s.regs[0x22];
        let old_conversion = self.conversion();
        let reg = (reg & 0x7f) as usize;
        let old_value = self.s.regs[reg];
        let value = value as u8;
        match reg {
            5..=10 | 0x32 => self.s.regs[reg] = value,
            0x33 if value & 0x80 == 0 => self.s.regs[reg] = value,
            0x20 if value & 1 == 0 => self.s.regs[reg] = value,
            0x21 if value & 0x93 == 0 => {
                if value & 4 != 0 {
                    self.reset();
                    return true;
                }
                self.s.regs[reg] = value & 0x60;
            }
            0x22 if value & 0xd8 == 0 && !(value & 3 == 1 && self.s.regs[0x20] & 2 != 0) => {
                self.s.regs[reg] = value
            }
            0x23 if value & 0xf1 == 0 => self.s.regs[reg] = value,
            0x24 if value & 0xbf == 0 => self.s.regs[reg] = value,
            0x30 if value & 1 == 0 => self.s.regs[reg] = value,
            _ => return false,
        }
        if self.s.regs[0x22] & 3 >= 2 {
            self.next = None;
        } else if reg == 0x22 && (old_mode & 3 >= 2 || value & 3 == 1) {
            self.next = Some(self.s.now + self.startup());
        } else if matches!(reg, 0x20 | 0x22 | 0x23) && old_value != value {
            self.next = Some(self.s.now + old_conversion + self.conversion());
        }
        true
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        self.sync();
        let range = match field {
            29..=31 => -1600.0..=1600.,
            0 => -40.0..=85.,
            _ => return false,
        };
        if !value.is_finite() || !range.contains(&value) {
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
    fn i2c_requires_increment_bit_and_retains_physical_address_and_pins() {
        let clock = Arc::new(AtomicU64::new(0));
        let config = SensorConfig {
            id: 1,
            sda: 4,
            scl: 5,
            address: 0x1e,
            model: 35,
            shunt_milliohms: 0,
        };
        assert!(config.valid());
        assert!(!SensorConfig {
            address: 0x1d,
            ..config
        }
        .valid());
        assert!(SensorConfig {
            model: 34,
            ..config
        }
        .valid());
        assert!(!SensorConfig {
            address: 0x1c,
            model: 34,
            ..config
        }
        .valid());
        let state = Arc::new(Mutex::new(Sensor::new(config, clock.clone(), 1_000_000)));
        state.lock().unwrap().set(29, 100.);
        state.lock().unwrap().set(30, -100.);
        let mut bus = SensorI2c::new(state);
        assert_eq!(bus.pins(), Some((4, 5)));
        clock.store(1200, Ordering::Relaxed);
        assert!(bus.start(false));
        assert!(bus.write(0xa8));
        assert!(bus.start(true));
        assert_eq!(
            (0..6).map(|_| bus.read()).collect::<Vec<_>>(),
            [0xba, 0x1a, 0x46, 0xe5, 0, 0]
        );
        bus.stop();
        assert!(bus.start(false));
        assert!(bus.write(0x28));
        assert!(bus.start(true));
        assert_eq!((0..6).map(|_| bus.read()).collect::<Vec<_>>(), [0xba; 6]);
    }
    #[test]
    fn hmc_datasheet_gain_delay_lock_overflow_and_single_timing() {
        let clock = Arc::new(AtomicU64::new(0));
        let mut d = Hmc5883l::new(clock.clone(), 1_000_000);
        assert_eq!(&d.registers()[10..13], b"H43");
        assert_eq!(d.next, Some(56_000));
        assert!(d.set(29, 100.));
        assert!(d.set(30, -50.));
        assert!(d.set(31, 25.));
        assert!(d.write(2, 1));
        clock.store(5999, Ordering::Relaxed);
        assert_eq!(d.generation(), 0);
        clock.store(6000, Ordering::Relaxed);
        assert_eq!(d.generation(), 1);
        assert_eq!(d.s.get16(3), 1090);
        assert_eq!(d.s.get16(7) as i16, -545);
        assert_eq!(d.s.get16(5), 273);
        assert_eq!(d.s.regs[2] & 3, 3);
        d.read_done(3);
        assert_eq!(d.s.regs[9], 2);
        d.set(29, 200.);
        d.write(0, 0x10);
        assert_eq!(d.s.regs[9] & 2, 0);
        for r in 3..=8 {
            d.read_done(r);
        }
        assert_eq!(d.s.regs[9], 0);
        assert!(d.write(1, 0xe0));
        assert!(d.write(2, 1));
        clock.store(12000, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.s.get16(3) as i16, -4096);
        assert!(d.write(2, 1));
        clock.store(18000, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.s.get16(3), 460);
        assert!(!d.write(0, 0x11));
        assert!(!d.write(0, 0x1c));
        assert!(!d.set(29, f64::NAN));
        assert!(d.write(2, 0));
        clock.store(151333, Ordering::Relaxed);
        assert_eq!(d.generation(), 3);
        clock.store(151334, Ordering::Relaxed);
        assert_eq!(d.generation(), 4);
        d.read_done(3);
        d.set(29, -100.);
        clock.store(300000, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.s.get16(3), 460);
        for r in 4..=8 {
            d.read_done(r);
        }
        clock.store(400000, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.s.get16(3) as i16, -230);
    }
    #[test]
    fn lis_rates_scale_bdu_shutdown_reset_offsets_endianness() {
        let clock = Arc::new(AtomicU64::new(0));
        let mut d = Lis3mdl::new(clock.clone(), 1_000_000);
        assert_eq!(d.registers()[0x8f], 0x3d);
        assert_eq!(d.next_address(0x28), 0x28);
        assert_eq!(d.next_address(0xa8), 0xa9);
        assert!(d.set(29, 100.));
        assert!(d.set(30, -100.));
        assert!(d.write(0x22, 0));
        clock.store(1199, Ordering::Relaxed);
        assert_eq!(d.generation(), 0);
        clock.store(1200, Ordering::Relaxed);
        assert_eq!(d.generation(), 1);
        assert_eq!(&d.s.regs[0x28..0x2a], &6842i16.to_le_bytes());
        assert!(d.write(0x24, 0x40));
        for r in [0xa8, 0xaa, 0xac] {
            d.read_done(r);
        }
        d.set(29, 200.);
        clock.store(200000, Ordering::Relaxed);
        assert_eq!(d.generation(), 1);
        for r in 0xa8..=0xad {
            d.read_done(r);
        }
        assert_eq!(d.s.regs[0x27], 0);
        clock.store(300000, Ordering::Relaxed);
        assert_eq!(d.generation(), 2);
        assert!(d.write(0x22, 3));
        d.set(29, -100.);
        clock.store(500000, Ordering::Relaxed);
        assert_eq!(d.generation(), 2);
        assert!(d.write(0x21, 4));
        assert_eq!(d.s.regs[0x22], 3);
        assert_eq!(d.s.regs[0x24], 0);
        assert!(d.write(5, 10));
        assert!(d.write(0x21, 0x60));
        assert!(d.write(0x23, 2));
        assert!(d.write(0x22, 1));
        clock.store(600000, Ordering::Relaxed);
        d.sync();
        assert_eq!(&d.s.regs[0x28..0x2a], &(-1721i16).to_be_bytes());
        assert_eq!(d.s.regs[0x22] & 3, 3);
        assert!(!d.write(0x20, 1));
        assert!(!d.write(0x24, 0x80));
        assert!(!d.write(0x30, 1));
        assert!(!d.set(30, 1601.));
        for (ctrl, period) in [(0, 1600000), (0x1c, 12500), (2, 1000), (0x62, 6452)] {
            d.write(0x20, ctrl);
            assert_eq!(d.period(), period);
        }
        d.write(0x22, 0x20);
        assert_eq!(d.period(), 1600000);
    }
}
