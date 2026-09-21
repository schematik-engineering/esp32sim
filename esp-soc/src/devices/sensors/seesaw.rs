use super::*;

pub(super) struct Seesaw {
    s: SampleState,
    profile: u8,
    selected: u16,
    command_pending: bool,
    response: Vec<u8>,
    write_bytes: Vec<u8>,
    pending: Option<(u64, Option<(usize, f64)>)>,
    boot_until: u64,
    position: i32,
    delta: i32,
}
impl Seesaw {
    pub fn new(clock: Arc<AtomicU64>, hz: u32, profile: u8) -> Self {
        let mut s = SampleState::new(clock, hz);
        s.inputs[18..21].copy_from_slice(&[512., 256., 768.]);
        s.inputs[75] = 500.;
        s.inputs[0] = 25.;
        Self {
            s,
            profile,
            selected: 0,
            command_pending: false,
            response: Vec::new(),
            write_bytes: Vec::new(),
            pending: None,
            boot_until: 0,
            position: 0,
            delta: 0,
        }
    }
    fn execute_command(&mut self) {
        self.sync();
        let reg = self.selected;
        let mut sample = None;
        // These are nominal response latencies within the unchanged driver's waiting windows.
        let mut delay = 100;
        match (self.profile, reg) {
            (_, 0x0001) => self.response.push(0x55),
            (_, 0x0002) => {
                let product: u32 = match self.profile {
                    42 => 3657,
                    43 => 4991,
                    _ => 4026,
                };
                self.response
                    .extend_from_slice(&(product << 16).to_be_bytes());
            }
            (_, 0x0003) => {
                let options: u32 = 3 | match self.profile {
                    42 => (1 << 8) | (1 << 9) | (1 << 11) | (1 << 14),
                    43 => (1 << 11) | (1 << 14) | (1 << 17),
                    _ => 1 << 15,
                };
                self.response.extend_from_slice(&options.to_be_bytes());
            }
            (42, 0x0907..=0x0909) => {
                let field = 18 + usize::from(reg - 0x0907);
                let value = self.s.inputs[field] as u16;
                self.response.extend_from_slice(&value.to_be_bytes());
                sample = Some((field, f64::from(value)));
                delay = 400;
            }
            (42, 0x090a..=0x090e) | (44, 0x0f11..=0x0f13) => {
                self.response.extend_from_slice(&[0, 0])
            }
            (44, 0x0f10) => {
                let value = self.s.inputs[75] as u16;
                self.response.extend_from_slice(&value.to_be_bytes());
                sample = Some((75, f64::from(value)));
                delay = 2500;
            }
            (44, 0x0004) => {
                let value = (self.s.inputs[0] * 65536.).round() as i32;
                self.response.extend_from_slice(&value.to_be_bytes());
                sample = Some((0, f64::from(value) / 65536.));
            }
            (43, 0x1130 | 0x1140) => {
                let value = if reg == 0x1130 {
                    self.position
                } else {
                    self.delta
                };
                self.response.extend_from_slice(&value.to_be_bytes());
                sample = Some((74, f64::from(self.position)));
                // The official firmware clears delta on either position or delta reads.
                self.delta = 0;
            }
            _ => {}
        }
        self.pending = Some((self.s.now + self.s.ticks(delay), sample));
    }
    fn reset(&mut self) {
        self.command_pending = false;
        self.s.time();
        self.position = 0;
        self.delta = 0;
        self.response.clear();
        self.write_bytes.clear();
        self.pending = None;
        self.s.readings = [f64::NAN; FIELD_COUNT];
        self.s.generation = 0;
        // ponytail: nominal 10 ms firmware startup; replace with measured per-image timing if needed.
        self.boot_until = self.s.now + self.s.ticks(10_000);
    }
}
impl RegisterSensor for Seesaw {
    fn format(&self) -> WireFormat {
        WireFormat::Address16
    }
    fn address_ready(&self) -> bool {
        self.s.now >= self.boot_until
    }
    fn read_ready(&self) -> bool {
        self.pending.is_none() && !self.response.is_empty()
    }
    fn select_extended(&mut self, reg: u16) {
        self.selected = reg;
        self.write_bytes.clear();
        self.response.clear();
        self.pending = None;
        self.command_pending = true;
    }
    fn stop(&mut self) {
        if self.command_pending {
            self.command_pending = false;
            self.execute_command();
        }
    }
    fn start(&mut self, read: bool) {
        if read {
            self.stop();
        }
    }
    fn read_extended(&self, reg: u16) -> u8 {
        self.response
            .get(usize::from(reg.wrapping_sub(self.selected)))
            .copied()
            .unwrap_or(0xff)
    }
    fn write_extended(&mut self, reg: u16, value: u8) -> bool {
        let index = usize::from(reg.wrapping_sub(self.selected));
        if index != self.write_bytes.len() || index >= 4 {
            return false;
        }
        self.command_pending = false;
        self.write_bytes.push(value);
        self.pending = None;
        self.response.clear();
        match self.selected {
            0x007f if index == 0 && value == 0xff => self.reset(),
            0x1130 if self.profile == 43 && index == 3 => {
                self.position = i32::from_be_bytes(self.write_bytes[..4].try_into().unwrap());
            }
            _ => {}
        }
        true
    }
    fn sync(&mut self) {
        self.s.time();
        if let Some((deadline, sample)) = self.pending {
            if self.s.now >= deadline {
                if let Some((field, value)) = sample {
                    self.s.readings[field] = value;
                    self.s.publish(1);
                }
                self.pending = None;
            }
        }
    }
    fn registers(&self) -> [u8; 256] {
        self.s.regs
    }
    fn write(&mut self, _: u8, _: u16) -> bool {
        false
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        let range = match (self.profile, field) {
            (42, 18..=20) => (0., 1023.),
            (43, 74) => (i32::MIN as f64, i32::MAX as f64),
            (44, 75) => (0., 65534.),
            (44, 0) => (-40., 85.),
            _ => return false,
        };
        if !value.is_finite()
            || value < range.0
            || value > range.1
            || (field != 0 && value.fract() != 0.)
        {
            return false;
        }
        if field == 74 {
            let movement = (value as i32).wrapping_sub(self.s.inputs[74] as i32);
            self.position = self.position.wrapping_add(movement);
            self.delta = self.delta.wrapping_add(movement);
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
    fn time(d: &mut Seesaw, us: u64) {
        d.s.clock.store(us, Ordering::Relaxed);
        d.sync();
    }
    fn read(d: &mut Seesaw, reg: u16, us: u64) -> Vec<u8> {
        d.select_extended(reg);
        d.stop();
        time(d, d.s.now + us);
        assert!(d.read_ready());
        (0..d.response.len())
            .map(|i| d.read_extended(reg + i as u16))
            .collect()
    }
    #[test]
    fn seesaw_profiles_timing_channels_and_wire_endianness() {
        for (profile, product, options) in [
            (42, 3657u32, 0x4b03u32),
            (43, 4991, 0x24803),
            (44, 4026, 0x8003),
        ] {
            let mut d = Seesaw::new(Arc::new(AtomicU64::new(0)), 1_000_000, profile);
            assert_eq!(read(&mut d, 1, 100), [0x55]);
            assert_eq!(read(&mut d, 2, 100), (product << 16).to_be_bytes());
            assert_eq!(read(&mut d, 3, 100), options.to_be_bytes());
            assert!(!d.set(74, f64::NAN));
            assert!(!d.set(18, 1024.));
            assert!(!d.set(75, 1.5));
            assert!(!d.set(74, 2147483648.));
        }
        let mut adc = Seesaw::new(Arc::new(AtomicU64::new(0)), 1_000_000, 42);
        assert!(adc.set(18, 1023.));
        adc.select_extended(0x0907);
        adc.stop();
        time(&mut adc, 399);
        assert!(!adc.read_ready());
        assert_eq!(adc.generation(), 0);
        time(&mut adc, 400);
        assert!(adc.read_ready());
        assert_eq!(adc.value(18), 1023.);
        assert_eq!(
            [adc.read_extended(0x0907), adc.read_extended(0x0908)],
            [3, 255]
        );
        assert_eq!(read(&mut adc, 0x090a, 100), [0, 0]);
        adc.select_extended(0x1130);
        adc.stop();
        time(&mut adc, 1000);
        assert!(!adc.read_ready());
        let mut soil = Seesaw::new(Arc::new(AtomicU64::new(0)), 1_000_000, 44);
        assert!(soil.set(75, 65534.));
        soil.select_extended(0x0f10);
        soil.stop();
        time(&mut soil, 2499);
        assert!(!soil.read_ready());
        time(&mut soil, 2500);
        assert!(soil.read_ready());
        assert_eq!(soil.value(75), 65534.);
        assert!(soil.set(0, -12.25));
        assert_eq!(read(&mut soil, 4, 100), (-802816i32).to_be_bytes());
        assert_eq!(read(&mut soil, 0x0f11, 100), [0, 0]);
    }
    #[test]
    fn seesaw_encoder_physical_motion_offset_delta_wrap_and_reset() {
        let mut d = Seesaw::new(Arc::new(AtomicU64::new(0)), 1_000_000, 43);
        assert!(d.set(74, -17.));
        assert_eq!(read(&mut d, 0x1140, 100), (-17i32).to_be_bytes());
        assert_eq!(read(&mut d, 0x1140, 100), [0; 4]);
        assert!(d.set(74, -10.));
        assert_eq!(read(&mut d, 0x1130, 100), (-10i32).to_be_bytes());
        assert_eq!(read(&mut d, 0x1140, 100), [0; 4]);
        assert!(d.set(74, -8.));
        d.select_extended(0x1130);
        for (i, b) in i32::MAX.to_be_bytes().into_iter().enumerate() {
            assert!(d.write_extended(0x1130 + i as u16, b));
        }
        assert_eq!(d.s.inputs[74], -8.);
        assert_eq!(read(&mut d, 0x1140, 100), 2i32.to_be_bytes());
        assert!(d.set(74, -7.));
        assert_eq!(read(&mut d, 0x1130, 100), i32::MIN.to_be_bytes());
        d.select_extended(0x007f);
        assert!(d.write_extended(0x007f, 0xff));
        assert!(!d.address_ready());
        let end = d.boot_until;
        time(&mut d, end - 1);
        assert!(!d.address_ready());
        time(&mut d, end);
        assert!(d.address_ready());
        assert_eq!(d.s.inputs[74], -7.);
        assert_eq!(read(&mut d, 0x1130, 100), [0; 4]);
        assert!(d.set(74, -9.));
        assert_eq!(read(&mut d, 0x1130, 100), (-2i32).to_be_bytes());
        assert!(d.value(18).is_nan());
    }
    #[test]
    fn seesaw_wire_commands_latch_samples_and_reject_unavailable_profiles() {
        for (model, address, field, command, us, expected) in [
            (42, 0x49, 18, 0x0907u16, 400, vec![2, 0]),
            (43, 0x36, 74, 0x1130, 100, vec![0, 0, 0, 0]),
            (44, 0x37, 75, 0x0f10, 2500, vec![1, 244]),
        ] {
            let clock = Arc::new(AtomicU64::new(0));
            let config = SensorConfig {
                id: 0,
                sda: 4,
                scl: 5,
                address,
                model,
                shunt_milliohms: 0,
            };
            assert!(config.valid());
            let state = Arc::new(Mutex::new(Sensor::new(config, clock.clone(), 1_000_000)));
            let mut wire = SensorI2c::new(state.clone());
            assert!(wire.start(false));
            for b in command.to_be_bytes() {
                assert!(wire.write(b));
            }
            wire.stop();
            clock.store(us - 1, Ordering::Relaxed);
            assert!(!wire.start(true));
            clock.store(us, Ordering::Relaxed);
            assert!(wire.start(true));
            assert!(state.lock().unwrap().set(field, 9.));
            assert_eq!(
                (0..expected.len()).map(|_| wire.read()).collect::<Vec<_>>(),
                expected
            );
            assert_eq!(wire.read(), 0xff);
            wire.stop();
            for wrong in [18, 74, 75].into_iter().filter(|f| *f != field) {
                assert!(!state.lock().unwrap().set(wrong, 1.));
            }
            let missing = if model == 43 { 0x0907u16 } else { 0x1130 };
            assert!(wire.start(false));
            for b in missing.to_be_bytes() {
                assert!(wire.write(b));
            }
            wire.stop();
            clock.store(us + 10000, Ordering::Relaxed);
            assert!(!wire.start(true));
        }
    }
}
