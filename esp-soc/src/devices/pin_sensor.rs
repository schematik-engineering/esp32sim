//! Timed, open-drain DHT and DS18B20 wire protocols. Times are CPU cycles.
use std::collections::VecDeque;

#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub model: u8,
    pub id: u8,
    pub pin: u8,
    pub rom: [u8; 8],
}
pub fn crc8(bytes: &[u8]) -> u8 {
    let mut crc = 0u8;
    for &byte in bytes {
        let mut b = byte;
        for _ in 0..8 {
            let mix = (crc ^ b) & 1;
            crc >>= 1;
            if mix != 0 {
                crc ^= 0x8c;
            }
            b >>= 1;
        }
    }
    crc
}
struct Sensor {
    config: Config,
    temperature: f64,
    humidity: f64,
    latched: [f64; 2],
    generation: u32,
    scratch: [u8; 9],
    saved: [u8; 3],
    conversion: Option<u64>,
    sample: f64,
    storage: Option<(u64, bool)>,
}
impl Sensor {
    fn new(config: Config) -> Self {
        let mut scratch = [0x50, 0x05, 75, 70, 0x7f, 0xff, 0x0c, 0x10, 0];
        scratch[8] = crc8(&scratch[..8]);
        Self {
            config,
            temperature: 25.,
            humidity: 50.,
            latched: [f64::NAN; 2],
            generation: 0,
            scratch,
            saved: [75, 70, 0x7f],
            conversion: None,
            sample: 25.,
            storage: None,
        }
    }
    fn latch(&mut self, t: f64, h: f64) {
        self.latched = [t, h];
        self.generation = self.generation.wrapping_add(1).max(1);
    }
    fn refresh_crc(&mut self) {
        self.scratch[8] = crc8(&self.scratch[..8]);
    }
    fn advance(&mut self, now: u64) {
        if let Some((at, copy)) = self.storage {
            if at <= now {
                self.storage = None;
                if copy {
                    self.saved = self.scratch[2..5].try_into().unwrap();
                } else {
                    self.scratch[2..5].copy_from_slice(&self.saved);
                    self.refresh_crc();
                }
            }
        }
        if self.conversion.is_some_and(|at| at <= now) {
            self.conversion = None;
            let shift = 3 - ((self.scratch[4] >> 5) & 3);
            let raw = ((self.sample * 16.).round() as i16) & !((1 << shift) - 1);
            self.scratch[..2].copy_from_slice(&raw.to_le_bytes());
            self.refresh_crc();
            self.latch(raw as f64 / 16., f64::NAN);
        }
    }
}
#[derive(Clone, Debug)]
enum State {
    Rom,
    Match(Vec<u8>),
    Function,
    WriteScratch(Vec<u8>),
    Read(VecDeque<bool>),
    Search {
        bit: u8,
        phase: u8,
        candidates: Vec<usize>,
    },
    Poll,
    Ignore,
}
struct Wire {
    pin: u8,
    devices: Vec<usize>,
    selected: Vec<usize>,
    state: State,
    byte: u8,
    bits: u8,
    master_low: Option<u64>,
    low: bool,
    edges: VecDeque<(u64, bool)>,
    last_dht: Option<u64>,
    dht_latch: Option<(u64, f64, f64)>,
}
pub struct PinSensors {
    sensors: Vec<Sensor>,
    wires: Vec<Wire>,
    hz: u64,
    now: u64,
}
impl PinSensors {
    pub fn new(configs: &[Config], hz: u64) -> Result<Self, String> {
        if configs.len() > 16 || hz < 1_000_000 {
            return Err("invalid pin sensor count or clock".into());
        }
        let mut sensors = Vec::new();
        let mut wires: Vec<Wire> = Vec::new();
        for (index, &c) in configs.iter().enumerate() {
            if c.id >= 16
                || c.pin >= 49
                || !(1..=3).contains(&c.model)
                || configs[..index].iter().any(|p| p.id == c.id)
                || (c.model == 3 && (c.rom[0] != 0x28 || crc8(&c.rom[..7]) != c.rom[7]))
                || (c.model != 3 && c.rom != [0; 8])
            {
                return Err("invalid pin sensor identity, model, pin or ROM CRC".into());
            }
            if let Some(w) = wires.iter_mut().find(|w| w.pin == c.pin) {
                if c.model != 3
                    || w.devices
                        .iter()
                        .any(|&i| configs[i].model != 3 || configs[i].rom == c.rom)
                {
                    return Err("incompatible sensors or duplicate ROM on one wire".into());
                }
                w.devices.push(index);
            } else {
                wires.push(Wire {
                    pin: c.pin,
                    devices: vec![index],
                    selected: Vec::new(),
                    state: State::Rom,
                    byte: 0,
                    bits: 0,
                    master_low: None,
                    low: false,
                    edges: VecDeque::new(),
                    last_dht: None,
                    dht_latch: None,
                });
            }
            sensors.push(Sensor::new(c));
        }
        Ok(Self {
            sensors,
            wires,
            hz,
            now: 0,
        })
    }
    pub fn set(&mut self, id: u8, field: u32, value: f64) -> bool {
        let Some(s) = self.sensors.iter_mut().find(|s| s.config.id == id) else {
            return false;
        };
        if !value.is_finite() {
            return false;
        }
        let bounds = match (s.config.model, field) {
            (1, 0) => (0., 50.),
            (1, 1) => (20., 90.),
            (2, 0) => (-40., 80.),
            (2, 1) => (0., 100.),
            (3, 0) => (-55., 125.),
            _ => return false,
        };
        if !(bounds.0..=bounds.1).contains(&value) {
            return false;
        }
        if field == 0 {
            s.temperature = value;
        } else {
            s.humidity = value;
        }
        true
    }
    pub fn generation(&self, id: u8) -> u32 {
        self.sensors
            .iter()
            .find(|s| s.config.id == id)
            .map_or(u32::MAX, |s| s.generation)
    }
    pub fn value(&self, id: u8, field: u32) -> f64 {
        self.sensors
            .iter()
            .find(|s| s.config.id == id)
            .and_then(|s| s.latched.get(field as usize))
            .copied()
            .unwrap_or(f64::NAN)
    }
    pub fn gpio_drive(&mut self, now: u64, enabled: u64, output: u64) {
        let pins: Vec<_> = self.wires.iter().map(|w| w.pin).collect();
        for pin in pins {
            self.drive(now, pin, enabled & !output & (1u64 << pin) != 0);
        }
    }
    pub fn levels(&self) -> Vec<(u8, bool)> {
        self.wires.iter().map(|w| (w.pin, !w.low)).collect()
    }
    pub fn next_deadline(&self) -> Option<u64> {
        self.wires
            .iter()
            .flat_map(|w| {
                w.edges
                    .front()
                    .map(|e| e.0)
                    .into_iter()
                    .chain(w.dht_latch.map(|e| e.0))
            })
            .chain(
                self.sensors
                    .iter()
                    .flat_map(|s| s.conversion.into_iter().chain(s.storage.map(|v| v.0))),
            )
            .min()
    }
    pub fn advance(&mut self, now: u64) {
        self.now = now;
        for s in &mut self.sensors {
            s.advance(now);
        }
        for w in &mut self.wires {
            while w.edges.front().is_some_and(|e| e.0 <= now) {
                w.low = w.edges.pop_front().unwrap().1;
            }
            if let Some((at, t, h)) = w.dht_latch {
                if at <= now {
                    self.sensors[w.devices[0]].latch(t, h);
                    w.dht_latch = None;
                }
            }
        }
    }
    /// `low` reflects the MCU actively sinking the line, not its sampled input.
    pub fn drive(&mut self, now: u64, pin: u8, low: bool) {
        self.advance(now);
        let Some(w) = self.wires.iter_mut().find(|w| w.pin == pin) else {
            return;
        };
        if low {
            if w.master_low.is_none() {
                w.master_low = Some(now);
            }
            return;
        }
        let Some(start) = w.master_low.take() else {
            return;
        };
        let us = self.hz / 1_000_000;
        let duration = now.saturating_sub(start);
        if self.sensors[w.devices[0]].config.model != 3 {
            let s = &mut self.sensors[w.devices[0]];
            let min = if s.config.model == 1 { 18_000 } else { 1_000 };
            if duration < min * us
                || w.last_dht
                    .is_some_and(|at| now.saturating_sub(at) < 2 * self.hz)
            {
                return;
            }
            w.last_dht = Some(now);
            w.edges.clear();
            w.low = false;
            let (t, h, bytes) = if s.config.model == 1 {
                let t = s.temperature.round();
                let h = s.humidity.round();
                (t, h, [h as u8, 0, t as u8, 0])
            } else {
                let t = (s.temperature * 10.).round() as i16;
                let h = (s.humidity * 10.).round() as u16;
                let raw = t.unsigned_abs() | if t < 0 { 0x8000 } else { 0 };
                (
                    t as f64 / 10.,
                    h as f64 / 10.,
                    [(h >> 8) as u8, h as u8, (raw >> 8) as u8, raw as u8],
                )
            };
            let checksum = bytes.iter().fold(0u8, |a, b| a.wrapping_add(*b));
            let mut at = now + 20 * us;
            w.edges.push_back((at, true));
            at += 80 * us;
            w.edges.push_back((at, false));
            at += 80 * us;
            for byte in bytes.into_iter().chain([checksum]) {
                for bit in (0..8).rev() {
                    w.edges.push_back((at, true));
                    at += 50 * us;
                    w.edges.push_back((at, false));
                    at += if byte & (1 << bit) != 0 {
                        70 * us
                    } else {
                        27 * us
                    };
                }
            }
            w.edges.push_back((at, true));
            at += 50 * us;
            w.edges.push_back((at, false));
            w.dht_latch = Some((at, t, h));
            return;
        }
        // GPIO is observed at 64-cycle execution horizons; Arduino delays also
        // quantize esp_timer_get_time to integer microseconds.
        if duration.saturating_add(64) >= 480 * us {
            w.state = State::Rom;
            w.selected.clear();
            w.byte = 0;
            w.bits = 0;
            w.low = false;
            w.edges.clear();
            w.edges.push_back((now + 30 * us, true));
            w.edges.push_back((now + 150 * us, false));
            return;
        }
        let state = std::mem::replace(&mut w.state, State::Ignore);
        let mut output = None;
        match state {
            State::Read(mut bits) => {
                output = Some(bits.pop_front().unwrap_or(true));
                w.state = State::Read(bits);
            }
            State::Poll => {
                output = Some(w.selected.iter().all(|&i| {
                    self.sensors[i].conversion.is_none() && self.sensors[i].storage.is_none()
                }));
                w.state = State::Poll;
            }
            State::Search {
                mut bit,
                mut phase,
                mut candidates,
            } => {
                if phase < 2 {
                    output = Some(candidates.iter().all(|&i| {
                        let b =
                            self.sensors[i].config.rom[(bit / 8) as usize] & (1 << (bit % 8)) != 0;
                        if phase == 0 {
                            b
                        } else {
                            !b
                        }
                    }));
                    phase += 1;
                } else {
                    let choice = duration <= 15 * us;
                    candidates.retain(|&i| {
                        (self.sensors[i].config.rom[(bit / 8) as usize] & (1 << (bit % 8)) != 0)
                            == choice
                    });
                    bit += 1;
                    phase = 0;
                }
                w.state = if bit == 64 {
                    State::Ignore
                } else {
                    State::Search {
                        bit,
                        phase,
                        candidates,
                    }
                };
            }
            state => {
                w.state = state;
                if duration <= 15 * us {
                    w.byte |= 1 << w.bits;
                }
                w.bits += 1;
                if w.bits == 8 {
                    let byte = w.byte;
                    w.byte = 0;
                    w.bits = 0;
                    Self::command(w, &mut self.sensors, byte, now, self.hz);
                }
            }
        }
        if output == Some(false) {
            w.low = true;
            w.edges.push_back((start + 60 * us, false));
        }
    }
    fn command(w: &mut Wire, sensors: &mut [Sensor], byte: u8, now: u64, hz: u64) {
        let state = std::mem::replace(&mut w.state, State::Ignore);
        w.state = match state {
            State::Rom => match byte {
                0xf0 => State::Search {
                    bit: 0,
                    phase: 0,
                    candidates: w.devices.clone(),
                },
                0x55 => State::Match(Vec::new()),
                0xcc => {
                    w.selected = w.devices.clone();
                    State::Function
                }
                0x33 => {
                    let bytes = (0..8).map(|n| {
                        w.devices
                            .iter()
                            .fold(0xff, |v, &i| v & sensors[i].config.rom[n])
                    });
                    State::Read(Self::bits(bytes))
                }
                _ => State::Ignore,
            },
            State::Match(mut rom) => {
                rom.push(byte);
                if rom.len() == 8 {
                    w.selected = w
                        .devices
                        .iter()
                        .copied()
                        .filter(|&i| sensors[i].config.rom.as_slice() == rom)
                        .collect();
                    State::Function
                } else {
                    State::Match(rom)
                }
            }
            State::Function => match byte {
                0xbe => {
                    let bytes = (0..9).map(|n| {
                        w.selected
                            .iter()
                            .fold(0xff, |v, &i| v & sensors[i].scratch[n])
                    });
                    State::Read(Self::bits(bytes))
                }
                0x44 => {
                    for &i in &w.selected {
                        let s = &mut sensors[i];
                        s.sample = s.temperature;
                        s.conversion =
                            Some(now + ((hz * 750 / 1000) >> (3 - ((s.scratch[4] >> 5) & 3))));
                    }
                    State::Poll
                }
                0x4e => State::WriteScratch(Vec::new()),
                0xb4 => State::Read(VecDeque::from([true])),
                0x48 => {
                    for &i in &w.selected {
                        sensors[i].storage = Some((now + hz / 100, true));
                    }
                    State::Poll
                }
                0xb8 => {
                    for &i in &w.selected {
                        sensors[i].storage = Some((now + hz / 1000, false));
                    }
                    State::Poll
                }
                _ => State::Ignore,
            },
            State::WriteScratch(mut bytes) => {
                bytes.push(byte);
                if bytes.len() == 3 {
                    for &i in &w.selected {
                        let s = &mut sensors[i];
                        s.scratch[2..4].copy_from_slice(&bytes[..2]);
                        s.scratch[4] = (bytes[2] & 0x60) | 0x1f;
                        s.refresh_crc();
                    }
                    State::Ignore
                } else {
                    State::WriteScratch(bytes)
                }
            }
            _ => State::Ignore,
        };
    }
    fn bits(bytes: impl Iterator<Item = u8>) -> VecDeque<bool> {
        bytes
            .flat_map(|byte| (0..8).map(move |bit| byte & (1 << bit) != 0))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config(id: u8) -> Config {
        let mut rom = [0x28, id + 1, 2, 3, 4, 5, 6, 0];
        rom[7] = crc8(&rom[..7]);
        Config {
            model: 3,
            id,
            pin: 4,
            rom,
        }
    }
    fn slot(model: &mut PinSensors, at: &mut u64, bit: bool) -> bool {
        model.drive(*at, 4, true);
        *at += if bit { 6 } else { 60 };
        model.drive(*at, 4, false);
        model.advance(*at + 3);
        let result = model.levels()[0].1;
        *at += 64;
        model.advance(*at);
        result
    }
    fn byte(model: &mut PinSensors, at: &mut u64, byte: u8) {
        for bit in 0..8 {
            slot(model, at, byte & (1 << bit) != 0);
        }
    }
    fn read(model: &mut PinSensors, at: &mut u64) -> u8 {
        let mut v = 0;
        for bit in 0..8 {
            if slot(model, at, true) {
                v |= 1 << bit;
            }
        }
        v
    }
    fn reset(model: &mut PinSensors, at: &mut u64) {
        model.drive(*at, 4, true);
        *at += 500;
        model.drive(*at, 4, false);
        model.advance(*at + 50);
        assert!(!model.levels()[0].1);
        *at += 500;
        model.advance(*at);
        assert!(model.levels()[0].1);
    }
    #[test]
    fn scratchpad_crc_conversion_deadline_and_resolution() {
        let mut m = PinSensors::new(&[config(0)], 1_000_000).unwrap();
        let mut at = 1_000_000;
        assert!(m.set(0, 0, -12.375));
        reset(&mut m, &mut at);
        byte(&mut m, &mut at, 0xcc);
        byte(&mut m, &mut at, 0x44);
        assert!(!slot(&mut m, &mut at, true));
        assert_eq!(m.generation(0), 0);
        m.advance(at + 750_000);
        at += 750_000;
        assert_eq!(m.value(0, 0), -12.375);
        reset(&mut m, &mut at);
        byte(&mut m, &mut at, 0xcc);
        byte(&mut m, &mut at, 0xbe);
        let data: Vec<_> = (0..9).map(|_| read(&mut m, &mut at)).collect();
        assert_eq!(crc8(&data), 0);
        assert_eq!(i16::from_le_bytes([data[0], data[1]]), -198);
        reset(&mut m, &mut at);
        byte(&mut m, &mut at, 0xcc);
        byte(&mut m, &mut at, 0x4e);
        for b in [75, 70, 0x1f] {
            byte(&mut m, &mut at, b);
        }
        reset(&mut m, &mut at);
        byte(&mut m, &mut at, 0xcc);
        byte(&mut m, &mut at, 0x44);
        m.advance(at + 94_000);
        assert_eq!(m.value(0, 0), -12.5);
    }
    #[test]
    fn search_branch_selects_real_rom_and_match_isolates_conversion() {
        let configs = [config(0), config(1)];
        let mut m = PinSensors::new(&configs, 1_000_000).unwrap();
        let mut at = 0;
        reset(&mut m, &mut at);
        byte(&mut m, &mut at, 0xf0);
        for bit in 0..64 {
            let b = slot(&mut m, &mut at, true);
            let inverse = slot(&mut m, &mut at, true);
            let selected = configs[1].rom[bit / 8] & (1 << (bit % 8)) != 0;
            assert!(!(b && inverse));
            slot(&mut m, &mut at, selected);
        }
        reset(&mut m, &mut at);
        byte(&mut m, &mut at, 0x55);
        for b in configs[1].rom {
            byte(&mut m, &mut at, b);
        }
        byte(&mut m, &mut at, 0x44);
        m.advance(at + 750_000);
        assert_eq!(m.generation(0), 0);
        assert_eq!(m.generation(1), 1);
    }
    #[test]
    fn dht_negative_frame_has_wire_checksum_and_bounded_schedule() {
        let mut m = PinSensors::new(
            &[Config {
                model: 2,
                id: 0,
                pin: 4,
                rom: [0; 8],
            }],
            1_000_000,
        )
        .unwrap();
        m.set(0, 0, -3.2);
        m.set(0, 1, 65.4);
        m.drive(0, 4, true);
        m.drive(1100, 4, false);
        assert_eq!(m.wires[0].edges.len(), 84);
        let edges: Vec<_> = m.wires[0].edges.iter().copied().collect();
        let mut data = [0u8; 5];
        for i in 0..40 {
            let start = edges[3 + i * 2].0;
            let end = edges[4 + i * 2].0;
            if end - start == 70 {
                data[i / 8] |= 1 << (7 - i % 8);
            }
        }
        assert_eq!(data, [2, 142, 128, 32, 48]);
        m.advance(20_000);
        assert_eq!(m.value(0, 0), -3.2);
        assert_eq!(m.generation(0), 1);
        m.drive(21_000, 4, true);
        m.drive(23_000, 4, false);
        assert!(m.wires[0].edges.is_empty());
        assert!(!m.set(0, 1, 101.));
    }
    #[test]
    fn configuration_rejects_conflicting_wires_and_bad_roms() {
        let c = config(0);
        assert!(PinSensors::new(&[c, c], 1_000_000).is_err());
        let mut bad = config(1);
        bad.rom[7] ^= 1;
        assert!(PinSensors::new(&[bad], 1_000_000).is_err());
        let d = Config {
            model: 2,
            id: 1,
            pin: 4,
            rom: [0; 8],
        };
        assert!(PinSensors::new(&[c, d], 1_000_000).is_err());
    }
}
