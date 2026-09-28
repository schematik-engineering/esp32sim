use super::*;

pub(super) struct Bno055 {
    s: SampleState,
    page: u8,
    config: [u8; 256],
    boot_at: u64,
    next: Option<u64>,
    next_mag: u64,
}
fn rotation(heading: f64, roll: f64, pitch: f64) -> [[f64; 3]; 3] {
    let (sh, ch) = (-heading.to_radians()).sin_cos();
    let (sr, cr) = (-roll.to_radians()).sin_cos();
    let (sp, cp) = (-pitch.to_radians()).sin_cos();
    [
        [ch * cr, ch * sr * sp - sh * cp, ch * sr * cp + sh * sp],
        [sh * cr, sh * sr * sp + ch * cp, sh * sr * cp - ch * sp],
        [-sr, cr * sp, cr * cp],
    ]
}
fn quaternion(m: [[f64; 3]; 3]) -> [f64; 4] {
    let trace = m[0][0] + m[1][1] + m[2][2];
    let mut q = [0.; 4];
    if trace > 0. {
        let a = (1. + trace).sqrt() * 2.;
        q = [
            a / 4.,
            (m[2][1] - m[1][2]) / a,
            (m[0][2] - m[2][0]) / a,
            (m[1][0] - m[0][1]) / a,
        ];
    } else {
        let i = if m[0][0] > m[1][1] && m[0][0] > m[2][2] {
            0
        } else if m[1][1] > m[2][2] {
            1
        } else {
            2
        };
        let j = (i + 1) % 3;
        let k = (i + 2) % 3;
        let a = (1. + m[i][i] - m[j][j] - m[k][k]).max(0.).sqrt() * 2.;
        q[0] = (m[k][j] - m[j][k]) / a;
        q[i + 1] = a / 4.;
        q[j + 1] = (m[j][i] + m[i][j]) / a;
        q[k + 1] = (m[k][i] + m[i][k]) / a;
    }
    if q[0] < 0. {
        for v in &mut q {
            *v = -*v;
        }
    }
    q
}
impl Bno055 {
    pub fn new(clock: Arc<AtomicU64>, hz: u32) -> Self {
        let mut d = Self {
            s: SampleState::new(clock, hz),
            page: 0,
            config: [0; 256],
            boot_at: 0,
            next: None,
            next_mag: 0,
        };
        d.s.inputs[0] = 25.;
        d.s.inputs[6] = 9.80665;
        d.s.inputs[30] = 30.;
        d.reset();
        d
    }
    fn reset(&mut self) {
        self.s.regs = [0; 256];
        self.config = [0; 256];
        self.page = 0;
        self.s.regs[..7].copy_from_slice(&[0xa0, 0xfb, 0x32, 0x0f, 0x11, 0x03, 0x15]);
        self.s.regs[0x36] = 0x0f;
        self.s.regs[0x3b] = 0x80;
        self.s.regs[0x41] = 0x24;
        self.config[8] = 0x0d;
        self.config[9] = 0x0b;
        self.config[10] = 0x38;
        self.s.readings = [f64::NAN; FIELD_COUNT];
        self.s.generation = 0;
        self.next = None;
        self.next_mag = 0;
        self.boot_at = self.s.now + self.s.ticks(650000);
    }
    fn mode(&self) -> u8 {
        self.s.regs[0x3d] & 15
    }
    fn period(&self) -> u64 {
        self.s.hz
            / match self.mode() {
                9 => 20,
                10 => 50,
                _ => 100,
            }
    }
    fn put(&mut self, reg: usize, value: f64, scale: f64) -> f64 {
        let raw = (value * scale).round().clamp(-32768., 32767.) as i16;
        self.s.regs[reg..reg + 2].copy_from_slice(&raw.to_le_bytes());
        raw as f64 / scale
    }
    fn mapped(&self, first: usize) -> [f64; 3] {
        std::array::from_fn(|axis| {
            let src = ((self.s.regs[0x41] >> (axis * 2)) & 3) as usize;
            if src > 2 {
                return 0.;
            }
            let sign = if self.s.regs[0x42] & (1 << (2 - axis)) != 0 {
                -1.
            } else {
                1.
            };
            self.s.inputs[first + src] * sign
        })
    }
    fn pose(&self) -> Option<[[f64; 3]; 3]> {
        let base = rotation(self.s.inputs[26], self.s.inputs[27], self.s.inputs[28]);
        let mut out = [[0.; 3]; 3];
        let mut axes = 0;
        for a in 0..3 {
            let src = ((self.s.regs[0x41] >> (a * 2)) & 3) as usize;
            if src > 2 || axes & (1 << src) != 0 {
                return None;
            }
            axes |= 1 << src;
            let sign = if self.s.regs[0x42] & (1 << (2 - a)) != 0 {
                -1.
            } else {
                1.
            };
            for r in 0..3 {
                out[r][a] = base[r][src] * sign;
            }
        }
        let d = out[0][0] * (out[1][1] * out[2][2] - out[1][2] * out[2][1])
            - out[0][1] * (out[1][0] * out[2][2] - out[1][2] * out[2][0])
            + out[0][2] * (out[1][0] * out[2][1] - out[1][1] * out[2][0]);
        (d > 0.5).then_some(out)
    }
    fn capture(&mut self, count: u64) {
        let mode = self.mode();
        let units = self.s.regs[0x3b];
        let mask = [0, 1, 2, 4, 3, 5, 6, 7, 5, 3, 3, 7, 7][mode as usize];
        let acc = self.mapped(4);
        let gyro = self.mapped(7);
        let mag = self.mapped(29);
        let acc_scale = if units & 1 == 0 {
            100.
        } else {
            1000. / 9.80665
        };
        let gyro_scale = if units & 2 == 0 {
            16. * 180. / std::f64::consts::PI
        } else {
            900.
        };
        let acc_limit = 9.80665 * (2u32 << ((self.config[8] & 3) as u32)) as f64;
        let gyro_limit =
            [2000f64, 1000., 500., 250., 125.][(self.config[10] & 7).min(4) as usize].to_radians();
        let sample_mag = self.s.now >= self.next_mag;
        if sample_mag {
            self.next_mag = self.s.now + self.s.hz / if mode == 10 { 50 } else { 20 };
        }
        for axis in 0..3 {
            if mask & 1 != 0 {
                self.s.readings[4 + axis] = self.put(
                    0x08 + axis * 2,
                    acc[axis].clamp(-acc_limit, acc_limit),
                    acc_scale,
                );
            }
            if mask & 4 != 0 {
                self.s.readings[7 + axis] = self.put(
                    0x14 + axis * 2,
                    gyro[axis].clamp(-gyro_limit, gyro_limit),
                    gyro_scale,
                );
            }
            if mask & 2 != 0 && sample_mag {
                self.s.readings[29 + axis] = self.put(0x0e + axis * 2, mag[axis], 16.);
            }
        }
        if mode >= 8 {
            if let Some(m) = self.pose() {
                let e = [
                    (-m[1][0].atan2(m[0][0])).to_degrees().rem_euclid(360.),
                    m[2][0].clamp(-1., 1.).asin().to_degrees(),
                    (-m[2][1].atan2(m[2][2])).to_degrees(),
                ];
                let angle_scale = if units & 4 == 0 {
                    16.
                } else {
                    900. * std::f64::consts::PI / 180.
                };
                for axis in 0..3 {
                    let v = if axis == 2 && units & 0x80 == 0 {
                        -e[axis]
                    } else {
                        e[axis]
                    };
                    self.s.readings[26 + axis] = self.put(0x1a + axis * 2, v, angle_scale);
                }
                for (axis, q) in quaternion(m).into_iter().enumerate() {
                    self.s.readings[32 + axis] = self.put(0x20 + axis * 2, q, 16384.);
                }
                for axis in 0..3 {
                    let gravity = m[2][axis] * 9.80665;
                    self.s.readings[39 + axis] = self.put(0x2e + axis * 2, gravity, acc_scale);
                    self.s.readings[36 + axis] =
                        self.put(0x28 + axis * 2, acc[axis] - gravity, acc_scale);
                }
            }
        }
        let temp = if units & 0x10 == 0 {
            self.s.inputs[0]
        } else {
            (self.s.inputs[0] * 1.8 + 32.) / 2.
        };
        let raw = temp.round().clamp(-128., 127.) as i8;
        self.s.regs[0x34] = raw as u8;
        self.s.readings[0] = if units & 0x10 == 0 {
            raw as f64
        } else {
            (raw as f64 * 2. - 32.) / 1.8
        };
        self.s.regs[0x35] = 0;
        for i in 0..4 {
            self.s.regs[0x35] |= (self.s.inputs[42 + i] as u8) << (6 - 2 * i);
            self.s.readings[42 + i] = self.s.inputs[42 + i];
        }
        self.s.regs[0x39] = if mode >= 8 { 5 } else { 6 };
        self.s.publish(count);
    }
}
impl RegisterSensor for Bno055 {
    fn address_ready(&self) -> bool {
        self.s.now >= self.boot_at
    }
    fn sync(&mut self) {
        self.s.time();
        if self.s.now < self.boot_at {
            return;
        }
        if let Some(next) = self.next {
            if self.s.now >= next {
                let period = self.period().max(1);
                let n = 1 + (self.s.now - next) / period;
                self.capture(n);
                self.next = Some(next + n * period);
            }
        }
    }
    fn registers(&self) -> [u8; 256] {
        let mut regs = if self.page == 0 {
            self.s.regs
        } else {
            self.config
        };
        regs[7] = self.page;
        regs
    }
    fn write(&mut self, reg: u8, value: u16) -> bool {
        self.sync();
        let v = value as u8;
        if reg == 7 {
            self.page = v & 1;
            return true;
        }
        if self.page == 1 {
            if self.mode() == 0 && (8..=0x1f).contains(&reg) {
                self.config[reg as usize] = v;
            }
            return true;
        }
        match reg {
            0x3f if v & 0x20 != 0 => self.reset(),
            0x3d if v <= 12 => {
                let old = self.mode();
                self.s.regs[0x3d] = v;
                self.s.regs[0x39] = 0;
                self.next = if v == 0 || self.s.regs[0x3e] == 2 {
                    None
                } else {
                    Some(
                        self.s.now
                            + self.s.ticks(if old == 0 { 7000 } else { 19000 })
                            + self.period(),
                    )
                };
                if v == 0 {
                    self.s.readings = [f64::NAN; FIELD_COUNT];
                }
            }
            0x3e if v <= 2 => {
                self.s.regs[0x3e] = v;
                self.next = if v == 2 || self.mode() == 0 {
                    None
                } else {
                    Some(self.s.now + self.period())
                };
            }
            0x3b | 0x40..=0x42 | 0x55..=0x6a if self.mode() == 0 => self.s.regs[reg as usize] = v,
            0x3f => self.s.regs[0x3f] = v & 0x80,
            _ => {}
        }
        true
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        let range = match field {
            0 => -40.0..=85.0,
            4..=6 => -156.9064..=156.9064,
            7..=9 => -34.90658504..=34.90658504,
            26 => 0.0..=360.0,
            27 => -90.0..=90.0,
            28 => -180.0..=180.0,
            29..=31 => -1300.0..=1300.0,
            42..=45 => 0.0..=3.0,
            _ => return false,
        };
        if !value.is_finite() || !range.contains(&value) || (field >= 42 && value.fract() != 0.) {
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
    fn read16(d: &Bno055, reg: usize) -> i16 {
        i16::from_le_bytes(d.registers()[reg..reg + 2].try_into().unwrap())
    }
    #[test]
    fn bno_boot_pose_units_calibration_suspend_and_reset() {
        let clock = Arc::new(AtomicU64::new(0));
        let mut d = Bno055::new(clock.clone(), 1_000_000);
        assert!(!d.address_ready());
        clock.store(649999, Ordering::Relaxed);
        d.sync();
        assert!(!d.address_ready());
        clock.store(650000, Ordering::Relaxed);
        d.sync();
        assert!(d.address_ready());
        d.set(26, 90.);
        assert!(!d.set(42, 1.5));
        assert!(!d.set(32, 0.));
        d.write(0x3d, 12);
        clock.store(666999, Ordering::Relaxed);
        assert_eq!(d.generation(), 0);
        clock.store(667000, Ordering::Relaxed);
        assert_eq!(d.generation(), 1);
        assert_eq!(read16(&d, 0x1a), 1440);
        assert_eq!(read16(&d, 0x20), 11585);
        assert_eq!(read16(&d, 0x26), -11585);
        assert!((d.value(41) - 9.81).abs() < 1e-9);
        assert_eq!(d.value(38), 0.);
        assert_eq!(d.registers()[0x35], 0);
        d.set(42, 3.);
        d.set(43, 2.);
        d.set(44, 1.);
        d.set(45, 3.);
        clock.store(677000, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.registers()[0x35], 0xe7);
        d.write(0x3d, 0);
        d.write(0x3b, 0x87);
        d.write(0x3d, 12);
        clock.store(694000, Ordering::Relaxed);
        d.sync();
        assert_eq!(read16(&d, 0x1a), 1414);
        assert_eq!(read16(&d, 0x0c), 1000);
        d.write(0x3e, 2);
        let generation = d.generation();
        clock.store(900000, Ordering::Relaxed);
        assert_eq!(d.generation(), generation);
        d.write(0x3e, 0);
        clock.store(910000, Ordering::Relaxed);
        assert!(d.generation() > generation);
        d.write(0x3f, 0x20);
        assert!(!d.address_ready());
        assert_eq!(d.s.inputs[26], 90.);
        clock.store(1560000, Ordering::Relaxed);
        d.sync();
        assert!(d.address_ready());
        assert_eq!(d.registers()[0x3d], 0);
        assert_eq!(d.registers()[0x3b], 0x80);
        assert_eq!(d.registers()[0x35], 0);
    }
    #[test]
    fn magnetic_channel_keeps_documented_ndof_twenty_hz_cadence() {
        let clock = Arc::new(AtomicU64::new(650000));
        let mut d = Bno055::new(clock.clone(), 1_000_000);
        d.sync();
        d.write(0x3d, 12);
        clock.store(667000, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.value(29), 0.);
        d.set(29, 100.);
        clock.store(677000, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.value(29), 0.);
        clock.store(717000, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.value(29), 100.);
    }
    #[test]
    fn pose_single_axes_and_remapping_preserve_unit_rotation() {
        for (h, r, p) in [
            (0., 0., 0.),
            (90., 0., 0.),
            (0., 90., 0.),
            (0., 0., 90.),
            (120., 20., -30.),
        ] {
            let m = rotation(h, r, p);
            let q = quaternion(m);
            assert!((q.iter().map(|v| v * v).sum::<f64>() - 1.).abs() < 1e-12);
            for row in m {
                assert!((row.iter().map(|v| v * v).sum::<f64>() - 1.).abs() < 1e-12);
            }
        }
        let mut d = Bno055::new(Arc::new(AtomicU64::new(0)), 1_000_000);
        d.s.regs[0x41] = 0x21;
        d.s.regs[0x42] = 4;
        assert!(d.pose().is_some());
        d.s.regs[0x42] = 0;
        assert!(d.pose().is_none());
    }
}
