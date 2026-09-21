/*
 * Copyright (c) 2021 Bosch Sensortec GmbH. All rights reserved.
 *
 * BSD-3-Clause
 *
 * Redistribution and use in source and binary forms, with or without
 * modification, are permitted provided that the following conditions are met:
 *
 * 1. Redistributions of source code must retain the above copyright
 *    notice, this list of conditions and the following disclaimer.
 *
 * 2. Redistributions in binary form must reproduce the above copyright
 *    notice, this list of conditions and the following disclaimer in the
 *    documentation and/or other materials provided with the distribution.
 *
 * 3. Neither the name of the copyright holder nor the names of its
 *    contributors may be used to endorse or promote products derived from
 *    this software without specific prior written permission.
 *
 * THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
 * "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
 * LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS
 * FOR A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE
 * COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT,
 * INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES
 * (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
 * SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION)
 * HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT,
 * STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING
 * IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE
 * POSSIBILITY OF SUCH DAMAGE.
 *
 */
use super::*;
const T1: f64 = 27504.;
const T2: f64 = 26435.;
const T3: f64 = 3.;
const P: [f64; 10] = [
    36477., -10685., 88., 3024., 2855., 30., -100., -14600., 6000., 30.,
];
const H: [f64; 7] = [75., 362., 0., 45., 20., 120., -100.];
fn temp(raw: f64) -> (f64, f64) {
    let a = (raw / 16384. - T1 / 1024.) * T2;
    let b = (raw / 131072. - T1 / 8192.).powi(2) * T3 * 16.;
    ((a + b) / 5120., a + b)
}
fn press(raw: f64, fine: f64) -> f64 {
    let mut a = fine / 2. - 64000.;
    let mut b = a * a * P[5] / 131072.;
    b += a * P[4] * 2.;
    b = b / 4. + P[3] * 65536.;
    a = ((P[2] * a * a / 16384. + P[1] * a) / 524288. / 32768. + 1.) * P[0];
    let p = (1048576. - raw - b / 4096.) * 6250. / a;
    p + (P[8] * p * p / 2147483648.
        + p * P[7] / 32768.
        + (p / 256.).powi(3) * P[9] / 131072.
        + P[6] * 128.)
        / 16.
}
fn humid(raw: f64, t: f64) -> f64 {
    let a = raw - (H[0] * 16. + H[2] / 2. * t);
    let b = a * (H[1] / 262144.) * (1. + H[3] / 16384. * t + H[4] / 1048576. * t * t);
    (b + (H[5] / 16384. + H[6] / 2097152. * t) * b * b).clamp(0., 100.)
}
fn gas(raw: f64, range: usize) -> f64 {
    const K1: [f64; 16] = [
        0., 0., 0., 0., 0., -1., 0., -0.8, 0., 0., -0.2, -0.5, 0., -1., 0., 0.,
    ];
    const K2: [f64; 16] = [
        0., 0., 0., 0., 0.1, 0.7, 0., -0.8, -0.1, 0., 0., 0., 0., 0., 0., 0.,
    ];
    1. / ((1. + K2[range] / 100.)
        * 0.000000125
        * (1u32 << range) as f64
        * ((raw - 512.) / (1340. * (1. + K1[range] / 100.)) + 1.))
}
pub(super) struct Bme680 {
    s: SampleState,
    next: Option<u64>,
    sample: [f64; 4],
    filtered: Option<[f64; 2]>,
    profile: u8,
    gas_on: bool,
}
impl Bme680 {
    pub fn new(clock: Arc<AtomicU64>, hz: u32) -> Self {
        let mut d = Self {
            s: SampleState::new(clock, hz),
            next: None,
            sample: [23., 50., 101325., 100000.],
            filtered: None,
            profile: 0,
            gas_on: false,
        };
        for (f, v) in [(0, 23.), (1, 50.), (2, 101325.), (17, 100000.)] {
            d.s.inputs[f] = v;
        }
        d.reset();
        d
    }
    fn reset(&mut self) {
        self.s.regs = [0; 256];
        self.s.readings = [f64::NAN; FIELD_COUNT];
        self.next = None;
        self.filtered = None;
        self.s.regs[0xd0] = 0x61;
        let mut c = [0u8; 42];
        for (i, v) in [
            (0, T2),
            (4, P[0]),
            (6, P[1]),
            (10, P[3]),
            (12, P[4]),
            (18, P[7]),
            (20, P[8]),
            (31, T1),
            (33, -2500.),
        ] {
            c[i..i + 2].copy_from_slice(&(v as i32 as u16).to_le_bytes());
        }
        for (i, v) in [
            (2, T3),
            (8, P[2]),
            (14, P[6]),
            (15, P[5]),
            (22, P[9]),
            (26, H[2]),
            (27, H[3]),
            (28, H[4]),
            (29, H[5]),
            (30, H[6]),
            (35, -30.),
            (36, 4.),
            (37, 20.),
            (39, 16.),
        ] {
            c[i] = v as i16 as u8;
        }
        c[23] = (H[1] as u16 >> 4) as u8;
        c[24] = ((H[1] as u16 as u8 & 15) << 4) | (H[0] as u8 & 15);
        c[25] = (H[0] as u16 >> 4) as u8;
        self.s.regs[0x8a..0xa1].copy_from_slice(&c[..23]);
        self.s.regs[0xe1..0xef].copy_from_slice(&c[23..37]);
        self.s.regs[..5].copy_from_slice(&c[37..]);
    }
    fn start(&mut self) {
        let os = self.s.regs[0x74];
        let us =
            (oversample(os >> 5) + oversample((os >> 2) & 7) + oversample(self.s.regs[0x72] & 7))
                * 1963
                + 5293;
        self.profile = (self.s.regs[0x71] & 15).min(9);
        self.gas_on = self.s.regs[0x71] & 0x10 != 0 && self.s.regs[0x70] & 8 == 0;
        let wait = self.s.regs[0x64 + self.profile as usize];
        let heat_us = if self.gas_on {
            (wait as u64 & 63) * (1u64 << ((wait >> 6) * 2)) * 1000
        } else {
            0
        };
        self.sample = [
            self.s.inputs[0],
            self.s.inputs[1],
            self.s.inputs[2],
            self.s.inputs[17],
        ];
        self.s.regs[0x1d] = 0x20 | self.profile;
        self.next = Some(self.s.now + self.s.ticks(us + heat_us));
    }
    fn complete(&mut self) {
        let mut t = inverse(self.sample[0], 0xfffff, false, |v| temp(v).0) as f64;
        let mut p = inverse(self.sample[2], 0xfffff, true, |v| press(v, temp(t).1)) as f64;
        let n = 1u32 << ((self.s.regs[0x75] >> 2) & 7);
        if let Some(old) = self.filtered {
            t = (old[0] * (n - 1) as f64 + t) / n as f64;
            p = (old[1] * (n - 1) as f64 + p) / n as f64;
        }
        self.filtered = Some([t, p]);
        let t = t.round() as u32;
        let p = p.round() as u32;
        let (tc, fine) = temp(t as f64);
        let h = inverse(self.sample[1], 0xffff, false, |v| humid(v, tc));
        for (reg, v) in [(0x1f, p), (0x22, t)] {
            self.s.regs[reg] = (v >> 12) as u8;
            self.s.regs[reg + 1] = (v >> 4) as u8;
            self.s.regs[reg + 2] = (v << 4) as u8;
        }
        self.s.regs[0x25..0x27].copy_from_slice(&(h as u16).to_be_bytes());
        self.s.readings[0] = tc;
        self.s.readings[1] = humid(h as f64, tc);
        self.s.readings[2] = press(p as f64, fine);
        self.s.readings[17] = f64::NAN;
        let mut best = (f64::INFINITY, 0, 0);
        for range in 0..16 {
            let raw = inverse(self.sample[3], 1023, true, |v| gas(v, range));
            let error = (gas(raw as f64, range) - self.sample[3]).abs();
            if error < best.0 {
                best = (error, range, raw);
            }
        }
        let (_, range, raw) = best;
        self.s.regs[0x2a] = (raw >> 2) as u8;
        self.s.regs[0x2b] =
            ((raw as u8 & 3) << 6) | range as u8 | if self.gas_on { 0x30 } else { 0 };
        if self.gas_on {
            self.s.readings[17] = gas(raw as f64, range);
        }
        self.s.regs[0x1d] = 0x80 | self.profile;
        self.s.regs[0x1e] = self.s.regs[0x1e].wrapping_add(1);
        self.s.regs[0x74] &= !3;
        self.s.publish(1);
    }
}
impl RegisterSensor for Bme680 {
    fn format(&self) -> WireFormat {
        WireFormat::Pairs
    }
    fn sync(&mut self) {
        self.s.time();
        if self.next.is_some_and(|t| t <= self.s.now) {
            self.next = None;
            self.complete();
        }
    }
    fn registers(&self) -> [u8; 256] {
        self.s.regs
    }
    fn write(&mut self, reg: u8, value: u16) -> bool {
        self.sync();
        let v = value as u8;
        if reg == 0xe0 && v == 0xb6 {
            self.reset();
            return true;
        }
        match reg {
            0x50..=0x6e | 0x70..=0x73 | 0x75 => {
                self.s.regs[reg as usize] = v;
                true
            }
            0x74 => {
                if v & 3 > 1 {
                    return false;
                }
                self.s.regs[0x74] = v;
                if v & 3 == 1 {
                    self.start();
                } else {
                    self.next = None;
                    self.s.regs[0x1d] &= !0x60;
                }
                true
            }
            _ => false,
        }
    }
    fn read_done(&mut self, reg: u8) {
        if reg == 0x2b {
            self.s.regs[0x1d] &= !0x80;
        }
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        self.sync();
        let range = match field {
            0 => (-40., 85.),
            1 => (0., 100.),
            2 => (30000., 110000.),
            17 => (1000., 10000000.),
            _ => return false,
        };
        if !value.is_finite() || !(range.0..=range.1).contains(&value) {
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
    fn forced_conversion_latches_inputs_and_waits_for_heater() {
        let clock = Arc::new(AtomicU64::new(0));
        let mut d = Bme680::new(clock.clone(), 1_000_000);
        assert_eq!(u16::from_le_bytes([d.s.regs[0x8e], d.s.regs[0x8f]]), 36477);
        assert_eq!(
            ((d.s.regs[0xe1] as u16) << 4) | (d.s.regs[0xe2] >> 4) as u16,
            362
        );
        assert!(d.set(0, -10.));
        assert!(d.set(1, 60.));
        assert!(d.set(2, 99000.));
        assert!(d.set(17, 152500.));
        d.write(0x71, 0x10);
        d.write(0x72, 2);
        d.write(0x64, 0x65);
        d.write(0x74, 0x8d);
        let deadline = (8 + 4 + 2) * 1963 + 5293 + 148000;
        assert_eq!(d.next, Some(deadline));
        assert!(d.set(17, 100000.));
        clock.store(deadline - 1, Ordering::Relaxed);
        assert_eq!(d.generation(), 0);
        clock.store(deadline, Ordering::Relaxed);
        assert_eq!(d.generation(), 1);
        for (field, expected, tolerance) in [
            (0, -10., 0.01),
            (1, 60., 0.01),
            (2, 99000., 0.3),
            (17, 152500., 200.),
        ] {
            assert!((d.value(field) - expected).abs() < tolerance);
        }
        assert_eq!(d.s.regs[0x74] & 3, 0);
        assert_eq!(d.s.regs[0x2b] & 0x30, 0x30);
        d.read_done(0x2b);
        assert_eq!(d.s.regs[0x1d] & 0x80, 0);
        d.write(0x71, 0);
        d.write(0x74, 0x8d);
        clock.store(d.next.unwrap(), Ordering::Relaxed);
        assert_eq!(d.generation(), 2);
        assert!(d.value(17).is_nan());
        d.write(0xe0, 0xb6);
        assert!(d.next.is_none());
        assert_eq!(d.s.inputs[17], 100000.);
        for bad in [f64::NAN, f64::INFINITY, 999., 10000001.] {
            assert!(!d.set(17, bad));
        }
    }
    #[test]
    fn bosch_interleaved_write_and_sequential_calibration_read() {
        let clock = Arc::new(AtomicU64::new(0));
        let cfg = SensorConfig {
            id: 0,
            sda: 4,
            scl: 5,
            address: 0x77,
            model: 10,
            shunt_milliohms: 0,
        };
        let state = Arc::new(Mutex::new(Sensor::new(cfg, clock, 1_000_000)));
        let mut wire = SensorI2c::new(state.clone());
        assert!(wire.start(false));
        for byte in [0x71, 0x10, 0x72, 2, 0x73, 0, 0x74, 0x8c, 0x75, 0] {
            assert!(wire.write(byte));
        }
        let regs = state.lock().unwrap().device.registers();
        assert_eq!(&regs[0x71..0x76], &[0x10, 2, 0, 0x8c, 0]);
        assert!(wire.start(false));
        assert!(wire.write(0x8e));
        assert!(wire.start(true));
        assert_eq!(u16::from_le_bytes([wire.read(), wire.read()]), 36477);
    }
}
