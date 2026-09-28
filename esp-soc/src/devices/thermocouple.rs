use crate::SpiPins;
use super::spi_bitbang::SpiBitBang;

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
        [1, 2].contains(&self.model) && self.id < 16
            && (self.model != 1 || self.mosi == 255)
            && pins.iter().enumerate().all(|(i, &p)|
                (p < 49 || (i == 1 && self.model == 1 && p == 255))
                && !pins[..i].contains(&p))
    }
}

struct Conversion {
    deadline: u64,
    counts: u16,
    reference: f64,
    nominal: f64,
    fault: u8,
}

pub struct Thermocouple {
    pub config: Config,
    hz: u64,
    now: u64,
    temperature: f64,
    cold: f64,
    reference: f64,
    nominal: f64,
    fault_input: u8,
    generation: u32,
    output: [f64; 7],
    selected: bool,
    clock: bool,
    miso: bool,
    input: SpiBitBang,
    bit: usize,
    response: u8,
    address: Option<(u8, bool)>,
    word: u32,
    next_55: u64,
    pending_55: Option<u32>,
    registers: [u8; 8],
    bias_ready: u64,
    start_pending: bool,
    conversion: Option<Conversion>,
    diagnostic: Option<u64>,
    diagnostic_pending: bool,
}
impl Thermocouple {
    pub fn new(config: Config, hz: u64, now: u64) -> Self {
        let mut input = SpiBitBang::new(config.sclk, config.mosi, config.cs);
        input.pin(config.sclk, true);
        let mut device = Self {
            config, hz, now, temperature: 25., cold: 25., reference: 430., nominal: 100.,
            fault_input: 0, generation: 0, output: [f64::NAN; 7], selected: false,
            clock: false, miso: false, input, bit: 0, response: 0, address: None,
            word: 0, next_55: now + hz / 5, pending_55: None,
            registers: [0, 0, 0, 255, 255, 0, 0, 0], bias_ready: now,
            start_pending: false, conversion: None, diagnostic: None, diagnostic_pending: false,
        };
        device.output[4] = 0.;
        device
    }
    fn duration(&self, microseconds: u64) -> u64 {
        (self.hz * microseconds).div_ceil(1_000_000).max(1)
    }
    pub fn set(&mut self, field: u32, value: f64) -> bool {
        if !value.is_finite() { return false; }
        match (self.config.model, field) {
            (1, 0) if (-270. ..=1372.).contains(&value) => self.temperature = value,
            (2, 0) if (-200. ..=850.).contains(&value) => self.temperature = value,
            (1, 1) if (-55. ..=125.).contains(&value) => self.cold = value,
            (2, 2) if value > 0. => self.reference = value,
            (2, 3) if value > 0. => self.nominal = value,
            (model, 4) if value.fract() == 0. && (0. ..=255.).contains(&value)
                && ((model == 1 && value <= 7.) || (model == 2 && value as u8 & 3 == 0)) => {
                    self.fault_input = value as u8;
                    if model == 2 && self.fault_input & 4 != 0 { self.registers[7] |= 4; self.publish_65(); }
                }
            _ => return false,
        }
        true
    }
    pub fn generation(&self) -> u32 { self.generation }
    pub fn value(&self, field: u32) -> f64 {
        self.output.get(field as usize).copied().unwrap_or(f64::NAN)
    }
    pub fn level(&self) -> Option<(u8, bool)> {
        self.selected.then_some((self.config.miso, self.miso))
    }
    pub fn next_deadline(&self) -> Option<u64> {
        if self.config.model == 1 { Some(self.next_55) }
        else { self.conversion.as_ref().map(|c| c.deadline).into_iter().chain(self.diagnostic)
            .chain((self.conversion.is_none() && !self.start_pending && self.registers[0] & 0xc0 == 0xc0 && self.now < self.bias_ready).then_some(self.bias_ready)).min() }
    }
    fn publish_55(&mut self, word: u32) {
        self.word = word;
        self.output[0] = if word & 0x10000 == 0 { ((word as i32) >> 18) as f64 / 4. } else { f64::NAN };
        self.output[1] = ((word as i32) << 16 >> 20) as f64 / 16.;
        self.output[4] = (word & 7) as f64;
        self.generation = self.generation.wrapping_add(1);
    }
    fn publish_65(&mut self) {
        self.output[4] = self.registers[7] as f64;
        if self.registers[7] != 0 { self.output[0] = f64::NAN; }
        self.registers[2] = (self.registers[2] & !1) | u8::from(self.registers[7] != 0);
        self.generation = self.generation.wrapping_add(1);
    }
    fn start_conversion(&mut self, first: bool) {
        if self.registers[0] & 0x80 == 0 || self.now < self.bias_ready { return; }
        let ratio = resistance_ratio(self.temperature) * (self.nominal / self.reference);
        let counts = (ratio * 32768.).round().clamp(0., 32767.) as u16;
        let micros = match (first, self.registers[0] & 1 != 0) {
            (true, false) => 52_000, (true, true) => 62_500,
            (false, false) => 16_700, (false, true) => 20_000,
        };
        self.conversion = Some(Conversion { deadline: self.now + self.duration(micros), counts,
            reference: self.reference, nominal: self.nominal, fault: self.fault_input });
    }
    pub fn advance(&mut self, now: u64) {
        self.now = now;
        if self.config.model == 1 {
            if now >= self.next_55 {
                let thermo = if self.fault_input & 1 != 0 { 0x1fff } else { (self.temperature * 4.).round() as i32 as u32 & 0x3fff };
                let cold = (self.cold * 16.).round() as i32 as u32 & 0xfff;
                let word = thermo << 18 | cold << 4 | u32::from(self.fault_input != 0) << 16 | self.fault_input as u32;
                if self.selected { self.pending_55 = Some(word); } else { self.publish_55(word); }
                let period = self.duration(70_000);
                self.next_55 += ((now - self.next_55) / period + 1) * period;
            }
            return;
        }
        if self.diagnostic.is_some_and(|d| now >= d) {
            self.diagnostic = None;
            self.registers[0] &= !0x0c;
            self.registers[7] |= self.fault_input;
            self.publish_65();
        }
        if self.conversion.as_ref().is_some_and(|c| now >= c.deadline) {
            let c = self.conversion.take().unwrap();
            let raw = c.counts << 1;
            self.registers[1] = (raw >> 8) as u8;
            self.registers[2] = raw as u8;
            let high = u16::from_be_bytes([self.registers[3], self.registers[4]]) & 0xfffe;
            let low = u16::from_be_bytes([self.registers[5], self.registers[6]]) & 0xfffe;
            self.registers[7] |= c.fault | if raw >= high { 0x80 } else { 0 } | if raw <= low { 0x40 } else { 0 };
            self.output[5] = c.counts as f64;
            self.output[6] = c.counts as f64 / 32768. * c.reference;
            self.output[0] = if c.counts == 0 || c.counts == 32767 { f64::NAN }
                else { temperature_from_ratio((c.counts as f64 / 32768.) * (c.reference / c.nominal)) };
            self.registers[0] &= !0x20;
            self.publish_65();
            if self.registers[0] & 0x40 != 0 { self.start_conversion(false); }
        }
        if self.conversion.is_none() && !self.start_pending && self.registers[0] & 0xc0 == 0xc0 && now >= self.bias_ready {
            self.start_conversion(true);
        }
    }
    fn write(&mut self, address: u8, value: u8) {
        match address {
            0 => {
                let previous = self.registers[0];
                self.registers[0] = value & !2;
                if value & 0x80 == 0 { self.conversion = None; }
                if value & 0x80 != 0 && previous & 0x80 == 0 { self.bias_ready = self.now + self.duration(1_000); }
                if value & 2 != 0 { self.registers[7] = self.fault_input & 4; self.publish_65(); }
                if value & 0x20 != 0 && value & 0x0c != 0 { self.registers[0] &= !0x2c; self.start_pending=false; self.diagnostic_pending=false; }
                else {
                    self.start_pending = value & 0x20 != 0 || (value & 0x40 != 0 && previous & 0x40 == 0);
                    self.diagnostic_pending = matches!(value & 0x0c, 4 | 12);
                }
            }
            3..=6 => self.registers[address as usize] = value,
            _ => {}
        }
    }
    fn exchange(&mut self, byte: u8) -> u8 {
        if self.config.model == 1 {
            let result = if self.bit < 32 { (self.word >> (24 - self.bit)) as u8 } else { 0 };
            self.bit = (self.bit + 8).min(32);
            return result;
        }
        match self.address {
            None => { self.address = Some((byte & 0x7f, byte & 0x80 != 0)); 0 },
            Some((address, write)) => {
                let response = self.registers.get(address as usize).copied().unwrap_or(0);
                if write { self.write(address, byte); }
                self.address = Some((address.saturating_add(1), write));
                if write { 0 } else { response }
            }
        }
    }
    fn select(&mut self, selected: bool) {
        if self.selected == selected { return; }
        self.selected = selected;
        self.bit = 0;
        self.address = None;
        self.response = 0;
        if selected { self.miso = self.config.model == 1 && self.word & 0x80000000 != 0; }
        else {
            if let Some(word) = self.pending_55.take() { self.publish_55(word); }
            if self.start_pending { self.start_pending = false; self.start_conversion(true); }
            if self.diagnostic_pending { self.diagnostic_pending=false; self.diagnostic=Some(self.now+self.duration(550)); }
        }
    }
    pub fn drive(&mut self, enabled:u64, output:u64) {
        let c=self.config;
        if c.model==2 {self.gpio(c.mosi, enabled&(1<<c.mosi)!=0 && output&(1<<c.mosi)!=0);}
        self.gpio(c.cs, enabled&(1<<c.cs)==0 || output&(1<<c.cs)!=0);
        self.gpio(c.sclk, enabled&(1<<c.sclk)!=0 && output&(1<<c.sclk)!=0);
    }
    pub fn gpio(&mut self, pin: u8, high: bool) {
        if pin == self.config.cs { self.select(!high); }
        if self.config.model == 2 {
            if pin == self.config.sclk && high && !self.clock && self.selected {
                if self.bit % 8 == 0 {
                    self.response = match self.address { Some((a, false)) => self.registers.get(a as usize).copied().unwrap_or(0), _ => 0 };
                }
                self.miso = self.response & (0x80 >> (self.bit % 8)) != 0;
                self.bit = (self.bit + 1) % 8;
            }
            if let Some(byte) = self.input.pin(pin, if pin == self.config.sclk { !high } else { high }) { self.exchange(byte); }
        } else if pin == self.config.sclk && !high && self.clock && self.selected {
            self.bit = (self.bit + 1).min(32);
            self.miso = self.bit < 32 && self.word & (0x80000000 >> self.bit) != 0;
        }
        if pin == self.config.sclk { self.clock = high; }
    }
    pub fn spi(&mut self, pins: SpiPins, tx: &[u8], rx_len: usize) -> Option<Vec<u8>> {
        if !(self.selected || pins.cs & (1 << self.config.cs) != 0)
            || pins.sclk & (1 << self.config.sclk) == 0
            || (self.config.model == 2 && pins.mosi & (1 << self.config.mosi) == 0) { return None; }
        let hardware_cs = !self.selected;
        if hardware_cs { self.select(true); }
        let mut rx = vec![255; rx_len];
        for i in 0..tx.len().max(rx_len) {
            let byte = self.exchange(tx.get(i).copied().unwrap_or(0));
            if i < rx_len && pins.miso == Some(self.config.miso) { rx[i] = byte; }
        }
        if hardware_cs { self.select(false); }
        Some(rx)
    }
}
fn resistance_ratio(t: f64) -> f64 {
    1. + 3.9083e-3 * t - 5.775e-7 * t * t
        + if t < 0. { -4.183e-12 * (t - 100.) * t.powi(3) } else { 0. }
}
fn temperature_from_ratio(ratio: f64) -> f64 {
    if !ratio.is_finite() || ratio < resistance_ratio(-200.) || ratio > resistance_ratio(850.) { return f64::NAN; }
    let (mut low, mut high) = (-200., 850.);
    for _ in 0..48 { let middle = (low + high) / 2.; if resistance_ratio(middle) < ratio { low = middle; } else { high = middle; } }
    (low + high) / 2.
}

