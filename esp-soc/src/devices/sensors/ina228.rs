use super::*;

// TI SLYS021A, sections 7.3.2, 7.3.4, 7.6 and 8.1.2.
pub(super) struct Ina228 {
    s: SampleState,
    shunt: u16,
    next: Option<u64>,
    phase: usize,
    samples: [u32; 3],
    sums: [f64; 4],
    current: f64,
    energy: f64,
    charge: f64,
}
impl Ina228 {
    pub fn new(clock: Arc<AtomicU64>, hz: u32, shunt: u16) -> Self {
        let mut d = Self {
            s: SampleState::new(clock, hz),
            shunt: if shunt == 0 { 15 } else { shunt },
            next: None,
            phase: 0,
            samples: [0; 3],
            sums: [0.; 4],
            current: 0.,
            energy: 0.,
            charge: 0.,
        };
        d.s.inputs[0] = 25.;
        d.s.inputs[10] = 5.;
        d.s.inputs[11] = 1.5;
        d.reset();
        d
    }
    fn offset(reg: u8) -> usize {
        match reg {
            9 => 80,
            10 => 85,
            _ => reg as usize * 4,
        }
    }
    fn put(&mut self, reg: u8, value: u64) {
        let width = self.register_width(reg) as usize;
        let offset = Self::offset(reg);
        self.s.regs[offset..offset + width].copy_from_slice(&value.to_be_bytes()[8 - width..]);
    }
    fn get(&self, reg: u8) -> u64 {
        let offset = Self::offset(reg);
        self.s.regs[offset..offset + self.register_width(reg) as usize]
            .iter()
            .fold(0, |v, b| v * 256 + *b as u64)
    }
    fn reset(&mut self) {
        self.s.regs.fill(0);
        for (reg, value) in [
            (1, 0xfb68),
            (2, 0x1000),
            (11, 1),
            (12, 0x7fff),
            (13, 0x8000),
            (14, 0x7fff),
            (16, 0x7fff),
            (17, 0xffff),
            (0x3e, 0x5449),
            (0x3f, 0x2281),
        ] {
            self.put(reg, value);
        }
        self.energy = 0.;
        self.charge = 0.;
        self.current = 0.;
        self.s.readings.fill(f64::NAN);
        self.s.generation = 0;
        self.restart();
    }
    fn mode(&self) -> u64 {
        self.get(1) >> 12
    }
    fn enabled(&self, phase: usize) -> bool {
        self.mode() & [2, 1, 4][phase] != 0
    }
    fn phase_us(&self, phase: usize) -> u64 {
        [50, 84, 150, 280, 540, 1052, 2074, 4120][((self.get(1) >> [6, 9, 3][phase]) & 7) as usize]
    }
    fn cycle_us(&self) -> u64 {
        (0..3)
            .filter(|p| self.enabled(*p))
            .map(|p| self.phase_us(p))
            .sum()
    }
    fn averages(&self) -> u32 {
        [1, 4, 16, 64, 128, 256, 512, 1024][(self.get(1) & 7) as usize]
    }
    fn restart(&mut self) {
        self.samples = [0; 3];
        self.sums = [0.; 4];
        self.next = (0..3).find(|p| self.enabled(*p)).map(|p| {
            self.phase = p;
            self.s.now
                + self
                    .s
                    .ticks(self.phase_us(p) + ((self.get(0) >> 6) & 255) * 2000)
                    .max(1)
        });
    }
    fn shunt_lsb(&self) -> f64 {
        if self.get(0) & 16 != 0 {
            0.000078125
        } else {
            0.0003125
        }
    }
    fn current_lsb(&self) -> f64 {
        self.get(2) as f64
            / (13107.2e6 * self.shunt as f64 / 1000. * if self.get(0) & 16 != 0 { 4. } else { 1. })
    }
    fn sample(&mut self) {
        let phase = self.phase;
        let raw = match phase {
            0 => (self.s.inputs[11] / self.shunt_lsb())
                .round()
                .clamp(-524288., 524287.),
            1 => (self.s.inputs[10] / 0.0001953125).round(),
            _ => (self.s.inputs[0] * 128.).round(),
        };
        self.samples[phase] += 1;
        self.sums[phase] += raw;
        if phase == 0 {
            self.current = if self.get(2) == 0 {
                0.
            } else {
                raw * 4096. / self.get(2) as f64
            };
            if self.get(0) & 32 != 0 {
                self.current /=
                    1. + (self.get(6) as u16 as i16 as f64 / 128. - 25.) * self.get(3) as f64 / 1e6;
            }
            self.sums[3] += self.current;
            if self.mode() & 8 != 0 {
                self.charge += self.current * self.cycle_us() as f64 / 1e6;
            }
        }
        if phase == 1 && self.mode() & 8 != 0 {
            self.energy += self.current.abs() * raw / 16384. * self.cycle_us() as f64 / 16e6;
        }
        if self.get(2) == 0 {
            self.energy = 0.;
            self.charge = 0.;
        }
        let mut flags = self.get(11);
        if self.energy >= (1u64 << 40) as f64 {
            flags |= 1 << 11;
            self.energy = self.energy.rem_euclid((1u64 << 40) as f64);
        }
        if self.charge < -(1i64 << 39) as f64 || self.charge >= (1i64 << 39) as f64 {
            flags |= 1 << 10;
            self.charge = (self.charge + (1i64 << 39) as f64).rem_euclid((1u64 << 40) as f64)
                - (1i64 << 39) as f64;
        }
        self.put(9, self.energy as u64);
        self.put(10, self.charge as i64 as u64);
        let average_done = self.samples[phase] == self.averages()
            && (self.averages() == 1 || !(phase + 1..3).any(|p| self.enabled(p)));
        for phase in 0..3 {
            if !average_done || self.samples[phase] != self.averages() {
                continue;
            }
            let count = self.samples[phase];
            let value = (self.sums[phase] / self.samples[phase] as f64).round() as i32;
            self.put(
                [4, 5, 6][phase],
                if phase == 2 {
                    value as u64
                } else {
                    (value << 4) as u64
                },
            );
            self.s.readings[[11, 10, 0][phase]] =
                value as f64 * [self.shunt_lsb(), 0.0001953125, 1. / 128.][phase];
            self.samples[phase] = 0;
            self.sums[phase] = 0.;
            if phase == 0 {
                let current = self.sums[3] / count as f64;
                self.sums[3] = 0.;
                if !(-524288.0..=524287.0).contains(&current) {
                    flags |= 512;
                }
                self.put(7, ((current as i64) << 4) as u64);
                self.s.readings[12] = if flags & 512 == 0 {
                    current.trunc() * self.current_lsb() * 1000.
                } else {
                    f64::NAN
                };
            }
            if phase == 1 {
                let current = ((self.get(7) as i32) << 8 >> 12) as f64;
                let power = (current.abs() * value as f64 / 16384.).trunc();
                if power > 16777215. {
                    flags |= 512;
                }
                self.put(8, power as u64);
                self.s.readings[13] = if flags & 512 == 0 {
                    power * self.current_lsb() * 3200.
                } else {
                    f64::NAN
                };
            }
            self.s.readings[15] = self.shunt as f64;
        }
        self.put(11, flags);
    }
}
impl RegisterSensor for Ina228 {
    fn format(&self) -> WireFormat {
        WireFormat::Block
    }
    fn register_offset(&self, reg: u8) -> usize {
        Self::offset(reg)
    }
    fn register_width(&self, reg: u8) -> u8 {
        match reg {
            4 | 5 | 7 | 8 => 3,
            9 | 10 => 5,
            _ => 2,
        }
    }
    fn sync(&mut self) {
        self.s.time();
        while let Some(next) = self.next {
            if self.s.now < next {
                break;
            }
            self.sample();
            let following = (self.phase + 1..3).find(|p| self.enabled(*p));
            if following.is_none() && self.samples[self.phase] == 0 {
                self.put(11, self.get(11) | 2);
                self.s.publish(1);
                if self.mode() & 8 == 0 {
                    self.next = None;
                    break;
                }
            }
            self.phase = following.unwrap_or_else(|| (0..3).find(|p| self.enabled(*p)).unwrap());
            self.next = Some(next + self.s.ticks(self.phase_us(self.phase)).max(1));
        }
    }
    fn registers(&self) -> [u8; 256] {
        self.s.regs
    }
    fn write(&mut self, reg: u8, value: u16) -> bool {
        self.sync();
        match reg {
            0 if value & 0x8000 != 0 => self.reset(),
            0 => {
                if value & 0x4000 != 0 {
                    self.energy = 0.;
                    self.charge = 0.;
                    self.put(9, 0);
                    self.put(10, 0);
                    self.put(11, self.get(11) & !0xe00);
                }
                self.put(0, (value & 0x3ff0) as u64);
            }
            1 => {
                self.put(1, value as u64);
                if self.mode() & 7 != 0 {
                    self.put(11, self.get(11) & !0x202);
                }
                self.restart();
            }
            2 => self.put(2, (value & 0x7fff) as u64),
            3 => self.put(3, (value & 0x3fff) as u64),
            11 => self.put(11, (self.get(11) & 0xeff) | (value as u64 & 0xf000)),
            12..=17 => self.put(
                reg,
                value as u64
                    & if reg == 14 || reg == 15 {
                        0x7fff
                    } else {
                        0xffff
                    },
            ),
            _ => {}
        }
        true
    }
    fn read_done(&mut self, reg: u8) {
        let mask = match reg {
            9 => 0x800,
            10 => 0x400,
            11 => {
                if self.get(11) & 0x8000 != 0 {
                    0xfe
                } else {
                    2
                }
            }
            _ => 0,
        };
        self.put(11, self.get(11) & !mask);
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        if !value.is_finite() {
            return false;
        }
        let valid = match field {
            0 => (-40.0..=125.0).contains(&value),
            10 => (0.0..=85.0).contains(&value),
            11 => (-163.84..=163.84).contains(&value),
            12 => value.abs() * self.shunt as f64 / 1000. <= 163.84,
            15 => value.fract() == 0. && (1.0..=65535.0).contains(&value),
            _ => false,
        };
        if !valid {
            return false;
        }
        self.sync();
        match field {
            12 => self.s.inputs[11] = value * self.shunt as f64 / 1000.,
            15 => self.shunt = value as u16,
            _ => self.s.inputs[field as usize] = value,
        }
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
    fn tick(d: &mut Ina228, us: u64) {
        d.s.clock.store(us, Ordering::Relaxed);
        d.sync();
    }
    fn signed(d: &Ina228, reg: u8) -> i32 {
        (d.get(reg) as i32) << 8 >> 12
    }
    #[test]
    fn ina228_timed_channels_calibration_ranges_average_and_reset() {
        let mut d = Ina228::new(Arc::new(AtomicU64::new(0)), 1_000_000, 15);
        assert_eq!(
            (d.get(0), d.get(1), d.get(2), d.get(0x3e), d.get(0x3f)),
            (0, 0xfb68, 4096, 0x5449, 0x2281)
        );
        assert!(d.set(10, 12.));
        assert!(d.set(12, -1000.));
        d.write(2, 7500); // 20 A / 2^19 current LSB, 15 mOhm.
        d.write(1, 0xf000); // 50 us per channel.
        tick(&mut d, 49);
        assert_eq!(d.get(4), 0);
        tick(&mut d, 50);
        assert_eq!(signed(&d, 4), -48000);
        assert_eq!(signed(&d, 7), -26214);
        assert_eq!(d.get(5), 0);
        assert_eq!(d.generation(), 0);
        tick(&mut d, 100);
        assert_eq!(d.get(5), 61440 << 4);
        assert!((d.value(13) - 12000.).abs() < 0.5);
        tick(&mut d, 150);
        assert_eq!(d.generation(), 1);
        assert_eq!(d.get(6), 3200);
        assert_eq!(d.get(11) & 2, 2);
        d.read_done(11);
        assert_eq!(d.get(11) & 2, 0);
        d.write(1, 0x0000);
        let frozen = d.get(4);
        d.set(11, 60.);
        tick(&mut d, 1000);
        assert_eq!(d.get(4), frozen);
        d.write(0, 16);
        d.write(2, 30000);
        d.write(1, 0x7000);
        tick(&mut d, 1149);
        assert_eq!(signed(&d, 4), 524287);
        assert!((d.value(11) - 40.959921875).abs() < 1e-9);
        tick(&mut d, 1150);
        assert_eq!(d.generation(), 2);
        tick(&mut d, 9999);
        assert_eq!(d.generation(), 2);
        d.set(11, -163.84);
        d.write(1, 0x7000);
        tick(&mut d, 10149);
        assert_eq!(signed(&d, 4), -524288);
        d.write(0, 0);
        d.write(2, 7500);
        d.set(11, 15.);
        d.write(1, 0x2001); // Four shunt samples, triggered.
        tick(&mut d, 10199);
        d.set(11, -15.);
        tick(&mut d, 10349);
        assert_eq!(signed(&d, 4), -24000);
        assert_eq!(d.next, None);
        d.write(2, 0);
        d.write(1, 0x3000);
        tick(&mut d, 10449);
        assert_eq!(d.get(7), 0);
        assert_eq!(d.get(8), 0);
        assert_eq!(d.get(9), 0);
        assert_eq!(d.get(10), 0);
        d.write(0, 0x8000);
        assert_eq!(d.get(1), 0xfb68);
        assert_eq!(d.get(2), 4096);
        assert_eq!(d.get(4), 0);
        assert_eq!(d.get(11), 1);
        assert_eq!(d.s.inputs[11], -15.);
    }
    #[test]
    fn ina228_delayed_average_overflow_and_fractional_accumulation() {
        let mut d = Ina228::new(Arc::new(AtomicU64::new(0)), 1_000_000, 15);
        d.set(11, 15.);
        d.write(2, 7500);
        d.write(0, 64);
        d.write(1, 0x7001);
        tick(&mut d, 2549);
        assert_eq!(d.get(4), 0);
        assert_eq!(d.generation(), 0);
        tick(&mut d, 2550);
        assert_eq!(d.get(4), 0);
        tick(&mut d, 2600);
        assert_eq!(signed(&d, 4), 48000);
        assert_eq!(d.generation(), 1);
        d.write(0, 0);
        d.write(2, 1);
        d.write(1, 0x2000);
        tick(&mut d, 2650);
        assert_eq!(d.get(11) & 512, 512);
        assert!(d.value(12).is_nan());
        d.write(2, 7500);
        d.set(11, 0.0003125);
        d.write(1, 0xa000);
        tick(&mut d, 1_002_650);
        assert_eq!(d.get(11) & 512, 0);
        assert!((d.charge * d.current_lsb() - 0.00002083333333).abs() < 1e-10);
        d.charge = (1i64 << 39) as f64 - 0.001;
        d.energy = (1u64 << 40) as f64 - 0.001;
        d.set(11, 15.);
        d.write(1, 0xb000);
        tick(&mut d, 1_002_750);
        assert_eq!(d.get(11) & 0xc00, 0xc00);
        d.read_done(9);
        assert_eq!(d.get(11) & 0xc00, 0x400);
        d.read_done(10);
        assert_eq!(d.get(11) & 0xc00, 0);
    }
    #[test]
    fn ina228_accumulation_shunt_independence_limits_and_block_reads() {
        let clock = Arc::new(AtomicU64::new(0));
        let mut d = Ina228::new(clock.clone(), 1_000_000, 15);
        assert!(d.set(11, 15.));
        assert!(d.set(15, 30.));
        assert_eq!(d.s.inputs[11], 15.);
        assert!(!d.set(15, 0.));
        assert!(!d.set(15, 1.5));
        assert!(!d.set(10, 85.001));
        assert!(!d.set(11, f64::NAN));
        assert!(!d.set(11, -163.841));
        assert!(!d.set(12, 6000.));
        assert!(!d.set(13, 1.));
        d.set(10, 12.);
        d.write(2, 15000);
        d.write(1, 0xb000);
        tick(&mut d, 1_000_000);
        assert!((d.get(9) as f64 * 51.2 * d.current_lsb() - 6.).abs() < 0.003);
        assert!((d.get(10) as f64 * d.current_lsb() - 0.5).abs() < 0.0001);
        d.write(1, 0);
        d.write(0, 0x4000);
        assert_eq!(d.get(9), 0);
        assert_eq!(d.get(10), 0);
        d.write(0, 0);
        d.set(11, -15.);
        d.write(1, 0xb000);
        tick(&mut d, 2_000_000);
        assert!(d.charge < 0.);
        assert!(d.energy > 0.);
        d.write(1, 0);
        d.put(9, 0x123456789a);
        d.put(10, 0xfedcba9876);
        d.put(11, 0x4001);
        let config = SensorConfig {
            id: 0,
            sda: 4,
            scl: 5,
            address: 0x40,
            model: 38,
            shunt_milliohms: 15,
        };
        assert!(config.valid());
        assert!(!SensorConfig {
            address: 0x50,
            ..config
        }
        .valid());
        let state = Arc::new(Mutex::new(Sensor {
            config,
            device: Box::new(d),
        }));
        let mut bus = SensorI2c::new(state);
        for (reg, expected) in [
            (9, vec![0x12, 0x34, 0x56, 0x78, 0x9a]),
            (10, vec![0xfe, 0xdc, 0xba, 0x98, 0x76]),
            (11, vec![0x40, 0x01]),
            (0x3e, vec![0x54, 0x49]),
            (0x3f, vec![0x22, 0x81]),
        ] {
            assert!(bus.start(false));
            assert!(bus.write(reg));
            assert!(bus.start(true));
            assert_eq!(
                (0..expected.len()).map(|_| bus.read()).collect::<Vec<_>>(),
                expected
            );
            assert_eq!(bus.read(), 0xff);
        }
    }
}
