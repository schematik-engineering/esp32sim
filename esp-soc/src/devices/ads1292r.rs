//! ADS1292R digital conversion boundary, TI SBAS502C §§8.5–8.6.
//! Inputs are signed post-filter ADC counts and comparator results, not voltages or physiology.
//! CLKSEL is fixed to the nominal internal 512 kHz oscillator; external CLK is not attached.
use crate::SpiPins;

#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub model: u8,
    pub id: u8,
    pub sclk: u8,
    pub mosi: u8,
    pub miso: u8,
    pub cs: u8,
    pub drdy: u8,
    pub start: u8,
    pub reset: u8,
}
impl Config {
    pub fn pins(&self) -> [u8; 7] {
        [
            self.sclk, self.mosi, self.miso, self.cs, self.drdy, self.start, self.reset,
        ]
    }
    pub fn conflicts(&self, other: &Self) -> bool {
        self.id == other.id
            || self.pins().iter().enumerate().any(|(i, p)| {
                other
                    .pins()
                    .iter()
                    .enumerate()
                    .any(|(j, q)| p == q && !(i == j && matches!(i, 0 | 1 | 2 | 5 | 6)))
            })
    }
    pub fn valid(&self) -> bool {
        let pins = self.pins();
        self.model == 1
            && self.id < 16
            && pins
                .iter()
                .enumerate()
                .all(|(i, p)| *p < 49 && !pins[..i].contains(p))
    }
}
#[derive(Clone, Copy)]
enum Transfer {
    Idle,
    Count { reg: u8, write: bool },
    Registers { reg: u8, remaining: u8, write: bool },
    Data { position: usize, snapshot: bool },
}
pub struct Ads1292r {
    pub config: Config,
    hz: u64,
    now: u64,
    ready_at: u64,
    sample_not_before: u64,
    reset_low_since: Option<u64>,
    registers: [u8; 12],
    input: [i32; 2],
    lead: u8,
    output: [f64; 3],
    generation: u32,
    frame: [u8; 9],
    snapshot: [u8; 9],
    next_sample: Option<u64>,
    stopping: bool,
    standby: bool,
    continuous: bool,
    start: bool,
    reset_high: bool,
    drdy_high: bool,
    selected: bool,
    clock: bool,
    mosi: bool,
    miso: bool,
    bits: u8,
    input_byte: u8,
    transfer: Transfer,
}
impl Ads1292r {
    pub fn new(config: Config, hz: u64, now: u64) -> Self {
        let mut d = Self {
            config,
            hz,
            now,
            ready_at: now,
            sample_not_before: now,
            reset_low_since: None,
            registers: [0; 12],
            input: [0; 2],
            lead: 0,
            output: [f64::NAN; 3],
            generation: 0,
            frame: [0; 9],
            snapshot: [0; 9],
            next_sample: None,
            stopping: false,
            standby: false,
            continuous: true,
            start: false,
            reset_high: true,
            drdy_high: true,
            selected: false,
            clock: false,
            mosi: false,
            miso: false,
            bits: 0,
            input_byte: 0,
            transfer: Transfer::Idle,
        };
        d.reset();
        d.ready_at = now.saturating_add(d.ticks(16384));
        d
    }
    fn ticks(&self, clocks: u64) -> u64 {
        ((self.hz as u128 * clocks as u128).div_ceil(512000)).min(u64::MAX as u128) as u64
    }
    fn divider(&self) -> u64 {
        if self.registers[8] & 0x40 != 0 {
            16
        } else {
            4
        }
    }
    fn period(&self) -> Option<u64> {
        let rate = self.registers[1] & 7;
        (rate < 7).then(|| self.ticks((4096u64 >> rate) * self.divider() / 4))
    }
    fn settling(&self) -> Option<u64> {
        let rate = (self.registers[1] & 7) as usize;
        [4100, 2052, 1028, 516, 260, 132, 68]
            .get(rate)
            .map(|n| self.ticks(n * self.divider()))
    }
    fn reset(&mut self) {
        self.registers = [0x73, 2, 0x80, 0x10, 0, 0, 0, 0, 0, 0, 2, 0x0c];
        self.next_sample = None;
        self.stopping = false;
        self.standby = false;
        self.continuous = true;
        self.drdy_high = true;
        self.generation = 0;
        self.output = [f64::NAN; 3];
        self.frame = [0; 9];
        self.frame[0] = 0xc0;
        self.snapshot = self.frame;
        self.transfer = Transfer::Idle;
    }
    fn start_conversion(&mut self) {
        if self.reset_high && !self.standby {
            self.stopping = false;
            self.next_sample = self.settling().map(|t| {
                self.now
                    .max(self.ready_at)
                    .saturating_add(t)
                    .max(self.sample_not_before)
            });
            self.drdy_high = true;
        }
    }
    fn flags(&self) -> u8 {
        if self.registers[2] & 0x40 == 0 {
            0
        } else {
            self.lead & ((self.registers[7] & 15) | (self.registers[6] & 16))
        }
    }
    fn sample(&mut self) {
        let flags = self.flags();
        let status =
            0xc00000 | ((flags as u32) << 15) | (((self.read_register(11) & 3) as u32) << 13);
        self.frame[..3].copy_from_slice(&status.to_be_bytes()[1..]);
        for i in 0..2 {
            let v = if self.registers[4 + i] & 0x80 != 0 {
                0
            } else {
                self.input[i]
            };
            self.frame[3 + 3 * i..6 + 3 * i].copy_from_slice(&v.to_be_bytes()[1..]);
            self.output[i] = v as f64;
        }
        self.output[2] = flags as f64;
        self.generation = self.generation.wrapping_add(1);
        self.drdy_high = false;
    }
    pub fn set(&mut self, field: u32, value: f64) -> bool {
        if !value.is_finite() || value.fract() != 0. {
            return false;
        }
        match field {
            0 | 1 if (-8388608. ..=8388607.).contains(&value) => {
                self.input[field as usize] = value as i32
            }
            2 if (0. ..=31.).contains(&value) => self.lead = value as u8,
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
        self.next_sample.map(|next| {
            if !self.drdy_high && next.saturating_sub(self.ticks(4)) > self.now {
                next - self.ticks(4)
            } else {
                next
            }
        })
    }
    pub fn advance(&mut self, now: u64) {
        self.now = now;
        if let Some(next) = self.next_sample {
            if now >= next {
                let period = self.period().unwrap_or(1).max(1);
                let n = if self.stopping || self.registers[1] & 0x80 != 0 {
                    1
                } else {
                    (now - next) / period + 1
                };
                self.sample();
                self.generation = self.generation.wrapping_add((n - 1) as u32);
                self.next_sample = if self.stopping || self.registers[1] & 0x80 != 0 {
                    None
                } else {
                    Some(next.saturating_add(n.saturating_mul(period)))
                };
            }
            if self
                .next_sample
                .is_some_and(|t| now >= t.saturating_sub(self.ticks(4)))
            {
                self.drdy_high = true;
            }
        }
    }
    fn read_register(&self, reg: u8) -> u8 {
        if reg == 11 {
            let r = self.registers[11];
            (r & 12) | (r & (!(r >> 2)) & 3)
        } else if reg == 8 {
            (self.registers[8] & 0x40) | self.flags()
        } else {
            self.registers.get(reg as usize).copied().unwrap_or(0)
        }
    }
    fn write_register(&mut self, reg: u8, value: u8) {
        let mask = match reg {
            1 => 0x87,
            2 => 0xfb,
            3 => 0xfd,
            4 | 5 | 6 | 9 => 0xff,
            7 => 0x3f,
            8 => 0x40,
            10 => 0x87,
            11 => 0x0f,
            _ => return,
        };
        let fixed = match reg {
            2 => 0x80,
            3 => 0x10,
            9 => 2,
            10 => 1,
            _ => 0,
        };
        self.registers[reg as usize] = (value & mask) | fixed;
        if matches!(reg, 1 | 8 | 9 | 10) && self.next_sample.is_some() {
            self.start_conversion();
        }
    }
    fn output_byte(&self) -> u8 {
        match self.transfer {
            Transfer::Registers {
                reg, write: false, ..
            } => self.read_register(reg),
            Transfer::Data { position, snapshot } => {
                if snapshot {
                    self.snapshot[position % 9]
                } else {
                    self.frame[position % 9]
                }
            }
            Transfer::Idle if self.continuous => self.frame[0],
            _ => 0,
        }
    }
    fn exchange(&mut self, input: u8) -> u8 {
        if !self.reset_high || self.now < self.ready_at {
            return 255;
        }
        let output = if self.standby {
            255
        } else {
            self.output_byte()
        };
        match self.transfer {
            Transfer::Count { reg, write } => {
                self.transfer = Transfer::Registers {
                    reg,
                    write,
                    remaining: (input & 31) + 1,
                }
            }
            Transfer::Registers {
                reg,
                remaining,
                write,
            } => {
                if write {
                    self.write_register(reg, input);
                }
                self.transfer = if remaining == 1 {
                    Transfer::Idle
                } else {
                    Transfer::Registers {
                        reg: reg.wrapping_add(1),
                        remaining: remaining - 1,
                        write,
                    }
                };
            }
            Transfer::Data { position, snapshot } => {
                self.transfer = Transfer::Data {
                    position: (position + 1) % 9,
                    snapshot,
                }
            }
            Transfer::Idle => {
                if self.standby && input != 2 {
                    return output;
                }
                match input {
                    2 => {
                        self.standby = false;
                        self.sample_not_before = self.now.saturating_add(self.ticks(5120));
                        self.ready_at = self.now.saturating_add(self.ticks(4));
                        if self.start {
                            self.start_conversion();
                        }
                    }
                    4 => {
                        self.standby = true;
                        self.next_sample = None;
                        self.drdy_high = true;
                    }
                    6 => {
                        let wait = self.ticks(9 * self.divider());
                        self.reset();
                        self.ready_at = self.now.saturating_add(wait);
                        if self.start {
                            self.start_conversion();
                        }
                    }
                    8 if !self.start => {
                        if self.next_sample.is_none() {
                            self.start_conversion();
                        }
                    }
                    10 if !self.start => self.stopping = true,
                    0x10 => {
                        self.continuous = true;
                        self.ready_at = self.now.saturating_add(self.ticks(4));
                    }
                    0x11 => {
                        self.continuous = false;
                        self.ready_at = self.now.saturating_add(self.ticks(4));
                    }
                    0x12 if !self.continuous => {
                        self.snapshot = self.frame;
                        self.transfer = Transfer::Data {
                            position: 0,
                            snapshot: true,
                        };
                    }
                    0x20..=0x3f if !self.continuous => {
                        self.transfer = Transfer::Count {
                            reg: input & 31,
                            write: false,
                        }
                    }
                    0x40..=0x5f if !self.continuous => {
                        self.transfer = Transfer::Count {
                            reg: input & 31,
                            write: true,
                        }
                    }
                    _ if self.continuous => {
                        self.transfer = Transfer::Data {
                            position: 1,
                            snapshot: false,
                        }
                    }
                    _ => {}
                }
            }
        }
        output
    }
    fn select(&mut self, selected: bool) {
        if self.selected != selected {
            self.selected = selected;
            self.transfer = Transfer::Idle;
            self.bits = 0;
            self.input_byte = 0;
            self.miso = false;
        }
    }
    pub fn level(&self) -> Option<(u8, bool)> {
        (self.selected && self.reset_high && !self.standby && self.now >= self.ready_at)
            .then_some((self.config.miso, self.miso))
    }
    pub fn drdy(&self) -> (u8, bool) {
        (self.config.drdy, self.drdy_high)
    }
    pub fn drive(&mut self, enabled: u64, output: u64) {
        let c = self.config;
        for (pin, undriven) in [
            (c.reset, true),
            (c.start, false),
            (c.mosi, false),
            (c.cs, true),
            (c.sclk, false),
        ] {
            self.gpio(
                pin,
                if enabled & (1 << pin) == 0 {
                    undriven
                } else {
                    output & (1 << pin) != 0
                },
            );
        }
    }
    pub fn gpio(&mut self, pin: u8, high: bool) {
        if pin == self.config.reset && high != self.reset_high {
            self.reset_high = high;
            if !high {
                self.reset_low_since = Some(self.now);
                self.reset();
            } else {
                if self
                    .reset_low_since
                    .take()
                    .is_some_and(|t| self.now.saturating_sub(t) >= self.ticks(512 * self.divider()))
                {
                    self.sample_not_before = self.now.saturating_add(self.ticks(5120));
                }
                self.ready_at = self.now.saturating_add(self.ticks(18));
                if self.start {
                    self.start_conversion();
                }
            }
        }
        if pin == self.config.start && high != self.start {
            self.start = high;
            if high {
                self.start_conversion();
            } else {
                self.stopping = true;
            }
        }
        if pin == self.config.cs {
            self.select(!high);
        }
        if pin == self.config.mosi {
            self.mosi = high;
        }
        if pin == self.config.sclk {
            if high && !self.clock && self.selected {
                self.miso = self.output_byte() & (0x80 >> self.bits) != 0;
            }
            if !high && self.clock {
                self.drdy_high = true;
                if self.selected {
                    self.input_byte = (self.input_byte << 1) | u8::from(self.mosi);
                    self.bits += 1;
                    if self.bits == 8 {
                        self.exchange(self.input_byte);
                        self.bits = 0;
                        self.input_byte = 0;
                    }
                }
            }
            self.clock = high;
        }
    }
    pub fn spi(&mut self, pins: SpiPins, tx: &[u8], rx_len: usize) -> Option<Vec<u8>> {
        let c = self.config;
        if pins.sclk & (1 << c.sclk) == 0 {
            return None;
        }
        if !tx.is_empty() || rx_len > 0 {
            self.drdy_high = true;
        }
        if !(self.selected || pins.cs & (1 << c.cs) != 0) || pins.mosi & (1 << c.mosi) == 0 {
            return None;
        }
        let hardware_cs = !self.selected;
        if hardware_cs {
            self.select(true);
        }
        let mut rx = vec![255; rx_len];
        for i in 0..tx.len().max(rx_len) {
            let b = self.exchange(tx.get(i).copied().unwrap_or(0));
            if i < rx_len && pins.miso == Some(c.miso) {
                rx[i] = b;
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
    fn device() -> Ads1292r {
        let mut d = Ads1292r::new(
            Config {
                model: 1,
                id: 0,
                sclk: 1,
                mosi: 2,
                miso: 3,
                cs: 4,
                drdy: 5,
                start: 6,
                reset: 7,
            },
            512000,
            0,
        );
        d.advance(16384);
        d
    }
    fn pins() -> SpiPins {
        SpiPins {
            sclk: 2,
            mosi: 4,
            miso: Some(3),
            cs: 16,
        }
    }
    fn cmd(d: &mut Ads1292r, c: u8) {
        d.spi(pins(), &[c], 0);
        d.advance(d.now + 36);
    }
    fn write(d: &mut Ads1292r, r: u8, v: u8) {
        d.spi(pins(), &[0x40 | r, 0, v], 0);
    }
    fn read(d: &mut Ads1292r, r: u8) -> u8 {
        d.spi(pins(), &[0x20 | r, 0, 0], 3).unwrap()[2]
    }
    fn frame(d: &mut Ads1292r) -> Vec<u8> {
        d.spi(pins(), &[0xff; 9], 9).unwrap()
    }
    fn start(d: &mut Ads1292r) {
        cmd(d, 0x11);
        write(d, 1, 0);
        cmd(d, 0x10);
        d.gpio(6, true);
    }
    #[test]
    fn ads1292r_powerdown_standby_wake_floor_and_gpio_input_masks() {
        let mut d = device();
        cmd(&mut d, 0x11);
        write(&mut d, 11, 15);
        assert_eq!(read(&mut d, 11), 12);
        write(&mut d, 11, 3);
        assert_eq!(read(&mut d, 11), 3);
        d.gpio(6, true);
        cmd(&mut d, 4);
        assert!(d.next_sample.is_none());
        assert!(d.level().is_none());
        cmd(&mut d, 2);
        let wake = d.next_sample.unwrap();
        assert_eq!(wake, d.sample_not_before);
        d.advance(wake - 1);
        assert_eq!(d.generation(), 0);
        d.advance(wake);
        assert_eq!(d.generation(), 1);
        d.gpio(7, false);
        d.advance(d.now + 5120);
        assert_eq!(frame(&mut d), vec![255; 9]);
        d.gpio(7, true);
        assert_eq!(d.next_sample.unwrap(), d.sample_not_before);
        let ready = d.next_sample.unwrap();
        d.advance(ready - 1);
        assert_eq!(d.generation(), 0);
        d.advance(ready);
        assert_eq!(d.generation(), 1);
    }
    #[test]
    fn ads1292r_clock_divider_changes_fmod_without_changing_the_fixed_oscillator() {
        let mut d = device();
        cmd(&mut d, 0x11);
        write(&mut d, 1, 0);
        assert_eq!(d.period(), Some(4096));
        assert_eq!(d.settling(), Some(16400));
        write(&mut d, 8, 0x40);
        assert_eq!(read(&mut d, 8), 0x40);
        assert_eq!(d.period(), Some(16384));
        assert_eq!(d.settling(), Some(65600));
        assert_eq!(d.ticks(512000), 512000);
        write(&mut d, 8, 0);
        assert_eq!(d.period(), Some(4096));
    }
    #[test]
    fn ads1292r_identity_register_masks_reset_busy_and_partial_transactions() {
        let mut d = device();
        cmd(&mut d, 0x11);
        assert_eq!(read(&mut d, 0), 0x73);
        assert_eq!(read(&mut d, 1), 2);
        assert_eq!(read(&mut d, 10), 2);
        assert_eq!(read(&mut d, 11), 12);
        write(&mut d, 0, 0);
        assert_eq!(read(&mut d, 0), 0x73);
        write(&mut d, 2, 255);
        assert_eq!(read(&mut d, 2), 251);
        d.gpio(4, false);
        d.spi(SpiPins { cs: 0, ..pins() }, &[0x41, 0], 0);
        d.gpio(4, true);
        assert_eq!(read(&mut d, 1), 2);
        d.spi(pins(), &[6], 0);
        let ready = d.ready_at;
        assert_eq!(ready - d.now, 36);
        d.advance(ready - 1);
        d.spi(pins(), &[0x11], 0);
        assert!(d.continuous);
        d.advance(ready);
        cmd(&mut d, 0x11);
        assert_eq!(read(&mut d, 2), 0x80);
        d.gpio(7, false);
        assert_eq!(d.generation(), 0);
        assert!(d.value(0).is_nan());
        d.advance(d.now + 512000);
        d.gpio(7, true);
        assert_eq!(d.ready_at - d.now, 18);
    }
    #[test]
    fn ads1292r_signed_two_channels_status_and_first_conversion_boundaries() {
        let mut d = device();
        d.set(0, -8388608.);
        d.set(1, 8388607.);
        d.set(2, 31.);
        start(&mut d);
        let first = d.next_sample.unwrap();
        assert_eq!(first - d.now, 16400);
        d.advance(first - 1);
        assert!(d.drdy().1);
        assert!(d.value(0).is_nan());
        d.advance(first);
        assert!(!d.drdy().1);
        assert_eq!(frame(&mut d), [0xc0, 0, 0, 0x80, 0, 0, 0x7f, 0xff, 0xff]);
        assert!(d.drdy().1);
        assert_eq!(d.value(2), 0.);
        cmd(&mut d, 0x11);
        write(&mut d, 2, 0xe0);
        write(&mut d, 7, 5);
        write(&mut d, 6, 16);
        cmd(&mut d, 0x10);
        d.advance(d.next_sample.unwrap());
        assert_eq!(frame(&mut d)[..3], [0xca, 0x80, 0]);
        assert_eq!(d.value(2), 21.);
        cmd(&mut d, 0x11);
        write(&mut d, 7, 0);
        write(&mut d, 6, 0);
        write(&mut d, 4, 0x81);
        cmd(&mut d, 0x10);
        d.advance(d.next_sample.unwrap());
        assert_eq!(frame(&mut d), [0xc0, 0, 0, 0, 0, 0, 0x7f, 0xff, 0xff]);
    }
    #[test]
    fn ads1292r_all_rates_stop_single_shot_and_unread_update_pulse() {
        for rate in 0..7 {
            let mut d = device();
            cmd(&mut d, 0x11);
            write(&mut d, 1, rate);
            d.gpio(6, true);
            let first = d.next_sample.unwrap();
            d.advance(first);
            assert_eq!(d.generation(), 1);
            let next = d.next_sample.unwrap();
            assert_eq!(next - first, 4096 >> rate);
            d.advance(next - 5);
            assert!(!d.drdy().1);
            d.advance(next - 4);
            assert!(d.drdy().1);
            d.advance(next);
            assert!(!d.drdy().1);
            d.gpio(6, false);
            d.advance(d.next_sample.unwrap());
            assert!(d.next_sample.is_none());
        }
        let mut d = device();
        cmd(&mut d, 0x11);
        write(&mut d, 1, 0x80);
        cmd(&mut d, 8);
        d.advance(d.next_sample.unwrap());
        assert!(d.next_sample.is_none());
        let generation = d.generation();
        d.advance(d.now + 512000);
        assert_eq!(d.generation(), generation);
        cmd(&mut d, 8);
        assert!(d.next_sample.is_some());
        cmd(&mut d, 10);
        d.advance(d.next_sample.unwrap());
        assert!(d.next_sample.is_none());
    }
    #[test]
    fn ads1292r_rdata_snapshot_repeat_and_continuous_register_guard() {
        let mut d = device();
        d.set(0, -1.);
        start(&mut d);
        d.advance(d.next_sample.unwrap());
        assert_eq!(read(&mut d, 0), 0);
        cmd(&mut d, 0x11);
        assert_eq!(read(&mut d, 0), 0x73);
        d.gpio(4, false);
        let p = SpiPins { cs: 0, ..pins() };
        d.spi(p, &[0x12], 0);
        d.set(0, 123.);
        d.advance(d.next_sample.unwrap());
        let b = d.spi(p, &[0; 18], 18).unwrap();
        assert_eq!(b[..9], b[9..]);
        assert_eq!(b[3..6], [255, 255, 255]);
        d.gpio(4, true);
        assert_eq!(d.value(0), 123.);
    }
    #[test]
    fn ads1292r_shared_start_preserves_config_separate_reset_and_shared_clock_clears_drdy() {
        let (mut a, mut b) = (device(), device());
        b.config.id = 1;
        b.config.cs = 8;
        b.config.drdy = 9;
        b.config.reset = 10;
        start(&mut a);
        a.set(0, 12.);
        a.set(1, -34.);
        b.gpio(10, false);
        b.advance(b.now + 51200);
        b.gpio(10, true);
        a.gpio(6, false);
        a.gpio(6, true);
        assert_eq!(a.registers[1], 0);
        let ready = a.next_sample.unwrap();
        a.advance(ready);
        assert!(!a.drdy().1);
        a.spi(SpiPins { cs: 256, ..pins() }, &[0xff; 9], 9);
        assert!(a.drdy().1);
        assert_eq!(a.value(0), 12.);
        assert_eq!(a.value(1), -34.);
        b.set(0, -56.);
        b.set(1, 78.);
        b.gpio(6, true);
        b.advance(b.next_sample.unwrap());
        assert_eq!(b.value(0), -56.);
        assert_eq!(a.value(0), 12.);
    }
    fn gpio_byte(d: &mut Ads1292r, b: u8) -> u8 {
        let mut r = 0;
        for bit in (0..8).rev() {
            d.gpio(2, b & (1 << bit) != 0);
            d.gpio(1, true);
            r = (r << 1) | u8::from(d.level().unwrap().1);
            d.gpio(1, false);
        }
        r
    }
    #[test]
    fn ads1292r_mode_one_gpio_wrong_routes_and_integer_input_validation() {
        let mut d = device();
        cmd(&mut d, 0x11);
        d.gpio(4, false);
        for b in [0x41, 0, 0] {
            gpio_byte(&mut d, b);
        }
        d.gpio(4, true);
        assert_eq!(read(&mut d, 1), 0);
        d.gpio(4, false);
        gpio_byte(&mut d, 0x20);
        gpio_byte(&mut d, 0);
        assert_eq!(gpio_byte(&mut d, 0), 0x73);
        d.gpio(4, true);
        assert!(d.level().is_none());
        for p in [
            SpiPins { sclk: 4, ..pins() },
            SpiPins { mosi: 8, ..pins() },
            SpiPins { cs: 256, ..pins() },
        ] {
            assert!(d.spi(p, &[0x20, 0, 0], 3).is_none());
        }
        assert_eq!(
            d.spi(
                SpiPins {
                    miso: Some(9),
                    ..pins()
                },
                &[0x20, 0, 0],
                3
            ),
            Some(vec![255; 3])
        );
        for v in [f64::NAN, f64::INFINITY, 8388608., -8388609., 0.5] {
            assert!(!d.set(0, v));
            assert!(!d.set(1, v));
        }
        for v in [-1., 32., 0.5] {
            assert!(!d.set(2, v));
        }
        assert!(!d.set(3, 0.));
        assert!(d.value(3).is_nan());
    }
}