#[cfg(test)]
mod thermocouple_tests {
    use super::*;
    fn device(model:u8)->Thermocouple {Thermocouple::new(Config{model,id:0,sclk:1,mosi:if model==1{255}else{2},miso:3,cs:4},1_000_000,0)}
    fn pins()->SpiPins{SpiPins{sclk:2,mosi:4,miso:Some(3),cs:16}}
    fn write(d:&mut Thermocouple,address:u8,value:u8){d.spi(pins(),&[address|0x80,value],0);}
    fn read(d:&mut Thermocouple,address:u8)->u8{d.spi(pins(),&[address,0],2).unwrap()[1]}
    fn sample(d:&mut Thermocouple){write(d,0,0x80);d.advance(d.now+10_000);write(d,0,0xa0);d.advance(d.now+52_000);}
    fn byte_gpio(d:&mut Thermocouple,value:u8)->u8{
        let mut out=0;
        for bit in (0..8).rev(){
            if d.config.model==2 {d.gpio(1,true);d.gpio(2,value&(1<<bit)!=0);d.gpio(1,false);out=(out<<1)|u8::from(d.level().unwrap().1);}
            else {d.gpio(1,true);out=(out<<1)|u8::from(d.level().unwrap().1);d.gpio(1,false);}
        }out
    }
    #[test] fn thermocouple_55_startup_signed_and_cold_junction(){
        let mut d=device(1);assert!(d.value(0).is_nan());d.set(0,-123.25);d.set(1,-12.5);
        d.advance(199_999);assert!(d.value(0).is_nan());d.advance(200_000);
        assert_eq!(d.value(0),-123.25);assert_eq!(d.value(1),-12.5);
        let bytes=d.spi(pins(),&[],4).unwrap();assert_eq!(u32::from_be_bytes(bytes.try_into().unwrap()),d.word);
    }
    #[test] fn thermocouple_55_latches_only_when_deselected(){
        let mut d=device(1);d.advance(200_000);let old=d.word;d.gpio(4,false);d.set(0,300.);d.advance(270_000);assert_eq!(d.word,old);
        let bytes:[u8;4]=std::array::from_fn(|_|byte_gpio(&mut d,0));assert_eq!(u32::from_be_bytes(bytes),old);
        d.gpio(4,true);assert_eq!(d.value(0),300.);assert_eq!(d.level(),None);
    }
    #[test] fn thermocouple_55_fault_bits_and_open_circuit_word(){
        for fault in 1..8 {let mut d=device(1);assert!(d.set(4,fault as f64));d.advance(200_000);assert!(d.value(0).is_nan());assert_eq!(d.value(1),25.);assert_eq!(d.word&0x10007,0x10000|fault);if fault&1!=0{assert_eq!(d.word>>18,0x1fff);}}
    }
    #[test] fn thermocouple_65_defaults_bias_and_single_conversion_ready(){
        let mut d=device(2);assert_eq!(d.registers,[0,0,0,255,255,0,0,0]);write(&mut d,0,0x20);d.advance(100_000);assert!(d.value(0).is_nan());
        write(&mut d,0,0x80);d.advance(110_000);write(&mut d,0,0xa0);d.advance(161_999);assert!(d.value(0).is_nan());d.advance(162_000);
        assert!((d.value(0)-25.).abs()<0.02);assert_eq!(read(&mut d,0)&0x20,0);
    }
    #[test] fn thermocouple_65_starts_on_cs_rising_and_samples_physical_calibration(){
        let mut d=device(2);write(&mut d,0,0x80);d.advance(10_000);d.gpio(4,false);d.spi(SpiPins{cs:0,..pins()},&[0x80,0xa0],0);d.advance(100_000);assert!(d.value(0).is_nan());
        d.gpio(4,true);d.set(0,80.);d.advance(152_000);assert!((d.value(0)-25.).abs()<0.02);assert_eq!(d.value(5),((resistance_ratio(25.)*100./430.)*32768.).round());
    }
    #[test] fn thermocouple_65_software_mode1_matches_hardware_registers(){
        let mut d=device(2);d.gpio(4,false);byte_gpio(&mut d,0x80);byte_gpio(&mut d,0x91);d.gpio(4,true);assert_eq!(read(&mut d,0),0x91);
        d.gpio(4,false);byte_gpio(&mut d,0);assert_eq!(byte_gpio(&mut d,0),0x91);d.gpio(4,true);assert_eq!(d.level(),None);
    }
    #[test] fn thermocouple_65_filter_continuous_and_bias_off(){
        let mut d=device(2);write(&mut d,0,0x81);d.advance(10_000);write(&mut d,0,0xc1);d.advance(72_499);assert!(d.value(0).is_nan());d.advance(72_500);let g=d.generation();
        d.set(0,70.);d.advance(92_500);assert_eq!(d.generation(),g+1);d.advance(112_500);assert!((d.value(0)-70.).abs()<0.03);
        write(&mut d,0,0);d.advance(200_000);assert_eq!(d.generation(),g+2);
    }
    #[test] fn thermocouple_65_threshold_fault_latches_and_clears(){
        let mut d=device(2);write(&mut d,3,0);write(&mut d,4,2);sample(&mut d);assert_eq!(read(&mut d,7),0x80);assert!(d.value(0).is_nan());assert_eq!(read(&mut d,2)&1,1);
        write(&mut d,0,0x82);assert_eq!(read(&mut d,7),0);assert!(d.value(0).is_nan());write(&mut d,3,255);write(&mut d,4,255);sample(&mut d);assert!(d.value(0).is_finite());
    }
    #[test] fn thermocouple_65_threshold_equality_ignores_reserved_low_bit(){
        for low_bit in [0,1] {let mut d=device(2);sample(&mut d);let raw=((d.value(5) as u16)<<1)|low_bit;
            write(&mut d,3,(raw>>8) as u8);write(&mut d,4,raw as u8);sample(&mut d);assert_eq!(read(&mut d,7),0x80);
            write(&mut d,0,0x82);write(&mut d,3,255);write(&mut d,4,255);write(&mut d,5,(raw>>8) as u8);write(&mut d,6,raw as u8);sample(&mut d);assert_eq!(read(&mut d,7),0x40);
        }
    }
    #[test] fn thermocouple_65_ideal_rc_bias_settling_boundary(){
        let mut d=device(2);write(&mut d,0,0x80);d.advance(999);write(&mut d,0,0xa0);assert!(d.conversion.is_none());
        d.advance(1000);write(&mut d,0,0xa0);assert_eq!(d.conversion.as_ref().unwrap().deadline,53000);
    }
    #[test] fn thermocouple_65_diagnostic_starts_on_deselect(){
        let mut d=device(2);d.set(4,8.);d.gpio(4,false);d.spi(SpiPins{cs:0,..pins()},&[0x80,0x84],0);d.advance(1000);assert_eq!(d.registers[7],0);
        d.gpio(4,true);d.advance(1549);assert_eq!(d.registers[7],0);d.advance(1550);assert_eq!(d.registers[7],8);
    }
    #[test] fn thermocouple_65_diagnostic_and_voltage_fault(){
        let mut d=device(2);d.set(4,0x28 as f64);write(&mut d,0,0x84);d.advance(549);assert_eq!(read(&mut d,7),0);d.advance(550);assert_eq!(read(&mut d,7),0x28);assert_eq!(read(&mut d,0)&0x0c,0);
        d.set(4,4.);write(&mut d,0,0x82);assert_eq!(read(&mut d,7),4);
    }
    #[test] fn thermocouple_calibration_positive_finite_and_quantization(){
        let mut d=device(2);for value in [0.,-1.,f64::NAN,f64::INFINITY]{assert!(!d.set(2,value));assert!(!d.set(3,value));}
        assert!(d.set(2,43.));assert!(d.set(3,10.));d.set(0,-100.);sample(&mut d);assert!((d.value(0)+100.).abs()<0.03);assert_eq!(d.value(6),d.value(5)*43./32768.);
        d.set(2,1.);sample(&mut d);assert_eq!(d.value(5),32767.);assert!(d.value(0).is_nan());
    }
    #[test] fn thermocouple_field_boundaries_reserved_faults_and_missing_fields(){
        for model in [1,2] {let mut d=device(model);for value in [f64::NAN,f64::INFINITY,-1000.,2000.]{assert!(!d.set(0,value));}assert!(!d.set(4,1.5));assert!(!d.set(5,1.));assert!(d.value(99).is_nan());}
        let mut d=device(2);assert!(!d.set(4,1.));assert!(!d.set(4,2.));assert!(!d.set(4,255.));assert!(d.set(4,252.));
    }
    #[test] fn thermocouple_wrong_routes_and_independent_instances(){
        let(mut a,mut b)=(device(1),device(1));b.config.cs=5;b.set(0,90.);a.advance(200_000);b.advance(200_000);
        assert!(a.spi(SpiPins{sclk:4,..pins()},&[],4).is_none());assert!(b.spi(pins(),&[],4).is_none());assert_eq!(a.spi(SpiPins{miso:Some(2),..pins()},&[],4),Some(vec![255;4]));
        assert_eq!(a.value(0),25.);assert_eq!(b.value(0),90.);assert!(b.spi(SpiPins{cs:32,..pins()},&[],4).is_some());
        let mut d=device(2);assert!(d.spi(SpiPins{mosi:8,..pins()},&[0x80,0x80],0).is_none());assert_eq!(d.registers[0],0);
    }
}
