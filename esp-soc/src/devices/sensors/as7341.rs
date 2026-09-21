use super::*;

// AS7341 DS000504 v3-00; SMUX diode locations from ams' configuration in
// Adafruit_AS7341 0a32bc8f963a50d5a0e6ab6129869d8053a1fe02.
const DIODES: [(usize, u8, usize); 19] = [
    (0x01, 0, 0),
    (0x10, 0, 0),
    (0x05, 0, 1),
    (0x0c, 4, 1),
    (0x00, 4, 2),
    (0x0f, 4, 2),
    (0x05, 4, 3),
    (0x0d, 0, 3),
    (0x06, 4, 4),
    (0x09, 4, 4),
    (0x04, 0, 5),
    (0x0e, 4, 5),
    (0x07, 0, 6),
    (0x0a, 0, 6),
    (0x03, 4, 7),
    (0x0e, 0, 7),
    (0x08, 4, 8),
    (0x11, 4, 8),
    (0x13, 0, 9),
];
pub(super) struct As7341 {
    s: SampleState,
    mux: [u8; 20],
    mux_ready: Option<u64>,
    ready: Option<u64>,
    cycles: u64,
}
impl As7341 {
    pub fn new(clock: Arc<AtomicU64>, hz: u32) -> Self {
        let mut d = Self {
            s: SampleState::new(clock, hz),
            mux: [0; 20],
            mux_ready: None,
            ready: None,
            cycles: 0,
        };
        for i in 0..10 {
            d.s.inputs[64 + i] = (i + 1) as f64 * 10.;
        }
        for (r, v) in [
            (0x92, 0x24),
            (0xaa, 9),
            (0xac, 0x0c),
            (0xaf, 0x10),
            (0xca, 0xe7),
            (0xcb, 3),
            (0xcf, 0x99),
            (0xd6, 255),
            (0xda, 0x48),
            (0xb3, 0xf2),
        ] {
            d.s.regs[r] = v;
        }
        d
    }
    fn steps(&self) -> u64 {
        (self.s.regs[0x81] as u64 + 1)
            * (u16::from_le_bytes([self.s.regs[0xca], self.s.regs[0xcb]]) as u64 + 1)
    }
    fn integration(&self) -> u64 {
        (self.steps() * 278 * self.s.hz)
            .div_ceil(100_000_000)
            .max(1)
    }
    fn gain(&self) -> f64 {
        0.5 * (1u32 << self.s.regs[0xaa].min(10)) as f64
    }
    fn active(&self) -> bool {
        self.s.regs[0x80] & 0x13 == 3 && self.s.regs[0x70] & 3 == 0
    }
    fn period(&mut self) -> u64 {
        let integration = self.integration();
        if self.s.regs[0x80] & 8 == 0 {
            return integration;
        }
        let wait = self.s.ticks(
            (self.s.regs[0x83] as u64 + 1) * 2780 * if self.s.regs[0xa9] & 4 != 0 { 16 } else { 1 },
        );
        if wait < integration {
            self.s.regs[0xa7] |= 4;
        }
        wait.max(integration)
    }
    fn start_conversion(&mut self, at: u64) {
        if !self.active() {
            self.ready = None;
            return;
        }
        let az = self.s.regs[0xd6] != 0;
        self.cycles = 0;
        let duration = self.integration() + if az { self.s.ticks(15_000) } else { 0 };
        if self.s.regs[0x80] & 8 != 0 && self.period() < duration {
            self.s.regs[0xa7] |= 4;
        }
        self.ready = Some(at + duration);
        self.s.regs[0xa3] &= !0x40;
    }
    fn publish(&mut self, count: u64) {
        let factor = self.steps() as f64 * 0.00278 * self.gain();
        let limit = self.steps().min(65535) as f64;
        let mut adc = [0.; 6];
        let mut weights = [[0.; 10]; 6];
        self.s.readings[64..74].fill(f64::NAN);
        for &(r, shift, channel) in &DIODES {
            let target = (self.mux[r] >> shift) & 15;
            if (1..=6).contains(&target) {
                let weight = if channel == 9 { 1. } else { 0.5 };
                adc[target as usize - 1] += self.s.inputs[64 + channel] * factor * weight;
                weights[target as usize - 1][channel] += weight;
            }
        }
        let saturated = adc.iter().any(|v| *v >= limit);
        self.s.regs[0xa3] = 0x40 | if saturated { 0x10 } else { 0 };
        self.s.regs[0x94] = self.s.regs[0xaa] | if saturated { 0x80 } else { 0 };
        for (i, value) in adc.iter().enumerate() {
            if weights[i].iter().filter(|w| **w != 0.).count() == 1 {
                if let Some(channel) = weights[i].iter().position(|w| *w == 1.) {
                    self.s.readings[64 + channel] = value.round().min(limit) / factor;
                }
            }
            self.s.regs[0x95 + i * 2..0x97 + i * 2]
                .copy_from_slice(&(value.round().min(limit) as u16).to_le_bytes());
        }
        self.s.publish(count);
    }
}
impl RegisterSensor for As7341 {
    fn sync(&mut self) {
        self.s.time();
        if let Some(at) = self.mux_ready.filter(|at| *at <= self.s.now) {
            self.mux_ready = None;
            match (self.s.regs[0xaf] >> 3) & 3 {
                0 => self.mux = [0; 20],
                1 => self.s.regs[..20].copy_from_slice(&self.mux),
                2 => self.mux.copy_from_slice(&self.s.regs[..20]),
                _ => (),
            }
            self.s.regs[0x80] &= !0x10;
            self.start_conversion(at);
        }
        if let Some(at) = self.ready.filter(|at| *at <= self.s.now) {
            let period = self.period();
            let nth = self.s.regs[0xd6] as u64;
            let az_extra = (self.integration() + self.s.ticks(15_000)).saturating_sub(period);
            let delay = |intervals: u64| {
                let zeros = if (1..255).contains(&nth) {
                    (self.cycles + intervals) / nth - self.cycles / nth
                } else {
                    0
                };
                intervals * period + zeros * az_extra
            };
            let mut low = 0;
            let mut high = (self.s.now - at) / period + 1;
            while low + 1 < high {
                let mid = low + (high - low) / 2;
                if delay(mid) <= self.s.now - at {
                    low = mid;
                } else {
                    high = mid;
                }
            }
            let count = low + 1;
            self.ready = Some(at + delay(count));
            if self.s.regs[0x80] & 8 != 0
                && az_extra > 0
                && (1..255).contains(&nth)
                && (self.cycles + count) / nth != self.cycles / nth
            {
                self.s.regs[0xa7] |= 4;
            }
            self.cycles += count;
            self.publish(count);
        }
    }
    fn registers(&self) -> [u8; 256] {
        let mut regs = self.s.regs;
        if regs[0xa9] & 0x10 == 0 {
            regs[0x60..=0x74].fill(0);
        } else {
            regs[0x60] = regs[0x94];
            regs[0x61..0x63].copy_from_slice(&self.s.regs[0x95..0x97]);
            regs[0x66..0x70].copy_from_slice(&self.s.regs[0x97..0xa1]);
            for r in 0x80..256 {
                if r != 0xa9 {
                    regs[r] = 0;
                }
            }
        }
        regs
    }
    fn write(&mut self, reg: u8, value: u16) -> bool {
        self.sync();
        let r = reg as usize;
        let v = value as u8;
        if r != 0xa9
            && ((0x60..=0x74).contains(&r) && self.s.regs[0xa9] & 0x10 == 0
                || r >= 0x80 && self.s.regs[0xa9] & 0x10 != 0)
        {
            return true;
        }
        match r {
            0..=0x13 => self.s.regs[r] = v,
            0x70
            | 0x72..=0x74
            | 0x81
            | 0x83..=0x87
            | 0xa9
            | 0xac
            | 0xaf
            | 0xb2
            | 0xb5
            | 0xbd
            | 0xbe
            | 0xca
            | 0xcb
            | 0xd6
            | 0xf9 => self.s.regs[r] = v,
            0xaa if v <= 10 => self.s.regs[r] = v,
            0x93 => self.s.regs[r] &= !v,
            0x80 => {
                let old = self.s.regs[r];
                self.s.regs[r] = v & 0x5b;
                if v & 1 == 0 {
                    self.ready = None;
                    self.mux_ready = None;
                    self.s.regs[0xa3] &= !0x40;
                } else if v & 0x10 != 0 && old & 0x10 == 0 {
                    self.ready = None;
                    // ponytail: deterministic 1ms SMUX latency; characterize silicon for sub-ms timing.
                    self.mux_ready = Some(self.s.now + self.s.ticks(1000));
                } else if !self.active() {
                    self.ready = None;
                    self.s.regs[0xa3] &= !0x40;
                } else if old & 3 != 3 {
                    self.start_conversion(self.s.now);
                }
            }
            _ => (),
        }
        true
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        if !(64..=73).contains(&field) || !value.is_finite() || !(0.0..=65535.).contains(&value) {
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
    const LOW: [u8; 20] = [
        0x30, 1, 0, 0, 0, 0x42, 0, 0, 0x50, 0, 0, 0, 0x20, 4, 0, 0x30, 1, 0x50, 0, 6,
    ];
    fn advance(d: &mut As7341, us: u64) {
        d.s.clock.fetch_add(us, Ordering::Relaxed);
        d.sync();
    }
    fn raw(d: &As7341, ch: usize) -> u16 {
        u16::from_le_bytes([d.s.regs[0x95 + ch * 2], d.s.regs[0x96 + ch * 2]])
    }
    fn load(d: &mut As7341) {
        for (i, v) in LOW.iter().enumerate() {
            d.write(i as u8, *v as u16);
        }
        d.write(0x80, 0x11);
        advance(d, 1000);
    }
    #[test]
    fn as7341_wait_autozero_recurring_and_restart() {
        let mut d = As7341::new(Arc::new(AtomicU64::new(0)), 1_000_000);
        load(&mut d);
        d.write(0xaa, 1);
        d.write(0xd6, 2);
        d.write(0x80, 3);
        advance(&mut d, 17779);
        assert_eq!(d.generation(), 0);
        advance(&mut d, 1);
        assert_eq!(d.generation(), 1);
        advance(&mut d, 2780);
        assert_eq!(d.generation(), 2);
        advance(&mut d, 17779);
        assert_eq!(d.generation(), 2);
        advance(&mut d, 1);
        assert_eq!(d.generation(), 3);
        // Two integrations and one autozero per repeated block, including skipped host polls.
        advance(&mut d, 20560 * 100);
        assert_eq!(d.generation(), 203);
        d.write(0x80, 1);
        d.write(0xd6, 255);
        d.write(0x80, 3);
        advance(&mut d, 17779);
        assert_eq!(d.generation(), 203);
        advance(&mut d, 1);
        assert_eq!(d.generation(), 204);
        advance(&mut d, 2780);
        assert_eq!(d.generation(), 205);
        d.write(0x80, 1);
        d.write(0xd6, 0);
        d.write(0x83, 9);
        d.write(0x80, 11);
        advance(&mut d, 2780);
        assert_eq!(d.generation(), 206);
        advance(&mut d, 27799);
        assert_eq!(d.generation(), 206);
        advance(&mut d, 1);
        assert_eq!(d.generation(), 207);
        d.write(0x80, 1);
        d.write(0x81, 19);
        d.write(0x83, 0);
        d.write(0x80, 11);
        advance(&mut d, 55600);
        assert_eq!(d.generation(), 208);
        assert_ne!(d.s.regs[0xa7] & 4, 0);
        advance(&mut d, 55599);
        assert_eq!(d.generation(), 208);
        advance(&mut d, 1);
        assert_eq!(d.generation(), 209);
        for nth in [1, 2] {
            let mut d = As7341::new(Arc::new(AtomicU64::new(0)), 1_000_000);
            load(&mut d);
            d.write(0xd6, nth);
            d.write(0x83, 9);
            d.write(0x80, 11);
            advance(&mut d, 17780);
            assert_eq!(d.generation(), 1);
            advance(&mut d, 27799);
            assert_eq!(d.generation(), 1);
            advance(&mut d, 1);
            assert_eq!(d.generation(), 2);
            advance(&mut d, 27799);
            assert_eq!(d.generation(), 2);
            advance(&mut d, 1);
            assert_eq!(d.generation(), 3);
            assert_eq!(d.s.regs[0xa7] & 4, 0);
        }
    }
    #[test]
    fn as7341_high_mux_half_gain_counter_limit_and_custom_routing() {
        let mut d = As7341::new(Arc::new(AtomicU64::new(0)), 1_000_000);
        let high = [
            0, 0, 0, 0x40, 2, 0, 0x10, 3, 0x50, 0x10, 3, 0, 0, 0, 0x24, 0, 0, 0x50, 0, 6,
        ];
        for (i, v) in high.iter().enumerate() {
            d.write(i as u8, *v);
        }
        d.write(0x80, 0x11);
        assert_ne!(d.s.regs[0x80] & 0x10, 0);
        advance(&mut d, 999);
        assert_eq!(d.mux, [0; 20]);
        advance(&mut d, 1);
        d.write(0xaa, 0);
        d.write(0x81, 99);
        d.write(0x80, 3);
        advance(&mut d, 293000);
        assert_eq!(
            (0..6).map(|i| raw(&d, i)).collect::<Vec<_>>(),
            [6950, 8340, 9730, 11120, 12510, 13900]
        );
        assert!(d.value(64).is_nan());
        assert_eq!(d.value(68), 50.);
        assert_eq!(d.value(73), 100.);
        d.write(0x80, 1);
        d.set(73, 65535.);
        d.write(0xaa, 10);
        d.write(0x80, 3);
        advance(&mut d, 293000);
        assert_eq!(raw(&d, 5), 65535);
        d.write(0x80, 1);
        d.write(0xaf, 0x10);
        // Moving both F5 photodiodes to ADC3 must move the result, not select a canned six-value table.
        d.write(6, 0x40);
        d.write(9, 0x40);
        d.write(3, 0);
        d.write(0x0e, 0x20);
        d.write(0x80, 0x11);
        advance(&mut d, 1000);
        d.write(0xaa, 1);
        d.write(0x80, 3);
        advance(&mut d, 293000);
        assert_eq!(raw(&d, 0), 0);
        assert_eq!(raw(&d, 3), 13900);
        assert_eq!(d.value(68), 50.);
        d.write(0x80, 1);
        d.write(9, 0);
        d.write(0x80, 0x11);
        advance(&mut d, 1000);
        d.write(0x80, 3);
        advance(&mut d, 293000);
        assert_eq!(raw(&d, 3), 6950);
        assert!(d.value(68).is_nan());
        d.write(0x80, 1);
        d.write(9, 0x40);
        d.write(4, 4);
        d.write(0x0e, 0x40);
        d.write(0x80, 0x11);
        advance(&mut d, 1000);
        d.write(0x80, 3);
        advance(&mut d, 293000);
        assert_eq!(raw(&d, 3), 30580);
        assert!(d.value(68).is_nan());
        assert!(d.value(69).is_nan());
        let cfg = SensorConfig {
            id: 0,
            sda: 4,
            scl: 5,
            address: 0x39,
            model: 40,
            shunt_milliohms: 0,
        };
        assert!(cfg.valid());
        assert!(!SensorConfig {
            address: 0x38,
            ..cfg
        }
        .valid());
    }
    #[test]
    fn as7341_mux_integration_banks_gain_saturation_disable_and_defaults() {
        let mut d = As7341::new(Arc::new(AtomicU64::new(0)), 1_000_000);
        assert_eq!(d.s.regs[0x92], 0x24);
        assert_eq!(d.s.regs[0x80], 0);
        assert_eq!(d.steps(), 1000);
        assert_eq!(d.gain(), 256.);
        assert!(d.value(73).is_nan());
        assert!(!d.set(74, 1.));
        assert!(!d.set(64, f64::NAN));
        assert!(!d.set(64, -1.));
        load(&mut d);
        assert_eq!(d.s.regs[0x80] & 0x10, 0);
        assert_eq!(d.mux, LOW);
        d.write(0xaa, 1);
        d.write(0x81, 9);
        d.write(0x80, 3);
        advance(&mut d, 42_799);
        assert_eq!(d.generation(), 0);
        assert_eq!(d.s.regs[0xa3] & 0x40, 0);
        advance(&mut d, 1);
        assert_eq!(d.generation(), 1);
        assert_eq!(
            (0..6).map(|i| raw(&d, i)).collect::<Vec<_>>(),
            [278, 556, 834, 1112, 2502, 2780]
        );
        d.write(0x01, 2);
        advance(&mut d, 27800);
        assert_eq!(raw(&d, 0), 278); // RAM alone cannot change active SMUX.
        d.write(0x80, 1);
        d.write(0xaf, 8);
        d.write(0x80, 0x11);
        advance(&mut d, 1000);
        assert_eq!(d.s.regs[1], 1);
        d.write(0x74, 0xff);
        assert_eq!(d.s.regs[0x74], 0);
        d.write(0xa9, 0x10);
        d.write(0x74, 0x89);
        assert_eq!(d.registers()[0x74], 0x89);
        assert_eq!(d.registers()[0x92], 0);
        assert_eq!(d.registers()[0x61], d.s.regs[0x95]);
        d.write(0x92, 0);
        d.write(0xa9, 0);
        assert_eq!(d.registers()[0x92], 0x24);
        d.write(0xaa, 2);
        d.write(0x80, 3);
        advance(&mut d, 42800);
        assert_eq!(raw(&d, 0), 556);
        d.set(64, 65535.);
        advance(&mut d, 27800);
        assert_eq!(raw(&d, 0), 10000);
        assert_eq!(d.s.regs[0xa3] & 0x10, 0x10);
        d.write(0x80, 0);
        let generation = d.generation();
        d.set(64, 1.);
        advance(&mut d, 1_000_000);
        assert_eq!(d.generation(), generation);
        assert_eq!(raw(&d, 0), 10000);
        assert_eq!(d.s.regs[0xaa], 2); // PON is power gating, not a register reset.
        let fresh = As7341::new(d.s.clock.clone(), 1_000_000);
        assert_eq!(fresh.s.regs[0x80], 0);
        assert_eq!(fresh.s.regs[0xaa], 9);
        assert_eq!(raw(&fresh, 0), 0);
    }
}
