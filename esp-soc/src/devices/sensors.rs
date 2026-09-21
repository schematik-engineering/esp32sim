use esp_periph::i2c::I2cDevice;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};

#[derive(Clone, Copy, Debug)]
pub struct SensorConfig {
    pub id: u8,
    pub sda: u8,
    pub scl: u8,
    pub address: u8,
    pub model: u8,
    pub shunt_milliohms: u16,
}
impl SensorConfig {
    pub fn valid(self) -> bool {
        self.sda < 49
            && self.scl < 49
            && self.sda != self.scl
            && (matches!(self.model, 5 | 38) || self.shunt_milliohms == 0)
            && match self.model {
                1 | 2 | 10 => matches!(self.address, 0x76 | 0x77),
                3 => matches!(self.address, 0x23 | 0x5c),
                4 => matches!(self.address, 0x68 | 0x69),
                5 | 38 => (0x40..=0x4f).contains(&self.address),
                6 | 29 => self.address == 0x68,
                7 => self.address == 0x44,
                8 => (0x48..=0x4b).contains(&self.address),
                9 => self.address == 0x5a,
                11 => (0x40..=0x4f).contains(&self.address),
                12 => (0x48..=0x4b).contains(&self.address),
                13 => (0x60..=0x67).contains(&self.address),
                14 => self.address == 0x36,
                15 => matches!(self.address, 0x6a | 0x6b),
                16 => matches!(self.address, 0x28 | 0x29),
                17 => (0x60..=0x67).contains(&self.address),
                18 => (0x44..=0x47).contains(&self.address),
                19 => self.address == 0x62,
                20 | 23..=25 => self.address == 0x36,
                21 => self.address == 0x2a,
                22 => self.address == 0x29,
                26 => matches!(self.address, 0x44 | 0x45),
                27 => self.address == 0x70,
                28 => self.address == 0x10,
                30 => self.address == 0x38,
                31 => (0x18..=0x1f).contains(&self.address),
                32 => self.address == 0x58,
                33 => self.address == 0x59,
                34 => self.address == 0x1e,
                35 => matches!(self.address, 0x1c | 0x1e),
                36 => matches!(self.address, 0x76 | 0x77),
                37 => matches!(self.address, 0x5c | 0x5d),
                39 => matches!(self.address, 0x53 | 0x1d),
                40 => self.address == 0x39,
                _ => false,
            }
    }
}
// Fixed factory trim, with the same nonlinear compensation used by the guest driver.
const T: [f64; 3] = [27504., 26435., -1000.];
const P: [f64; 9] = [
    36477., -10685., 3024., 2855., 140., -7., 15500., -14600., 6000.,
];
const H: [f64; 6] = [75., 362., 0., 334., 50., 30.];
fn temperature(raw: f64) -> (f64, f64) {
    let a = (raw / 16384. - T[0] / 1024.) * T[1];
    let b = (raw / 131072. - T[0] / 8192.).powi(2) * T[2];
    ((a + b) / 5120., a + b)
}
fn pressure(raw: f64, fine: f64) -> f64 {
    let mut a = fine / 2. - 64000.;
    let mut b = a * a * P[5] / 32768.;
    b += a * P[4] * 2.;
    b = b / 4. + P[3] * 65536.;
    a = (P[2] * a * a / 524288. + P[1] * a) / 524288.;
    a = (1. + a / 32768.) * P[0];
    let p = (1048576. - raw - b / 4096.) * 6250. / a;
    (p + (P[8] * p * p / 2147483648. + p * P[7] / 32768. + P[6]) / 16.).max(0.)
}
fn humidity(raw: f64, fine: f64) -> f64 {
    let t = fine - 76800.;
    let h = (raw - (H[3] * 64. + H[4] / 16384. * t))
        * (H[1] / 65536. * (1. + H[5] / 67108864. * t * (1. + H[2] / 67108864. * t)));
    (h * (1. - H[0] * h / 524288.)).clamp(0., 100.)
}
fn inverse(target: f64, max: u32, decreasing: bool, calc: impl Fn(f64) -> f64) -> u32 {
    let (mut lo, mut hi) = (0, max);
    while lo < hi {
        let m = lo + (hi - lo) / 2;
        if (calc(m as f64) < target) != decreasing {
            lo = m + 1;
        } else {
            hi = m;
        }
    }
    if lo > 0 && (calc((lo - 1) as f64) - target).abs() < (calc(lo as f64) - target).abs() {
        lo - 1
    } else {
        lo
    }
}
fn oversample(v: u8) -> u64 {
    if v == 0 {
        0
    } else {
        1 << ((v - 1).min(4))
    }
}

