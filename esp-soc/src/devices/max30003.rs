//! MAX30003 digital FIFO/RTOR boundary. Inputs are post-filter ADC counts and already-detected
//! R events; analog gain, calibration waveforms, filtering and QRS detection are outside this model.
//! Register and FIFO layout: ADI MAX30003 Rev.3, tables 7–33 and SPI interface pp.22–23.
use crate::SpiPins;
use std::collections::VecDeque;

#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub model: u8,
    pub id: u8,
    pub sclk: u8,
    pub mosi: u8,
    pub miso: u8,
    pub cs: u8,
}
impl Config {
    pub fn valid(&self) -> bool {
        let pins = [self.sclk, self.mosi, self.miso, self.cs];
        self.model == 1
            && self.id < 16
            && pins
                .iter()
                .enumerate()
                .all(|(i, p)| *p < 49 && !pins[..i].contains(p))
    }
}

pub struct Max30003 {
    pub config: Config,
    hz: u64,
    now: u64,
    counts: i32,
    bpm: f64,
    registers: [u32; 128],
    fifo: VecDeque<i32>,
    overflow: bool,
    rrint: bool,
    next_sample: Option<u64>,
    next_r: Option<u64>,
    previous_r: bool,
    generation: u32,
    output: [f64; 3],
    initialized: bool,
    selected: bool,
    clock: bool,
    mosi: bool,
    miso: bool,
    bits: u8,
    input: u8,
    command: Option<u8>,
    byte: u8,
    word: u32,
    response: u32,
}
impl Max30003 {
    pub fn new(config: Config, hz: u64, now: u64) -> Self {
        let mut d = Self {
            config,
            hz,
            now,
            counts: 0,
            bpm: 0.,
            registers: [0; 128],
            fifo: VecDeque::with_capacity(32),
            overflow: false,
            rrint: false,
            next_sample: None,
            next_r: None,
            previous_r: false,
            generation: 0,
            output: [f64::NAN; 3],
            initialized: false,
            selected: false,
            clock: false,
            mosi: false,
            miso: false,
            bits: 0,
            input: 0,
            command: None,
            byte: 0,
            word: 0,
            response: 0,
        };
        d.reset();
        d
    }
    fn reset(&mut self) {
        self.registers = [0; 128];
        for (reg, value) in [
            (4, 0x780004),
            (5, 0x3f0000),
            (0xf, 0x503000),
            (0x10, 4),
            (0x12, 0x4800),
            (0x14, 0x300000),
            (0x15, 0x805000),
            (0x1d, 0x3f2300),
            (0x1e, 0x202400),
        ] {
            self.registers[reg] = value;
        }
        self.fifo.clear();
        self.overflow = false;
        self.rrint = false;
        self.next_sample = None;
        self.next_r = None;
        self.previous_r = false;
        self.generation = 0;
        self.output = [f64::NAN; 3];
        self.initialized = false;
    }
    fn master(&self) -> f64 {
        match (self.registers[0x10] >> 20) & 3 {
            0 => 32768.,
            1 | 2 => 32000.,
            _ => 32768. * 40. / 41.,
        }
    }
    fn sample_period(&self) -> Option<u64> {
        if self.registers[0x10] & (1 << 19) == 0 {
            return None;
        }
        let rate = (self.registers[0x15] >> 22) & 3;
        let divisor = match ((self.registers[0x10] >> 20) & 3, rate) {
            (0 | 1, 0) => 64.,
            (0 | 1, 1) => 128.,
            (_, 2) => {
                if self.registers[0x10] & (2 << 20) != 0 {
                    160.
                } else {
                    256.
                }
            }
            _ => return None,
        };
        Some((self.hz as f64 * divisor / self.master()).ceil().max(1.) as u64)
    }
    fn r_ticks(&self) -> Option<u32> {
        if self.bpm == 0.
            || self.registers[0x10] & (1 << 19) == 0
            || self.registers[0x1d] & (1 << 15) == 0
        {
            return None;
        }
        let ticks = (60. * self.master() / (256. * self.bpm)).round();
        (1. ..=16383.).contains(&ticks).then_some(ticks as u32)
    }
    fn r_period(&self) -> Option<u64> {
        self.r_ticks().map(|ticks| {
            (self.hz as f64 * ticks as f64 * 256. / self.master())
                .ceil()
                .max(1.) as u64
        })
    }
    fn restart_r(&mut self) {
        self.previous_r = false;
        self.next_r = self
            .r_period()
            .map(|period| self.now.saturating_add(period));
    }
    fn synch(&mut self) {
        self.fifo.clear();
        self.overflow = false;
        self.rrint = false;
        self.registers[0x25] = 0;
        self.output = [f64::NAN; 3];
        self.generation = 0;
        self.next_sample = self
            .sample_period()
            .map(|period| self.now.saturating_add(period));
        self.restart_r();
    }
    pub fn set(&mut self, field: u32, value: f64) -> bool {
        if !value.is_finite() {
            return false;
        }
        match field {
            0 if value.fract() == 0. && (-131072. ..=131071.).contains(&value) => {
                self.counts = value as i32
            }
            1 if value == 0. || (7680. / 16383. ..=7680.).contains(&value) => {
                self.bpm = value;
                self.restart_r();
            }
            _ => return false,
        }
        true
    }
    pub fn generation(&self) -> u32 {
        self.generation
    }
    pub fn value(&self, field: u32) -> f64 {
        self.output.get(field as usize).copied().unwrap_or(f64::NAN)
    }
    pub fn next_deadline(&self) -> Option<u64> {
        self.next_sample.into_iter().chain(self.next_r).min()
    }
    pub fn advance(&mut self, now: u64) {
        self.now = now;
        if let (Some(next), Some(period)) = (self.next_sample, self.sample_period()) {
            if next <= now {
                let n = (now - next) / period + 1;
                self.output[0] = self.counts as f64;
                self.generation = self.generation.wrapping_add(n as u32);
                if !self.overflow {
                    for _ in 0..n.min(33) {
                        if self.fifo.len() == 32 {
                            self.overflow = true;
                            break;
                        }
                        self.fifo.push_back(self.counts);
                    }
                }
                self.next_sample = Some(next.saturating_add(n.saturating_mul(period)));
            }
        }
        if let (Some(next), Some(period), Some(ticks)) =
            (self.next_r, self.r_period(), self.r_ticks())
        {
            if next <= now {
                let n = (now - next) / period + 1;
                if self.previous_r || n >= 2 {
                    self.registers[0x25] = ticks << 10;
                    self.rrint = true;
                    self.output[2] = ticks as f64 * 256000. / self.master();
                    self.output[1] = 60000. / self.output[2];
                    self.generation = self.generation.wrapping_add(1);
                }
                self.previous_r = true;
                self.next_r = Some(next.saturating_add(n.saturating_mul(period)));
            }
        }
    }
    fn status(&self) -> u32 {
        (u32::from(self.fifo.len() >= (((self.registers[4] >> 19) & 31) + 1) as usize) << 23)
            | (u32::from(self.overflow) << 22)
            | (u32::from(self.rrint) << 10)
    }
    fn read(&self, reg: u8) -> u32 {
        match reg {
            0 | 0x7f => 0,
            1 => self.status(),
            0xf if !self.initialized => 0,
            0x20 | 0x21 => {
                if self.overflow {
                    7 << 3
                } else if let Some(value) = self.fifo.front() {
                    ((*value as u32 & 0x3ffff) << 6) | if self.fifo.len() == 1 { 2 << 3 } else { 0 }
                } else {
                    6 << 3
                }
            }
            0x15 => {
                let mut value = self.registers[0x15];
                let rate = (value >> 22) & 3;
                let filter = (value >> 12) & 3;
                if filter != 0 && ((rate == 1 && filter == 3) || (rate == 2 && filter > 1)) {
                    value = (value & !0x3000) | 0x1000;
                }
                value
            }
            _ => self.registers[reg as usize],
        }
    }
    fn read_done(&mut self, reg: u8) {
        if matches!(reg, 0x20 | 0x21) && !self.overflow {
            self.fifo.pop_front();
        }
        let clear = (self.registers[4] >> 4) & 3;
        if (reg == 1 && clear == 0) || (reg == 0x25 && clear == 1) {
            self.rrint = false;
        }
    }
    fn write(&mut self, reg: u8, value: u32) {
        match reg {
            8 if value == 0 => self.reset(),
            9 if value == 0 => self.synch(),
            10 if value == 0 => {
                self.fifo.clear();
                self.overflow = false;
            }
            2 | 3 | 4 | 5 | 0x10 | 0x12 | 0x14 | 0x15 | 0x1d | 0x1e => {
                self.registers[reg as usize] = value;
                if matches!(reg, 0x10 | 0x15) {
                    self.next_sample = self
                        .sample_period()
                        .map(|period| self.now.saturating_add(period));
                }
                if matches!(reg, 0x10 | 0x1d) {
                    self.restart_r();
                }
            }
            _ => {}
        }
    }
    fn output_byte(&self) -> u8 {
        if self.command.is_some_and(|c| c & 1 != 0) && self.byte < 3 {
            (self.response >> (16 - self.byte * 8)) as u8
        } else {
            0
        }
    }
    fn exchange(&mut self, input: u8) -> u8 {
        let output = self.output_byte();
        let Some(command) = self.command else {
            self.command = Some(input);
            self.response = self.read(input >> 1);
            self.byte = 0;
            self.word = 0;
            return 0;
        };
        if self.byte >= 3 {
            return 0;
        }
        self.word = (self.word << 8) | input as u32;
        self.byte += 1;
        if self.byte == 3 {
            let reg = command >> 1;
            if command & 1 != 0 {
                self.read_done(reg);
            } else {
                self.write(reg, self.word);
            }
            if !(reg == 8 && command & 1 == 0 && self.word == 0) {
                self.initialized = true;
            }
            if command == 0x41 {
                self.byte = 0;
                self.response = self.read(0x20);
            }
        }
        output
    }
    fn select(&mut self, selected: bool) {
        if self.selected != selected {
            self.selected = selected;
            self.command = None;
            self.byte = 0;
            self.word = 0;
            self.response = 0;
            self.bits = 0;
            self.input = 0;
            self.miso = false;
        }
    }
    pub fn level(&self) -> Option<(u8, bool)> {
        self.selected.then_some((self.config.miso, self.miso))
    }
    pub fn drive(&mut self, enabled: u64, output: u64) {
        let c = self.config;
        self.gpio(
            c.mosi,
            enabled & (1 << c.mosi) != 0 && output & (1 << c.mosi) != 0,
        );
        self.gpio(
            c.cs,
            enabled & (1 << c.cs) == 0 || output & (1 << c.cs) != 0,
        );
        self.gpio(
            c.sclk,
            enabled & (1 << c.sclk) != 0 && output & (1 << c.sclk) != 0,
        );
    }
    pub fn gpio(&mut self, pin: u8, high: bool) {
        if pin == self.config.cs {
            self.select(!high);
        }
        if pin == self.config.mosi {
            self.mosi = high;
        }
        if pin == self.config.sclk {
            if self.selected && high && !self.clock {
                self.input = (self.input << 1) | u8::from(self.mosi);
                self.bits += 1;
                if self.bits == 8 {
                    self.exchange(self.input);
                    self.bits = 0;
                    self.input = 0;
                }
            }
            if self.selected && !high && self.clock {
                self.miso = self.output_byte() & (0x80 >> self.bits) != 0;
            }
            self.clock = high;
        }
    }
    pub fn spi(&mut self, pins: SpiPins, tx: &[u8], rx_len: usize) -> Option<Vec<u8>> {
        let c = self.config;
        if !(self.selected || pins.cs & (1 << c.cs) != 0)
            || pins.sclk & (1 << c.sclk) == 0
            || pins.mosi & (1 << c.mosi) == 0
        {
            return None;
        }
        let hardware_cs = !self.selected;
        if hardware_cs {
            self.select(true);
        }
        let mut rx = vec![255; rx_len];
        for i in 0..tx.len().max(rx_len) {
            let byte = self.exchange(tx.get(i).copied().unwrap_or(0));
            if i < rx_len && pins.miso == Some(c.miso) {
                rx[i] = byte;
            }
        }
        if hardware_cs {
            self.select(false);
        }
        Some(rx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn device() -> Max30003 {
        Max30003::new(
            Config {
                model: 1,
                id: 0,
                sclk: 1,
                mosi: 2,
                miso: 3,
                cs: 4,
            },
            32_768_000,
            0,
        )
    }
    fn pins() -> SpiPins {
        SpiPins {
            sclk: 2,
            mosi: 4,
            miso: Some(3),
            cs: 16,
        }
    }
    fn write(d: &mut Max30003, reg: u8, value: u32) {
        d.spi(
            pins(),
            &[
                reg << 1,
                (value >> 16) as u8,
                (value >> 8) as u8,
                value as u8,
            ],
            0,
        );
    }
    fn read(d: &mut Max30003, reg: u8) -> u32 {
        let bytes = d.spi(pins(), &[reg << 1 | 1, 0, 0, 0], 4).unwrap();
        ((bytes[1] as u32) << 16) | ((bytes[2] as u32) << 8) | bytes[3] as u32
    }
    fn start(d: &mut Max30003) {
        write(d, 0x10, 0x081007);
        write(d, 0x1d, 0x3fc600);
        write(d, 9, 0);
    }
    #[test]
    fn max30003_defaults_identity_first_command_and_reset() {
        let mut d = device();
        assert_eq!(read(&mut d, 15), 0);
        assert_eq!(read(&mut d, 15), 0x503000);
        for (reg, value) in [
            (4, 0x780004),
            (5, 0x3f0000),
            (0x10, 4),
            (0x12, 0x4800),
            (0x14, 0x300000),
            (0x15, 0x805000),
            (0x1d, 0x3f2300),
            (0x1e, 0x202400),
        ] {
            assert_eq!(read(&mut d, reg), value);
        }
        start(&mut d);
        d.advance(256_000);
        assert_eq!(d.value(0), 0.);
        write(&mut d, 8, 1);
        assert_eq!(d.value(0), 0.);
        write(&mut d, 8, 0);
        assert_eq!(d.generation(), 0);
        assert!(d.value(0).is_nan());
        assert_eq!(read(&mut d, 15), 0);
        assert_eq!(read(&mut d, 15), 0x503000);
        assert_eq!(read(&mut d, 0x21), 48);
    }
    #[test]
    fn max30003_signed_fifo_sample_boundary_last_empty() {
        let mut d = device();
        d.set(0, -131072.);
        start(&mut d);
        d.advance(255_999);
        assert_eq!(read(&mut d, 0x21), 48);
        d.advance(256_000);
        assert_eq!(read(&mut d, 0x21), 0x800010);
        assert_eq!(read(&mut d, 0x21), 48);
        d.set(0, 131071.);
        d.advance(768_000);
        assert_eq!(read(&mut d, 0x21), 0x7fffc0);
        assert_eq!(read(&mut d, 0x21), 0x7fffd0);
    }
    #[test]
    fn max30003_fifo_depth_overflow_and_fifo_reset_preserve_phase() {
        let mut d = device();
        start(&mut d);
        d.advance(32 * 256_000);
        assert_eq!(read(&mut d, 1) & 0xc00000, 0x800000);
        d.advance(33 * 256_000);
        assert_eq!(read(&mut d, 1) & 0xc00000, 0xc00000);
        assert_eq!(read(&mut d, 0x21), 56);
        write(&mut d, 10, 1);
        assert_eq!(read(&mut d, 0x21), 56);
        write(&mut d, 10, 0);
        assert_eq!(read(&mut d, 0x21), 48);
        d.advance(34 * 256_000 - 1);
        assert_eq!(read(&mut d, 0x21), 48);
        d.advance(34 * 256_000);
        assert_eq!(read(&mut d, 0x21), 16);
    }
    #[test]
    fn max30003_partial_reads_writes_and_burst_commit_at_word_end() {
        let mut d = device();
        start(&mut d);
        d.set(0, -7.);
        d.advance(512_000);
        d.gpio(4, false);
        let p = SpiPins { cs: 0, ..pins() };
        d.spi(p, &[0x43, 0, 0], 3);
        assert_eq!(d.fifo.len(), 2);
        d.gpio(4, true);
        assert_eq!(read(&mut d, 0x21), 0xfffe40);
        let bytes = d.spi(pins(), &[0x41, 0, 0, 0, 0, 0, 0], 7).unwrap();
        assert_eq!(&bytes[1..], &[255, 254, 80, 0, 0, 48]);
        d.gpio(4, false);
        d.spi(p, &[0x20, 0, 0], 0);
        d.gpio(4, true);
        assert_eq!(read(&mut d, 0x10), 0x081007);
    }
    #[test]
    fn max30003_all_sample_rates_and_disabled_channel() {
        for (master, rate, hz) in [
            (0, 0, 512.),
            (0, 1, 256.),
            (0, 2, 128.),
            (1, 0, 500.),
            (1, 1, 250.),
            (1, 2, 125.),
            (2, 2, 200.),
            (3, 2, 32768. * 40. / 41. / 160.),
        ] {
            let mut d = device();
            write(&mut d, 0x10, (master << 20) | (1 << 19));
            write(&mut d, 0x15, rate << 22);
            write(&mut d, 9, 0);
            let period = (d.hz as f64 / hz).ceil() as u64;
            d.advance(period - 1);
            assert_eq!(read(&mut d, 0x21), 48);
            d.advance(period);
            assert_eq!(read(&mut d, 0x21), 16);
            write(&mut d, 0x10, 0);
            d.advance(period * 100);
            assert_eq!(read(&mut d, 0x21), 48);
        }
    }
    #[test]
    fn max30003_rtor_two_events_quantization_clear_and_no_events() {
        let mut d = device();
        d.set(1, 72.);
        start(&mut d);
        let period = 107 * 256_000;
        d.advance(period);
        assert_eq!(read(&mut d, 0x25), 0);
        assert!(d.value(1).is_nan());
        d.advance(2 * period - 1);
        assert_eq!(read(&mut d, 0x25), 0);
        d.advance(2 * period);
        assert_eq!(read(&mut d, 0x25), 107 << 10);
        assert_eq!(d.value(2), 835.9375);
        assert!((d.value(1) - 71.77570093457943).abs() < 1e-9);
        assert_eq!(read(&mut d, 1) & 1024, 1024);
        assert_eq!(read(&mut d, 1) & 1024, 0);
        d.set(1, 0.);
        d.advance(10 * period);
        assert_eq!(read(&mut d, 0x25), 107 << 10);
        write(&mut d, 9, 0);
        assert_eq!(read(&mut d, 0x25), 0);
        assert!(d.value(1).is_nan());
    }
    #[test]
    fn max30003_rtor_enable_clock_and_clear_on_rtor() {
        let mut d = device();
        d.set(1, 60.);
        write(&mut d, 0x10, 0x080000);
        d.advance(d.hz * 4);
        assert_eq!(read(&mut d, 0x25), 0);
        write(&mut d, 0x1d, 0x3f8000);
        write(&mut d, 4, 0x10);
        let next = d.now + d.hz * 2;
        d.advance(next);
        assert_eq!(read(&mut d, 1) & 1024, 1024);
        assert_eq!(read(&mut d, 0x25), 128 << 10);
        assert_eq!(read(&mut d, 1) & 1024, 0);
        write(&mut d, 0x10, 0x180000);
        write(&mut d, 9, 0);
        let next = d.now + d.hz * 2;
        d.advance(next);
        assert_eq!(read(&mut d, 0x25), 125 << 10);
        assert_eq!(d.value(1), 60.);
    }
    #[test]
    fn max30003_input_boundaries_and_independent_instances() {
        let (mut a, mut b) = (device(), device());
        b.config.cs = 5;
        for v in [f64::NAN, f64::INFINITY, -131073., 131072., 1.5] {
            assert!(!a.set(0, v));
        }
        for v in [f64::NAN, f64::INFINITY, -1., 0.4, 7681.] {
            assert!(!a.set(1, v));
        }
        assert!(a.set(1, 7680. / 16383.));
        assert!(a.set(1, 7680.));
        assert!(!a.set(2, 1.));
        assert!(a.value(9).is_nan());
        a.set(0, -1.);
        b.set(0, 123.);
        start(&mut a);
        write(&mut b, 0x10, 0x080000); // wrong CS: no configuration
        a.advance(256_000);
        b.advance(256_000);
        assert_eq!(a.value(0), -1.);
        assert!(b.value(0).is_nan());
        assert!(a
            .spi(SpiPins { sclk: 4, ..pins() }, &[31, 0, 0, 0], 4)
            .is_none());
        assert!(a
            .spi(SpiPins { mosi: 8, ..pins() }, &[31, 0, 0, 0], 4)
            .is_none());
        assert_eq!(
            a.spi(
                SpiPins {
                    miso: Some(2),
                    ..pins()
                },
                &[31, 0, 0, 0],
                4
            ),
            Some(vec![255; 4])
        );
    }
    fn gpio_byte(d: &mut Max30003, input: u8) -> u8 {
        let mut out = 0;
        for bit in (0..8).rev() {
            d.gpio(2, input & (1 << bit) != 0);
            d.gpio(1, true);
            out = (out << 1) | u8::from(d.level().unwrap().1);
            d.gpio(1, false);
        }
        out
    }
    #[test]
    fn max30003_mode_zero_gpio_and_hardware_share_transactions() {
        let mut d = device();
        d.gpio(4, false);
        for b in [0x20, 8, 0, 4] {
            gpio_byte(&mut d, b);
        }
        d.gpio(4, true);
        assert_eq!(read(&mut d, 0x10), 0x080004);
        d.gpio(4, false);
        gpio_byte(&mut d, 0x21);
        let bytes = [
            gpio_byte(&mut d, 0),
            gpio_byte(&mut d, 0),
            gpio_byte(&mut d, 0),
        ];
        d.gpio(4, true);
        assert_eq!(bytes, [8, 0, 4]);
        assert_eq!(d.level(), None);
        let mut d = device();
        d.drive(0, 0);
        for _ in 0..32 {
            d.drive(2, 2);
            d.drive(2, 0);
        }
        assert!(!d.initialized);
        assert!(!d.selected);
    }
}
