use super::*;

const ALPHA: f64 = 1. / 8_388_608.;
const EMISSIVITY: f64 = 0.95;
fn fourth(celsius: f64) -> f64 {
    (celsius + 273.15).powi(4)
}
fn signed(value: f64) -> u16 {
    value.round().clamp(-32768., 32767.) as i16 as u16
}

// Nominal, synthetic trim in the vendor's EEPROM format, not a measured factory image.
fn calibration() -> [u16; 832] {
    let mut e = [0; 832];
    e[12] = 0x1901;
    e[15] = 0xbe33;
    e[32] = 0x7000;
    e[33] = 16383;
    e[48] = 10000;
    e[49] = 10000;
    e[50] = 64;
    e[51] = 0x0100;
    e[52] = 0x1111;
    e[54] = 0x0101;
    e[55] = 0x0101;
    e[56] = 0x2880;
    e[57] = 128;
    e[63] = 0x1770;
    e[64..].fill(0x0010);
    e
}

pub(super) struct Mlx90640 {
    s: SampleState,
    ee: [u16; 832],
    ram: [u16; 832],
    snapshot: [u16; 832],
    control: u16,
    active: u16,
    status: u16,
    selected: u16,
    high: Option<u8>,
    next: u64,
    page: u16,
    aux_valid: u8,
}
impl Mlx90640 {
    pub fn new(clock: Arc<AtomicU64>, hz: u32) -> Self {
        let mut s = SampleState::new(clock, hz);
        s.inputs[0] = 25.;
        s.inputs[16] = 25.;
        s.time();
        let next = s.now + s.ticks(540_000);
        Self {
            s,
            ee: calibration(),
            ram: [0; 832],
            snapshot: [0; 832],
            control: 0x1901,
            active: 0x1901,
            status: 0,
            selected: 0,
            high: None,
            next,
            page: 0,
            aux_valid: 0,
        }
    }
    fn period(&self) -> u64 {
        (2 * self.s.hz / (1 << ((self.active >> 7) & 7))).max(1)
    }
    fn sample(&mut self) {
        let scale = 2f64.powi(((self.active >> 10) & 3) as i32 - 2);
        let ambient = self.s.inputs[0];
        let ptat = 768. * scale;
        let art = signed(ptat * (262144. / (10000. + 8. * (ambient - 25.)) - 8.));
        if self.page == 0 || self.active & 1 == 0 {
            self.ram[768] = art;
            self.ram[778] = signed(10000. * scale);
            self.aux_valid |= 1;
        }
        if self.page == 1 || self.active & 1 == 0 {
            self.ram[800] = signed(ptat);
            self.ram[810] = signed(-16384. * scale);
            self.aux_valid |= 2;
        }
        let captured_ptat = self.ram[800] as i16 as f64;
        let ta = if self.aux_valid == 3 {
            (captured_ptat * 262144. / (8. * captured_ptat + self.ram[768] as i16 as f64) - 10000.)
                / 8.
                + 25.
        } else {
            f64::NAN
        };
        // Stefan-Boltzmann scene radiance, with a fixed physical emissivity and reflected environment.
        let radiance = EMISSIVITY * fourth(self.s.inputs[16])
            + (1. - EMISSIVITY) * fourth(ambient - 8.)
            - fourth(ambient);
        let raw = signed(ALPHA * radiance * scale);
        for p in 0..768 {
            let row = (p / 32) & 1;
            let pattern = if self.active & 0x1000 == 0 {
                row
            } else {
                row ^ (p & 1)
            };
            if self.active & 1 == 0 || pattern == self.page as usize {
                self.ram[p] = raw;
            }
        }
        self.s.readings[0] = ta;
        self.s.readings[16] = ((raw as i16 as f64 / scale / ALPHA + fourth(ambient)
            - (1. - EMISSIVITY) * fourth(ambient - 8.))
            / EMISSIVITY)
            .sqrt()
            .sqrt()
            - 273.15;
        self.status = (self.status & !0x11) | 8 | self.page;
        self.s.publish(1);
    }
    fn word(&self, address: u16) -> u16 {
        match address {
            0x2400..=0x273f => self.ee[(address - 0x2400) as usize],
            0x400..=0x73f => self.snapshot[(address - 0x400) as usize],
            0x8000 => self.status,
            0x800d => self.control,
            0x800e | 0x800f => 0,
            0x8010 => self.ee[15],
            _ => 0xffff,
        }
    }
    fn write_word(&mut self, address: u16, value: u16) -> bool {
        match address {
            0x8000 => {
                self.status = (self.status & 7) | (value & 0x38);
                true
            }
            0x800d => {
                self.control = value;
                true
            }
            _ => false,
        }
    }
}
impl RegisterSensor for Mlx90640 {
    fn format(&self) -> WireFormat {
        WireFormat::Address16
    }
    fn select_extended(&mut self, reg: u16) {
        self.selected = reg;
        self.high = None;
    }
    fn start(&mut self, read: bool) {
        if read {
            self.snapshot = self.ram;
        } else {
            self.high = None;
        }
    }
    fn read_extended(&self, reg: u16) -> u8 {
        let offset = reg.wrapping_sub(self.selected);
        self.word(self.selected.wrapping_add(offset / 2))
            .to_be_bytes()[(offset & 1) as usize]
    }
    fn write_extended(&mut self, reg: u16, value: u8) -> bool {
        let offset = reg.wrapping_sub(self.selected);
        let address = self.selected.wrapping_add(offset / 2);
        if !matches!(address, 0x8000 | 0x800d) {
            return false;
        }
        if let Some(high) = self.high.take() {
            self.write_word(address, u16::from_be_bytes([high, value]))
        } else {
            self.high = Some(value);
            true
        }
    }
    fn sync(&mut self) {
        self.s.time();
        if self.s.now < self.next {
            return;
        }
        // Finish the in-flight conversion before applying changed refresh/configuration.
        if self.active & 4 == 0 || self.status & 0x10 != 0 {
            self.sample();
        }
        self.active = self.control;
        self.page = if self.active & 1 == 0 {
            0
        } else if self.active & 8 != 0 {
            (self.active >> 4) & 1
        } else {
            self.page ^ 1
        };
        self.next = self.next.saturating_add(self.period());
        if self.s.now >= self.next {
            let count = (self.s.now - self.next) / self.period() + 1;
            // Only the last two subpages can remain in RAM after a long scheduling gap.
            if self.active & 9 == 1 && count > 2 {
                self.page ^= ((count - 2) & 1) as u16;
            }
            for _ in 0..count.min(2) {
                if self.active & 4 == 0 || self.status & 0x10 != 0 {
                    self.sample();
                }
                if self.active & 9 == 1 {
                    self.page ^= 1;
                }
            }
            self.next = self
                .next
                .saturating_add(count.saturating_mul(self.period()));
        }
    }
    fn registers(&self) -> [u8; 256] {
        [0; 256]
    }
    fn write(&mut self, _reg: u8, _value: u16) -> bool {
        false
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        self.sync();
        let valid = match field {
            0 => (-40.0..=85.).contains(&value),
            16 => (-40.0..=300.).contains(&value),
            _ => false,
        };
        if !value.is_finite() || !valid {
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
    fn device() -> Mlx90640 {
        Mlx90640::new(Arc::new(AtomicU64::new(0)), 1_000_000)
    }
    fn time(d: &mut Mlx90640, us: u64) {
        d.s.clock.store(us, Ordering::Relaxed);
        d.sync();
    }
    #[test]
    fn mlx90640_startup_subpages_rate_boundary_and_hold() {
        let mut d = device();
        assert!(d.value(0).is_nan());
        d.write_word(0x800d, 0x1981);
        time(&mut d, 539999);
        assert_eq!(d.status & 8, 0);
        time(&mut d, 540000);
        assert_eq!(d.status & 9, 8);
        d.write_word(0x8000, 0x30);
        time(&mut d, 789999);
        assert_eq!(d.status & 8, 0);
        time(&mut d, 790000);
        assert_eq!(d.status & 9, 9);
        d.write_word(0x800d, 0x1985);
        time(&mut d, 1040000);
        let old = d.ram;
        d.set(16, 100.);
        time(&mut d, 1290000);
        assert_eq!(d.ram, old);
        d.write_word(0x8000, 0x30);
        time(&mut d, 1540000);
        assert_ne!(d.ram, old);
        assert_eq!(d.status & 0x10, 0);
    }
    #[test]
    fn mlx90640_nominal_calibration_and_independent_radiance_oracle() {
        let mut d = device();
        assert!(d.ee[64..].iter().all(|v| *v != 0 && v & 1 == 0));
        assert_eq!(d.ee[7..10], [0, 0, 0]);
        // Decode physical sensitivity and PTAT coefficients from the EEPROM, independently of synthesis.
        let alpha = (d.ee[33] as f64 + 1.) / 2f64.powi((d.ee[32] >> 12) as i32 + 30);
        for resolution in 0..4 {
            for ambient in [-40., 25., 85.] {
                for object in [-40., 23.75, 31.25, 100., 300.] {
                    d.active = 0x1001 | (resolution << 10);
                    d.set(0, ambient);
                    d.set(16, object);
                    d.sample();
                    d.page ^= 1;
                    d.sample();
                    let gain = d.ee[48] as i16 as f64 / d.ram[778] as i16 as f64;
                    let recovered = ((d.ram[0] as i16 as f64 * gain / alpha + fourth(ambient)
                        - 0.05 * fourth(ambient - 8.))
                        / 0.95)
                        .sqrt()
                        .sqrt()
                        - 273.15;
                    assert!(
                        (recovered - object).abs() < 0.75,
                        "{resolution} {ambient} {object} {recovered}"
                    );
                    assert!((d.value(0) - ambient).abs() < 0.15);
                    assert!(d.ram[..768].iter().all(|raw| *raw == d.ram[0]));
                }
            }
        }
    }
    #[test]
    fn mlx90640_chess_interleaved_repeat_and_input_boundaries() {
        let mut d = device();
        for mode in [0, 0x1000] {
            d.active = 0x901 | mode;
            d.ram.fill(0);
            d.page = 0;
            d.set(16, 80.);
            d.sample();
            for p in 0..768 {
                let pattern = (p / 32 & 1) ^ if mode == 0 { 0 } else { p & 1 };
                assert_eq!(d.ram[p] != 0, pattern == 0);
            }
        }
        for (f, v) in [(0, 86.), (16, 301.), (16, f64::NAN), (1, 20.)] {
            assert!(!d.set(f, v));
        }
        d.write_word(0x800d, 0x1919);
        time(&mut d, 540000);
        assert_eq!(d.page, 1);
        time(&mut d, 1040000);
        assert_eq!(d.status & 1, 1);
        assert_eq!(d.page, 1);
    }
    #[test]
    fn mlx90640_i2c_word_address_endianness_readonly_and_partial_write() {
        let config = SensorConfig {
            id: 0,
            sda: 1,
            scl: 2,
            address: 0x33,
            model: 56,
            shunt_milliohms: 0,
        };
        assert!(config.valid());
        assert!(!SensorConfig {
            address: 0x34,
            ..config
        }
        .valid());
        let state = Arc::new(Mutex::new(Sensor::new(
            config,
            Arc::new(AtomicU64::new(0)),
            1_000_000,
        )));
        let mut bus = SensorI2c::new(state);
        assert!(bus.start(false));
        for b in [0x24, 0x30] {
            assert!(bus.write(b));
        }
        assert!(bus.start(true));
        assert_eq!(
            [bus.read(), bus.read(), bus.read(), bus.read()],
            [0x27, 0x10, 0x27, 0x10]
        );
        assert!(bus.start(false));
        for b in [0x80, 0x0d, 0x1b] {
            assert!(bus.write(b));
        }
        bus.stop();
        assert!(bus.start(false));
        for b in [0x80, 0x0d] {
            assert!(bus.write(b));
        }
        assert!(bus.start(true));
        assert_eq!([bus.read(), bus.read()], [0x19, 1]);
        assert!(bus.start(false));
        for b in [0x24, 0x00] {
            assert!(bus.write(b));
        }
        assert!(!bus.write(0));
    }
    #[test]
    fn mlx90640_auxiliary_subpage_banks_retain_previous_conversion() {
        let mut d = device();
        time(&mut d, 540000);
        assert_ne!(d.ram[768], 0);
        assert_eq!(d.ram[800], 0);
        assert!(d.value(0).is_nan());
        time(&mut d, 1040000);
        let previous = d.ram[768];
        let old_ta = d.value(0);
        time(&mut d, 1540000);
        d.set(0, 60.);
        time(&mut d, 2040000);
        assert_eq!(d.ram[768], previous);
        assert_eq!(d.value(0), old_ta);
        time(&mut d, 2540000);
        assert_ne!(d.ram[768], previous);
        assert!((d.value(0) - 60.).abs() < 0.15);
        time(&mut d, 3040000);
        assert!((d.value(0) - 60.).abs() < 0.15);
    }
    #[test]
    fn mlx90640_resolution_transition_requires_both_new_auxiliary_banks() {
        let mut d = device();
        time(&mut d, 1040000);
        d.write_word(0x800d, 0x1101);
        time(&mut d, 1540000);
        assert_eq!(d.ram[778], 10000);
        assert_eq!(d.ram[800], 768);
        time(&mut d, 2040000);
        assert_eq!(d.ram[778], 10000);
        assert_eq!(d.ram[800], 192);
        time(&mut d, 2539999);
        assert_eq!(d.ram[778], 10000);
        time(&mut d, 2540000);
        assert_eq!(d.ram[778], 2500);
        assert_eq!(d.ram[800], 192);
        assert!((d.value(0) - 25.).abs() < 0.15);
    }
    #[test]
    fn mlx90640_all_refresh_periods_and_full_page_mode() {
        for rate in 0..8 {
            let mut d = device();
            d.write_word(0x800d, 0x1801 | (rate << 7));
            time(&mut d, 540000);
            let period = 2_000_000 / (1 << rate);
            d.write_word(0x8000, 0x30);
            time(&mut d, 540000 + period - 1);
            assert_eq!(d.status & 8, 0);
            time(&mut d, 540000 + period);
            assert_eq!(d.status & 9, 9);
        }
        let mut d = device();
        d.active = 0x1900;
        d.set(16, 80.);
        d.sample();
        assert!(d.ram[..768]
            .iter()
            .all(|value| *value == d.ram[0] && *value != 0));
    }
    #[test]
    fn mlx90640_isolation_long_gap_and_fresh_power_on() {
        let mut a = device();
        let mut b = device();
        a.set(16, 90.);
        b.set(16, -20.);
        time(&mut a, 1_040_000);
        time(&mut b, 1_040_000);
        assert!((a.value(16) - 90.).abs() < 0.2);
        assert!((b.value(16) + 20.).abs() < 0.2);
        time(&mut a, 1_000_000_040_000);
        assert_eq!(a.status & 1, 1);
        let fresh = device();
        assert_eq!(fresh.control, 0x1901);
        assert_eq!(fresh.status, 0);
        assert!(fresh.s.readings[16].is_nan());
    }
}