struct EnvironmentSensor {
    pub config: SensorConfig,
    clock: Arc<AtomicU64>,
    hz: u64,
    now: u64,
    regs: [u8; 256],
    values: [f64; 4],
    humidity_os: u8,
    ready: Option<u64>,
    measuring: bool,
    filtered: Option<[f64; 2]>,
    readings: [f64; 4],
    generation: u32,
    sampled: bool,
    mode: u8,
    mtreg: u8,
    powered: bool,
}
impl EnvironmentSensor {
    pub fn new(config: SensorConfig, clock: Arc<AtomicU64>, hz: u32) -> Self {
        let mut s = Self {
            config,
            clock,
            hz: hz as u64,
            now: 0,
            regs: [0; 256],
            values: [23., 50., 101325., 100.],
            humidity_os: 0,
            ready: None,
            measuring: false,
            filtered: None,
            readings: [f64::NAN; 4],
            generation: 0,
            sampled: false,
            mode: 0,
            mtreg: 69,
            powered: false,
        };
        s.reset();
        s
    }
    fn ticks(&self, us: u64) -> u64 {
        us * self.hz / 1_000_000
    }
    fn reset(&mut self) {
        self.regs = [0; 256];
        self.sampled = false;
        self.readings = [f64::NAN; 4];
        self.humidity_os = 0;
        self.filtered = None;
        self.measuring = false;
        self.regs[0xd0] = if self.config.model == 1 { 0x60 } else { 0x58 };
        for (i, v) in T.into_iter().chain(P).enumerate() {
            self.regs[0x88 + i * 2..0x8a + i * 2].copy_from_slice(&(v as i32 as u16).to_le_bytes());
        }
        self.regs[0xa1] = H[0] as u8;
        self.regs[0xe1..0xe3].copy_from_slice(&(H[1] as i16).to_le_bytes());
        self.regs[0xe3] = H[2] as u8;
        self.regs[0xe4] = (H[3] as u16 >> 4) as u8;
        self.regs[0xe5] = ((H[3] as u16 as u8) & 15) | ((H[4] as u8 & 15) << 4);
        self.regs[0xe6] = (H[4] as u16 >> 4) as u8;
        self.regs[0xe7] = H[5] as i8 as u8;
        self.regs[0xf7] = 0x80;
        self.regs[0xfa] = 0x80;
        self.regs[0xfd] = 0x80;
        self.regs[0xf3] = 1;
        self.ready = Some(self.now + self.ticks(2000));
        if self.config.model == 3 {
            self.regs = [0; 256];
            self.ready = None;
            self.mtreg = 69;
            self.mode = 0;
            self.powered = false;
        }
    }
    fn measurement_us(&self) -> u64 {
        if self.config.model == 3 {
            return (if self.mode & 3 == 3 { 16000 } else { 120000 }) * self.mtreg as u64 / 69;
        }
        let t = oversample(self.regs[0xf4] >> 5);
        let p = oversample((self.regs[0xf4] >> 2) & 7);
        let h = if self.config.model == 1 {
            oversample(self.humidity_os)
        } else {
            0
        };
        1250 + 2300 * t
            + if p > 0 { 2300 * p + 575 } else { 0 }
            + if h > 0 { 2300 * h + 575 } else { 0 }
    }
    fn standby_us(&self) -> u64 {
        let i = (self.regs[0xf5] >> 5) as usize;
        let mut times = [500, 62500, 125000, 250000, 500000, 1000000, 10000, 20000];
        if self.config.model == 2 {
            times[6] = 2000000;
            times[7] = 4000000;
        }
        times[i]
    }
    fn capture(&mut self, count: u64) {
        self.generation = ((self.generation as u64 + count - 1) % (u32::MAX as u64 - 1) + 1) as u32;
        self.sampled = true;
        if self.config.model == 3 {
            let factor = if self.mode & 3 == 1 { 2. } else { 1. };
            let raw = (self.values[3] * 1.2 * self.mtreg as f64 / 69. * factor)
                .round()
                .clamp(0., 65535.) as u16;
            let raw = if self.mode & 3 == 3 { raw & !3 } else { raw };
            self.regs[..2].copy_from_slice(&raw.to_be_bytes());
            self.readings[3] = raw as f64 / 1.2 * 69. / self.mtreg as f64 / factor;
            return;
        }
        let raw_t = inverse(self.values[0], 0xfffff, false, |r| temperature(r).0);
        let fine = temperature(raw_t as f64).1;
        let raw_p = inverse(self.values[2], 0xfffff, true, |r| pressure(r, fine));
        let raw_h = inverse(self.values[1], 65535, false, |r| humidity(r, fine));
        let mut raw = [raw_t as f64, raw_p as f64];
        let filter = (self.regs[0xf5] >> 2) & 7;
        if filter > 0 {
            if let Some(old) = self.filtered {
                let decay = (1. - 1. / (1u32 << filter.min(4)) as f64).powf(count as f64);
                for i in 0..2 {
                    raw[i] += (old[i] - raw[i]) * decay;
                }
            }
            self.filtered = Some(raw);
        } else {
            self.filtered = None;
        }
        for (offset, value, enabled) in [
            (0xfa, raw[0].round() as u32, self.regs[0xf4] >> 5 != 0),
            (0xf7, raw[1].round() as u32, (self.regs[0xf4] >> 2) & 7 != 0),
        ] {
            let v = if enabled { value } else { 0x80000 };
            self.regs[offset] = (v >> 12) as u8;
            self.regs[offset + 1] = (v >> 4) as u8;
            self.regs[offset + 2] = (v << 4) as u8;
        }
        self.regs[0xfd..0xff].copy_from_slice(
            &(if self.humidity_os > 0 {
                raw_h as u16
            } else {
                0x8000
            })
            .to_be_bytes(),
        );
        let (temp, fine) = temperature(raw[0].round());
        self.readings[0] = if self.regs[0xf4] >> 5 != 0 {
            temp
        } else {
            f64::NAN
        };
        self.readings[2] = if (self.regs[0xf4] >> 2) & 7 != 0 {
            pressure(raw[1].round(), fine)
        } else {
            f64::NAN
        };
        self.readings[1] = if self.config.model == 1 && self.humidity_os > 0 {
            humidity(raw_h as f64, fine)
        } else {
            f64::NAN
        };
    }
    fn sync(&mut self) {
        self.now = self.clock.load(Ordering::Relaxed);
        let Some(deadline) = self.ready else { return };
        if self.now < deadline {
            if self.config.model != 3 && self.regs[0xf3] & 1 == 0 && self.regs[0xf4] & 3 == 3 {
                self.measuring = deadline - self.now <= self.ticks(self.measurement_us());
                self.regs[0xf3] = (self.regs[0xf3] & !8) | if self.measuring { 8 } else { 0 };
            }
            return;
        }
        if self.config.model != 3 && self.regs[0xf3] & 1 != 0 {
            self.regs[0xf3] &= !1;
            self.ready = None;
            return;
        }
        let continuous = if self.config.model == 3 {
            self.mode & 0x10 != 0
        } else {
            self.regs[0xf4] & 3 == 3
        };
        let period = self
            .ticks(
                self.measurement_us()
                    + if self.config.model == 3 {
                        0
                    } else {
                        self.standby_us()
                    },
            )
            .max(1);
        let count = if continuous {
            1 + (self.now - deadline) / period
        } else {
            1
        };
        self.capture(count);
        if continuous {
            self.ready = Some(deadline + count * period);
            self.measuring = self.config.model != 3
                && self.ready.unwrap() - self.now <= self.ticks(self.measurement_us());
        } else {
            self.ready = None;
            self.measuring = false;
            if self.config.model == 3 {
                self.powered = false;
            } else {
                self.regs[0xf4] &= !3;
            }
        }
        if self.config.model != 3 {
            self.regs[0xf3] = (self.regs[0xf3] & !8) | if self.measuring { 8 } else { 0 };
        }
    }
    pub fn generation(&mut self) -> u32 {
        self.sync();
        if self.sampled {
            self.generation
        } else {
            0
        }
    }
    pub fn value(&mut self, field: u32) -> f64 {
        self.sync();
        self.readings
            .get(field as usize)
            .copied()
            .unwrap_or(f64::NAN)
    }
    pub fn set(&mut self, field: u32, value: f64) -> bool {
        let range = match (self.config.model, field) {
            (1 | 2, 0) => -40.0..=85.0,
            (1, 1) => 0.0..=100.0,
            (1 | 2, 2) => 30000.0..=110000.0,
            (3, 3) => 0.0..=120000.0,
            _ => return false,
        };
        if !value.is_finite() || !range.contains(&value) {
            return false;
        }
        self.sync();
        self.values[field as usize] = value;
        true
    }
    fn write_reg(&mut self, addr: u8, value: u8) {
        self.sync();
        match addr {
            0xe0 if value == 0xb6 => self.reset(),
            0xf2 if self.config.model == 1 => self.regs[0xf2] = value & 7,
            0xf4 => {
                self.regs[0xf4] = value;
                self.humidity_os = self.regs[0xf2];
                if value & 3 != 0 {
                    self.measuring = true;
                    self.regs[0xf3] |= 8;
                    self.ready = Some(self.now + self.ticks(self.measurement_us()));
                } else {
                    self.ready = None;
                    self.measuring = false;
                    self.regs[0xf3] &= !8;
                }
            }
            0xf5 if self.regs[0xf4] & 3 == 0 => {
                self.regs[0xf5] = value & 0xfd;
                self.filtered = None;
            }
            _ => {}
        }
    }
    fn command(&mut self, v: u8) -> bool {
        self.sync();
        match v {
            0 => {
                self.powered = false;
                self.ready = None;
            }
            1 => self.powered = true,
            7 if self.powered => {
                self.regs[..2].fill(0);
                self.sampled = false;
                self.readings[3] = f64::NAN;
            }
            0x10 | 0x11 | 0x13 | 0x20 | 0x21 | 0x23 if (31..=254).contains(&self.mtreg) => {
                self.mode = v;
                self.powered = true;
                self.ready = Some(self.now + self.ticks(self.measurement_us()));
            }
            0x40..=0x47 => self.mtreg = (self.mtreg & 31) | ((v & 7) << 5),
            0x60..=0x7f => self.mtreg = (self.mtreg & 0xe0) | (v & 31),
            _ => return false,
        }
        true
    }
}

mod gas;
mod humidity_light;
mod analog;
mod aht_mcp;
mod magnetometers;
mod adxl375;
mod pressure;
mod environment_extra;
mod bme680;
mod bno055;
mod lsm6ds3;
mod motion;
mod power;
mod ina228;
mod as7341;
mod rtc;
mod temperature;
mod fuel;
mod nau7802;
mod vl53l1x;
const FIELD_COUNT: usize = 74;
#[derive(Clone, Copy, PartialEq)]
enum WireFormat {
    Bytes,
    Command,
    Word,
    Block,
    SmBus,
    Pairs,
    Address16,
}

trait RegisterSensor {
    fn general_reset(&mut self) -> bool {
        false
    }
    fn format(&self) -> WireFormat {
        WireFormat::Bytes
    }
    fn address(&self, configured: u8) -> u8 {
        configured
    }
    fn read_extended(&self, _reg: u16) -> u8 {
        0
    }
    fn write_extended(&mut self, _reg: u16, _value: u8) -> bool {
        false
    }
    fn start(&mut self, _read: bool) {}
    fn address_ready(&self) -> bool {
        true
    }
    fn next_address(&self, reg: u8) -> u8 {
        reg.wrapping_add(1)
    }
    fn register_width(&self, _reg: u8) -> u8 { 1 }
    fn register_offset(&self, reg: u8) -> usize { reg as usize * 4 }
    fn read_ready(&self) -> bool {
        true
    }
    fn sync(&mut self);
    fn registers(&self) -> [u8; 256];
    fn write(&mut self, reg: u8, value: u16) -> bool;
    fn stop(&mut self) {}
    fn read_done(&mut self, _reg: u8) {}
    fn set(&mut self, field: u32, value: f64) -> bool;
    fn generation(&mut self) -> u32;
    fn value(&mut self, field: u32) -> f64;
}
impl RegisterSensor for EnvironmentSensor {
    fn format(&self) -> WireFormat {
        if self.config.model == 3 {
            WireFormat::Command
        } else {
            WireFormat::Bytes
        }
    }
    fn sync(&mut self) {
        EnvironmentSensor::sync(self)
    }
    fn registers(&self) -> [u8; 256] {
        self.regs
    }
    fn write(&mut self, reg: u8, value: u16) -> bool {
        if self.config.model == 3 {
            self.command(value as u8)
        } else {
            self.write_reg(reg, value as u8);
            true
        }
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        EnvironmentSensor::set(self, field, value)
    }
    fn generation(&mut self) -> u32 {
        EnvironmentSensor::generation(self)
    }
    fn value(&mut self, field: u32) -> f64 {
        EnvironmentSensor::value(self, field)
    }
}
pub struct Sensor {
    pub config: SensorConfig,
    device: Box<dyn RegisterSensor>,
}
impl Sensor {
    pub fn new(config: SensorConfig, clock: Arc<AtomicU64>, hz: u32) -> Self {
        let device: Box<dyn RegisterSensor> = match config.model {
            26 | 27 => Box::new(humidity_light::Humidity::new(clock, hz, config.model)),
            28 => Box::new(humidity_light::Veml7700::new(clock, hz)),
            30 => Box::new(aht_mcp::Aht20::new(clock, hz)),
            31 => Box::new(aht_mcp::Mcp9808::new(clock, hz)),
            32 | 33 => Box::new(gas::Gas::new(clock, hz, config.model, config.id)),
            34 => Box::new(magnetometers::Hmc5883l::new(clock, hz)),
            35 => Box::new(magnetometers::Lis3mdl::new(clock, hz)),
            36 => Box::new(pressure::Dps310::new(clock, hz)),
            37 => Box::new(pressure::Lps22df::new(clock, hz)),
            38 => Box::new(ina228::Ina228::new(clock, hz, config.shunt_milliohms)),
            39 => Box::new(adxl375::Adxl375::new(clock, hz)),
            40 => Box::new(as7341::As7341::new(clock, hz)),
            22 => Box::new(vl53l1x::Vl53l1x::new(clock, hz)),
            21 => Box::new(nau7802::Nau7802::new(clock, hz)),
            20 | 23..=25 => Box::new(fuel::Max1704x::new(clock, hz, config.model)),
            16 => Box::new(bno055::Bno055::new(clock, hz)),
            15 => Box::new(lsm6ds3::Lsm6ds3::new(clock, hz)),
            4 => Box::new(motion::Mpu6050::new(clock, hz)),
            5 => Box::new(power::Ina219::new(clock, hz, config.shunt_milliohms)),
            6 => Box::new(rtc::Ds3231::new(clock, hz)),
            29 => Box::new(rtc::Ds1307::new(clock, hz)),
            7 => Box::new(temperature::Sht4x::new(clock, hz, config.id)),
            8 => Box::new(temperature::Tmp117::new(clock, hz)),
            9 => Box::new(temperature::Mlx90614::new(clock, hz)),
            10 => Box::new(bme680::Bme680::new(clock, hz)),
            11 => Box::new(analog::Ina260::new(clock, hz)),
            12 => Box::new(analog::Ads1115::new(clock, hz)),
            13 => Box::new(analog::Mcp4725::new(clock, hz)),
            14 => Box::new(analog::As5600::new(clock, hz)),
            17 => Box::new(environment_extra::Mcp9600::new(clock, hz)),
            18 => Box::new(environment_extra::Opt3001::new(clock, hz)),
            19 => Box::new(environment_extra::Scd4x::new(clock, hz, config.id)),
            _ => Box::new(EnvironmentSensor::new(config, clock, hz)),
        };
        Self { config, device }
    }
    pub fn set(&mut self, field: u32, value: f64) -> bool {
        self.device.set(field, value)
    }
    pub fn generation(&mut self) -> u32 {
        self.device.generation()
    }
    pub fn value(&mut self, field: u32) -> f64 {
        self.device.value(field)
    }
}
pub struct SensorI2c {
    general_call: bool,
    state: Arc<Mutex<Sensor>>,
    ptr: u8,
    wide_ptr: u16,
    first: bool,
    high: Option<u8>,
    snapshot: [u8; 256],
    index: u8,
    pending: Option<u16>,
}
impl SensorI2c {
    pub fn new(state: Arc<Mutex<Sensor>>) -> Self {
        Self {
            state,
            general_call: false,
            ptr: 0,
            wide_ptr: 0,
            first: true,
            high: None,
            snapshot: [0; 256],
            index: 0,
            pending: None,
        }
    }
}
impl I2cDevice for SensorI2c {
    fn matches_address(&self, configured: u8, address: u8, read: bool) -> bool {
        self.address(configured) == address
            || (address == 0 && !read && matches!(self.state.lock().unwrap().config.model, 32 | 33))
    }
    fn start_address(&mut self, address: u8, read: bool) -> bool {
        self.general_call = address == 0;
        self.start(read)
    }
    fn address(&self, configured: u8) -> u8 {
        self.state.lock().unwrap().device.address(configured)
    }
    fn pins(&self) -> Option<(u8, u8)> {
        let s = self.state.lock().unwrap();
        Some((s.config.sda, s.config.scl))
    }
    fn start(&mut self, read: bool) -> bool {
        let mut s = self.state.lock().unwrap();
        s.device.sync();
        if !s.device.address_ready() {
            return false;
        }
        s.device.start(read);
        self.high = None;
        self.index = 0;
        self.pending = None;
        if read {
            if !s.device.read_ready() {
                return false;
            }
            self.snapshot = s.device.registers();
            if s.device.format() == WireFormat::Command {
                self.ptr = 0;
            }
        } else {
            self.first = true;
        }
        true
    }
    fn write(&mut self, b: u8) -> bool {
        let mut s = self.state.lock().unwrap();
        let format = s.device.format();
        if self.general_call {
            if !self.first {
                return false;
            }
            self.first = false;
            return b == 6 && s.device.general_reset();
        }
        if format == WireFormat::Command {
            return s.device.write(0, b as u16);
        }
        if format == WireFormat::Address16 {
            if self.first {
                if let Some(hi) = self.high.take() {
                    self.wide_ptr = u16::from_be_bytes([hi, b]);
                    self.first = false;
                } else {
                    self.high = Some(b);
                }
                return true;
            }
            let ack = s.device.write_extended(self.wide_ptr, b);
            self.wide_ptr = self.wide_ptr.wrapping_add(1);
            return ack;
        }
        if self.first {
            if s.config.model == 5 && b > 5 {
                return false;
            }
            self.ptr = b;
            self.first = false;
            return true;
        }
        match format {
            WireFormat::Pairs => {
                self.first = true;
                s.device.write(self.ptr, b as u16)
            }
            WireFormat::Block => {
                match s.device.register_width(self.ptr) {
                    1 => s.device.write(self.ptr, b as u16),
                    2 => if let Some(high) = self.high.take() { s.device.write(self.ptr,u16::from_be_bytes([high,b])) } else { self.high=Some(b);true },
                    _ => false,
                }
            }
            WireFormat::Word => {
                if let Some(high) = self.high.take() {
                    s.device.write(self.ptr, u16::from_be_bytes([high, b]))
                } else {
                    self.high = Some(b);
                    true
                }
            }
            WireFormat::SmBus => {
                if let Some(word) = self.pending.take() {
                    let [lo, hi] = word.to_le_bytes();
                    if temperature::crc8(&[s.config.address << 1, self.ptr, lo, hi], 0, 7) != b {
                        return false;
                    }
                    return s.device.write(self.ptr, word);
                }
                if let Some(lo) = self.high.take() {
                    self.pending = Some(u16::from_le_bytes([lo, b]));
                } else {
                    self.high = Some(b);
                }
                true
            }
            _ => {
                let ack = s.device.write(self.ptr, b as u16);
                self.ptr = s.device.next_address(self.ptr);
                ack
            }
        }
    }
    fn read(&mut self) -> u8 {
        let mut s = self.state.lock().unwrap();
        let format = s.device.format();
        match format {
            WireFormat::Block => {
                let width=s.device.register_width(self.ptr);
                let offset=s.device.register_offset(self.ptr)+self.index as usize;
                let value=if self.index<width && offset<256 {self.snapshot[offset]} else {0xff};
                self.index=self.index.saturating_add(1);
                if self.index==width {s.device.read_done(self.ptr);}
                value
            }
            WireFormat::Address16 => {
                let value = s.device.read_extended(self.wide_ptr);
                self.wide_ptr = self.wide_ptr.wrapping_add(1);
                value
            }
            WireFormat::Word => {
                let offset = self.ptr.wrapping_mul(2) + self.index;
                let value = self.snapshot[offset as usize];
                self.index ^= 1;
                if self.index == 0 {
                    s.device.read_done(self.ptr);
                }
                value
            }
            WireFormat::SmBus => {
                let offset = self.ptr.wrapping_mul(2) as usize;
                let hi = self.snapshot[offset];
                let lo = self.snapshot[offset + 1];
                let value = match self.index {
                    0 => lo,
                    1 => hi,
                    2 => temperature::crc8(
                        &[
                            s.config.address << 1,
                            self.ptr,
                            (s.config.address << 1) | 1,
                            lo,
                            hi,
                        ],
                        0,
                        7,
                    ),
                    _ => 0xff,
                };
                self.index = self.index.saturating_add(1);
                if self.index == 3 {
                    s.device.read_done(self.ptr);
                }
                value
            }
            _ => {
                let value = self.snapshot[self.ptr as usize];
                s.device.read_done(self.ptr);
                self.ptr = s.device.next_address(self.ptr);
                value
            }
        }
    }
    fn stop(&mut self) {
        self.state.lock().unwrap().device.stop();
    }
}
struct SampleState {
    clock: Arc<AtomicU64>,
    hz: u64,
    now: u64,
    regs: [u8; 256],
    inputs: [f64; FIELD_COUNT],
    readings: [f64; FIELD_COUNT],
    generation: u32,
}
impl SampleState {
    fn new(clock: Arc<AtomicU64>, hz: u32) -> Self {
        Self {
            clock,
            hz: hz as u64,
            now: 0,
            regs: [0; 256],
            inputs: [0.; FIELD_COUNT],
            readings: [f64::NAN; FIELD_COUNT],
            generation: 0,
        }
    }
    fn time(&mut self) {
        self.now = self.clock.load(Ordering::Relaxed);
    }
    fn ticks(&self, us: u64) -> u64 {
        us * self.hz / 1_000_000
    }
    fn publish(&mut self, count: u64) {
        self.generation = ((self.generation as u64 + count - 1) % (u32::MAX as u64 - 1) + 1) as u32;
    }
    fn value(&self, field: u32) -> f64 {
        self.readings
            .get(field as usize)
            .copied()
            .unwrap_or(f64::NAN)
    }
    fn put16(&mut self, reg: usize, value: i32) {
        self.regs[reg..reg + 2].copy_from_slice(&(value as u16).to_be_bytes());
    }
    fn get16(&self, reg: usize) -> u16 {
        u16::from_be_bytes([self.regs[reg], self.regs[reg + 1]])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sensor(model: u8) -> EnvironmentSensor {
        EnvironmentSensor::new(
            SensorConfig {
                id: 0,
                sda: 4,
                scl: 5,
                address: if model == 3 { 0x23 } else { 0x76 },
                model,
                shunt_milliohms: 0,
            },
            Arc::new(AtomicU64::new(0)),
            1_000_000,
        )
    }
    fn time(s: &mut EnvironmentSensor, us: u64) {
        s.clock.store(us, Ordering::Relaxed);
        s.sync();
    }
    fn raw(s: &EnvironmentSensor, off: usize) -> f64 {
        (((s.regs[off] as u32) << 12)
            | ((s.regs[off + 1] as u32) << 4)
            | (s.regs[off + 2] as u32 >> 4)) as f64
    }
    #[test]
    fn factory_compensation_inverts_physical_inputs_across_the_sensor_range() {
        for temp in [-40., -10., 23., 85.] {
            let rt = inverse(temp, 0xfffff, false, |r| temperature(r).0);
            let (t, f) = temperature(rt as f64);
            assert!((t - temp).abs() < 0.005);
            for p in [30000., 101325., 110000.] {
                let r = inverse(p, 0xfffff, true, |r| pressure(r, f));
                assert!((pressure(r as f64, f) - p).abs() < 1.);
            }
            for h in [0., 35., 65., 100.] {
                let r = inverse(h, 65535, false, |r| humidity(r, f));
                assert!((humidity(r as f64, f) - h).abs() < 0.02);
            }
        }
    }
    #[test]
    fn bosch_reset_timing_forced_conversion_skip_and_read_only_calibration() {
        let mut s = sensor(1);
        assert_eq!(
            ((s.regs[0xe4] as u16) << 4) | (s.regs[0xe5] as u16 & 15),
            334
        );
        assert_eq!(
            ((s.regs[0xe6] as u16) << 4) | (s.regs[0xe5] as u16 >> 4),
            50
        );
        assert_eq!(s.generation(), 0);
        assert!(s.value(0).is_nan());
        s.set(0, 23.);
        assert_eq!(s.generation(), 0);
        assert_eq!(s.regs[0xf3], 1);
        time(&mut s, 2000);
        assert_eq!(s.regs[0xf3], 0);
        s.write_reg(0xd0, 0);
        s.write_reg(0x88, 0);
        assert_eq!(s.regs[0xd0], 0x60);
        assert_eq!(s.regs[0x88], T[0] as u16 as u8);
        s.write_reg(0xf2, 1);
        s.write_reg(0xf4, 0x25);
        let ready = s.ready.unwrap();
        assert_eq!(s.regs[0xf3] & 8, 8);
        assert_eq!(raw(&s, 0xfa), 0x80000 as f64);
        time(&mut s, ready - 1);
        assert_eq!(s.regs[0xf3] & 8, 8);
        time(&mut s, ready);
        assert_eq!(s.generation(), 1);
        assert!((s.value(0) - 23.).abs() < 0.01);
        assert_eq!(s.regs[0xf3], 0);
        assert_eq!(s.regs[0xf4] & 3, 0);
        let (t, f) = temperature(raw(&s, 0xfa));
        assert!((t - 23.).abs() < 0.01);
        assert!((pressure(raw(&s, 0xf7), f) - 101325.).abs() < 1.);
        assert!(
            (humidity(u16::from_be_bytes([s.regs[0xfd], s.regs[0xfe]]) as f64, f) - 50.).abs()
                < 0.02
        );
        s.write_reg(0xf4, 0x21);
        time(&mut s, ready + 10000);
        assert_eq!(raw(&s, 0xf7), 0x80000 as f64);
        s.write_reg(0xe0, 0xb6);
        assert_eq!(s.regs[0xf3], 1);
        assert_eq!(s.regs[0xf4], 0);
        assert!(!s.set(0, f64::NAN));
        assert!(!s.set(1, 101.));
        assert!(!s.set(3, 1.));
        assert!(!sensor(2).set(1, 50.));
    }
    #[test]
    fn burst_reads_hold_one_sample_while_normal_conversions_keep_running() {
        let clock = Arc::new(AtomicU64::new(2000));
        let state = Arc::new(Mutex::new(Sensor::new(
            SensorConfig {
                id: 0,
                sda: 4,
                scl: 5,
                address: 0x76,
                model: 1,
                shunt_milliohms: 0,
            },
            clock.clone(),
            1_000_000,
        )));
        let mut wire = SensorI2c::new(state.clone());
        wire.start(false);
        wire.write(0xf4);
        wire.write(0x27);
        wire.stop();
        clock.store(10000, Ordering::Relaxed);
        wire.start(false);
        wire.write(0xfa);
        wire.start(true);
        let old = state.lock().unwrap().device.registers()[0xfa..0xfd].to_vec();
        state.lock().unwrap().set(0, 40.);
        clock.store(1_000_000, Ordering::Relaxed);
        assert!((state.lock().unwrap().value(0) - 40.).abs() < 0.01);
        assert_eq!([wire.read(), wire.read(), wire.read()], old.as_slice());
        wire.start(false);
        wire.write(0xfa);
        wire.start(true);
        assert_ne!([wire.read(), wire.read(), wire.read()], old.as_slice());
        clock.store(365 * 86400 * 1_000_000, Ordering::Relaxed);
        assert!((state.lock().unwrap().value(0) - 40.).abs() < 0.01);
    }

    #[test]
    fn light_commands_measurement_time_gain_and_power_down() {
        let mut s = sensor(3);
        time(&mut s, 1_000_000);
        assert_eq!(s.regs[0], 0);
        s.set(3, 500.);
        assert!(s.command(0x21));
        let ready = s.ready.unwrap();
        time(&mut s, ready - 1);
        assert_eq!(s.regs[0], 0);
        time(&mut s, ready);
        assert_eq!(u16::from_be_bytes([s.regs[0], s.regs[1]]), 1200);
        assert!(!s.powered);
        assert!(s.ready.is_none());
        assert!(!s.command(7));
        s.command(1);
        s.command(7);
        assert_eq!(s.regs[0], 0);
        s.command(0x44);
        s.command(0x6a);
        assert_eq!(s.mtreg, 138);
        s.command(0x10);
        assert_eq!(s.ready.unwrap() - s.now, 240000);
        time(&mut s, ready + 240000);
        assert_eq!(u16::from_be_bytes([s.regs[0], s.regs[1]]), 1200);
        s.command(0);
        s.set(3, 100.);
        time(&mut s, 10_000_000);
        assert_eq!(u16::from_be_bytes([s.regs[0], s.regs[1]]), 1200);
    }
}

#[cfg(test)]
mod register_wire_tests {
    use super::*;
    #[test]
    fn word_register_pointer_is_retained_and_each_read_starts_with_the_msb() {
        let state = Arc::new(Mutex::new(Sensor::new(
            SensorConfig {
                id: 0,
                sda: 4,
                scl: 5,
                address: 0x40,
                model: 5,
                shunt_milliohms: 100,
            },
            Arc::new(AtomicU64::new(0)),
            1_000_000,
        )));
        let mut wire = SensorI2c::new(state);
        wire.start(false);
        assert!(wire.write(5));
        assert!(wire.write(0x12));
        assert!(wire.write(0x35));
        wire.stop();
        wire.start(false);
        wire.write(5);
        wire.start(true);
        assert_eq!(wire.read(), 0x12);
        wire.stop();
        wire.start(true);
        assert_eq!(wire.read(), 0x12);
        assert_eq!(wire.read(), 0x34);
        wire.stop();
        wire.start(false);
        assert!(!wire.write(0xff));
    }
}
