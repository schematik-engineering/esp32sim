use crate::board::BoardEdge;
use std::collections::{BTreeMap, VecDeque};

#[derive(Clone, Debug)]
pub enum InputConfig {
    Ultrasonic {
        id: u8,
        trigger: u8,
        echo: u8,
    },
    Keypad {
        id: u8,
        rows: Vec<u8>,
        columns: Vec<u8>,
    },
    Encoder {
        id: u8,
        a: u8,
        b: u8,
    },
}
impl InputConfig {
    pub fn id(&self) -> u8 {
        match self {
            Self::Ultrasonic { id, .. } | Self::Keypad { id, .. } | Self::Encoder { id, .. } => *id,
        }
    }
    pub fn pins(&self) -> Vec<u8> {
        match self {
            Self::Ultrasonic { trigger, echo, .. } => vec![*trigger, *echo],
            Self::Keypad { rows, columns, .. } => rows.iter().chain(columns).copied().collect(),
            Self::Encoder { a, b, .. } => vec![*a, *b],
        }
    }
    fn valid(&self) -> bool {
        let pins = self.pins();
        let mut used = 0u64;
        if pins.iter().any(|&pin| {
            if pin >= 49 || used & (1u64 << pin) != 0 {
                true
            } else {
                used |= 1u64 << pin;
                false
            }
        }) {
            return false;
        }
        match self {
            Self::Keypad { rows, columns, .. } => {
                (1..=8).contains(&rows.len()) && (1..=8).contains(&columns.len())
            }
            _ => true,
        }
    }
}

struct Ultrasonic {
    id: u8,
    trigger: u8,
    echo: u8,
    distance_mm: u32,
    high_since: Option<u64>,
    rise: Option<u64>,
    fall: Option<u64>,
    level: bool,
}
struct Keypad {
    id: u8,
    rows: Vec<u8>,
    columns: Vec<u8>,
    active: Option<(usize, usize)>,
    queue: VecDeque<(usize, usize)>,
    next: Option<u64>,
}
struct Encoder {
    id: u8,
    a: u8,
    b: u8,
    phase: u8,
    queue: VecDeque<i8>,
    next: Option<u64>,
}
enum Input {
    Ultrasonic(Ultrasonic),
    Keypad(Keypad),
    Encoder(Encoder),
}
impl Input {
    fn next(&self) -> Option<u64> {
        match self {
            Self::Ultrasonic(s) => s.rise.or(s.fall),
            Self::Keypad(k) => k.next,
            Self::Encoder(e) => e.next,
        }
    }
}

pub struct InputDevices {
    devices: Vec<Input>,
    hz: u64,
    cycle: u64,
    enabled: u64,
    output: u64,
    managed: u64,
    levels: BTreeMap<u8, bool>,
    edges: Vec<BoardEdge>,
}
impl InputDevices {
    pub fn owns_input(&self,pin:u8)->bool {pin<64 && (self.managed&(1u64<<pin)!=0 || self.devices.iter().any(|d|matches!(d,Input::Ultrasonic(u) if u.trigger==pin)))}

    pub fn new(configs: &[InputConfig], hz: u64) -> Result<Self, String> {
        if configs.len() > 16 || hz == 0 || hz > 1_000_000_000 {
            return Err("invalid input device count or clock".into());
        }
        let mut devices = Vec::new();
        let mut managed = 0u64;
        for (index, config) in configs.iter().enumerate() {
            if !config.valid() || configs[..index].iter().any(|c| c.id() == config.id()) {
                return Err("invalid input wiring or duplicate identity".into());
            }
            devices.push(match config {
                InputConfig::Ultrasonic { id, trigger, echo } => {
                    managed |= 1u64 << echo;
                    Input::Ultrasonic(Ultrasonic {
                        id: *id,
                        trigger: *trigger,
                        echo: *echo,
                        distance_mm: 300,
                        high_since: None,
                        rise: None,
                        fall: None,
                        level: false,
                    })
                }
                InputConfig::Keypad { id, rows, columns } => {
                    for pin in rows.iter().chain(columns) {
                        managed |= 1u64 << pin;
                    }
                    Input::Keypad(Keypad {
                        id: *id,
                        rows: rows.clone(),
                        columns: columns.clone(),
                        active: None,
                        queue: VecDeque::new(),
                        next: None,
                    })
                }
                InputConfig::Encoder { id, a, b } => {
                    managed |= 1u64 << a | 1u64 << b;
                    Input::Encoder(Encoder {
                        id: *id,
                        a: *a,
                        b: *b,
                        phase: 0,
                        queue: VecDeque::new(),
                        next: None,
                    })
                }
            });
        }
        let mut result = Self {
            devices,
            hz,
            cycle: 0,
            enabled: 0,
            output: 0,
            managed,
            levels: BTreeMap::new(),
            edges: Vec::new(),
        };
        result.resolve();
        result.edges.clear();
        Ok(result)
    }
    fn us(&self, us: u64) -> u64 {
        (self.hz * us).div_ceil(1_000_000).max(1)
    }
    pub fn distance_mm(&mut self, id: u8, value: u32) -> bool {
        if value > 4000 {
            return false;
        }
        for device in &mut self.devices {
            if let Input::Ultrasonic(s) = device {
                if s.id == id {
                    s.distance_mm = value;
                    return true;
                }
            }
        }
        false
    }
    pub fn keypad_press(&mut self, id: u8, row: usize, column: usize) -> bool {
        let hold = self.us(150_000);
        for device in &mut self.devices {
            if let Input::Keypad(k) = device {
                if k.id == id {
                    if row >= k.rows.len() || column >= k.columns.len() || k.queue.len() >= 32 {
                        return false;
                    }
                    if k.next.is_none() {
                        k.active = Some((row, column));
                        k.next = Some(self.cycle.saturating_add(hold));
                    } else {
                        k.queue.push_back((row, column));
                    }
                    self.resolve();
                    return true;
                }
            }
        }
        false
    }
    /// Quarter steps match the encoder library's raw transition count; four make one detent.
    pub fn encoder_steps(&mut self, id: u8, steps: i32) -> bool {
        let count = steps.unsigned_abs() as usize;
        let step = self.us(2000);
        for device in &mut self.devices {
            if let Input::Encoder(e) = device {
                if e.id == id {
                    if count > 1024 || e.queue.len() + count > 1024 {
                        return false;
                    }
                    e.queue
                        .extend(std::iter::repeat(steps.signum() as i8).take(count));
                    if count > 0 && e.next.is_none() {
                        e.next = Some(self.cycle.saturating_add(step));
                    }
                    return true;
                }
            }
        }
        false
    }
    pub fn next_deadline(&self) -> Option<u64> {
        self.devices.iter().filter_map(Input::next).min()
    }
    pub fn advance_to(&mut self, cycle: u64) {
        if cycle < self.cycle {
            return;
        }
        let hold = self.us(150_000);
        let gap = self.us(50_000);
        let step = self.us(2000);
        while let Some(next) = self.next_deadline().filter(|&next| next <= cycle) {
            self.cycle = next;
            for device in &mut self.devices {
                if device.next() == Some(next) {
                    match device {
                        Input::Ultrasonic(s) => {
                            if s.rise.take().is_some() {
                                s.level = true;
                            } else {
                                s.fall = None;
                                s.level = false;
                            }
                        }
                        Input::Keypad(k) => {
                            if k.active.take().is_some() {
                                k.next = Some(next.saturating_add(gap));
                            } else {
                                k.active = k.queue.pop_front();
                                k.next = k.active.map(|_| next.saturating_add(hold));
                            }
                        }
                        Input::Encoder(e) => {
                            if let Some(direction) = e.queue.pop_front() {
                                e.phase = ((e.phase as i8 + direction).rem_euclid(4)) as u8;
                            }
                            e.next = (!e.queue.is_empty()).then_some(next.saturating_add(step));
                        }
                    }
                }
            }
            self.resolve();
        }
        self.cycle = cycle;
    }
    pub fn gpio_drive(&mut self, cycle: u64, enabled: u64, output: u64) {
        self.advance_to(cycle);
        let minimum = self.us(10);
        let delay = self.us(100);
        for device in &mut self.devices {
            if let Input::Ultrasonic(s) = device {
                let high = enabled & output & (1u64 << s.trigger) != 0;
                if high && s.high_since.is_none() {
                    s.high_since = Some(cycle);
                }
                if !high {
                    if let Some(start) = s.high_since.take() {
                        if cycle.saturating_sub(start) >= minimum
                            && s.distance_mm != 0
                            && s.next().is_none()
                        {
                            let rise = cycle.saturating_add(delay);
                            // HC-SR04's nominal echo width is 58 microseconds per centimetre.
                            let width = (self.hz * s.distance_mm as u64 * 58)
                                .div_ceil(10_000_000)
                                .max(1);
                            s.rise = Some(rise);
                            s.fall = Some(rise.saturating_add(width));
                        }
                    }
                }
            }
        }
        self.enabled = enabled;
        self.output = output;
        self.resolve();
    }
    fn resolve(&mut self) {
        let mut levels = BTreeMap::new();
        let mut drive = |pin, value| {
            levels
                .entry(pin)
                .and_modify(|old| *old &= value)
                .or_insert(value);
        };
        for device in &self.devices {
            match device {
                Input::Ultrasonic(s) => drive(s.echo, s.level),
                Input::Encoder(e) => {
                    let phase = [3, 2, 0, 1][e.phase as usize];
                    drive(e.a, phase & 1 != 0);
                    drive(e.b, phase & 2 != 0);
                }
                Input::Keypad(k) => {
                    if let Some((row, column)) = k.active {
                        let (r, c) = (k.rows[row], k.columns[column]);
                        if self.enabled & (1u64 << c) != 0 && self.enabled & (1u64 << r) == 0 {
                            drive(r, self.output & (1u64 << c) != 0);
                        }
                        if self.enabled & (1u64 << r) != 0 && self.enabled & (1u64 << c) == 0 {
                            drive(c, self.output & (1u64 << r) != 0);
                        }
                    }
                }
            }
        }
        for (&pin, &level) in &levels {
            if self.levels.get(&pin) != Some(&level) {
                self.edges.push(BoardEdge {
                    cycle: self.cycle,
                    pin,
                    level,
                });
            }
        }
        self.levels = levels;
    }
    pub fn input_levels(&self) -> Vec<(u8, bool)> {
        self.levels
            .iter()
            .map(|(&pin, &level)| (pin, level))
            .collect()
    }
    pub fn released_inputs(&self) -> Vec<u8> {
        (0..49)
            .filter(|pin| self.managed & (1u64 << pin) != 0 && !self.levels.contains_key(pin))
            .collect()
    }
    pub fn take_edges(&mut self) -> Vec<BoardEdge> {
        std::mem::take(&mut self.edges)
    }
}
impl Ultrasonic {
    fn next(&self) -> Option<u64> {
        self.rise.or(self.fall)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sonar() -> InputConfig {
        InputConfig::Ultrasonic {
            id: 1,
            trigger: 4,
            echo: 5,
        }
    }
    #[test]
    fn echo_requires_driven_trigger_and_reports_real_elapsed_pulse() {
        let mut inputs = InputDevices::new(&[sonar()], 1_000_000).unwrap();
        assert!(inputs.distance_mm(1, 1000));
        inputs.gpio_drive(0, 0, 1 << 4);
        inputs.gpio_drive(20, 0, 0);
        assert_eq!(inputs.next_deadline(), None);
        inputs.gpio_drive(30, 1 << 4, 1 << 4);
        inputs.gpio_drive(39, 1 << 4, 0);
        assert_eq!(inputs.next_deadline(), None);
        inputs.gpio_drive(50, 1 << 4, 1 << 4);
        inputs.gpio_drive(60, 1 << 4, 0);
        assert_eq!(inputs.next_deadline(), Some(160));
        inputs.advance_to(160);
        assert_eq!(inputs.input_levels(), vec![(5, true)]);
        inputs.advance_to(5960);
        assert_eq!(inputs.input_levels(), vec![(5, false)]);
        let edges = inputs.take_edges();
        assert_eq!(
            edges
                .iter()
                .map(|e| (e.cycle, e.pin, e.level))
                .collect::<Vec<_>>(),
            [(160, 5, true), (5960, 5, false)]
        );
        assert!(inputs.distance_mm(1, 0));
        inputs.gpio_drive(6000, 1 << 4, 1 << 4);
        inputs.gpio_drive(6010, 1 << 4, 0);
        assert_eq!(inputs.next_deadline(), None);
    }
    #[test]
    fn keypad_follows_column_scanning_and_releases_floating_rows() {
        let mut inputs = InputDevices::new(
            &[InputConfig::Keypad {
                id: 2,
                rows: vec![1, 2],
                columns: vec![3, 4],
            }],
            1_000_000,
        )
        .unwrap();
        assert!(inputs.keypad_press(2, 1, 0));
        assert!(inputs.input_levels().is_empty());
        inputs.gpio_drive(10, 1 << 4, 0);
        assert!(inputs.input_levels().is_empty());
        inputs.gpio_drive(20, 1 << 3, 0);
        assert_eq!(inputs.input_levels(), vec![(2, false)]);
        inputs.gpio_drive(30, 1 << 3, 1 << 3);
        assert_eq!(inputs.input_levels(), vec![(2, true)]);
        inputs.gpio_drive(40, 0, 0);
        assert!(inputs.input_levels().is_empty());
        assert!(inputs.released_inputs().contains(&2));
        inputs.gpio_drive(50, 1 << 3, 0);
        inputs.advance_to(150000);
        assert!(inputs.input_levels().is_empty());
    }
    #[test]
    fn encoders_preserve_identity_and_emit_ordered_quadrature_between_slices() {
        let mut inputs = InputDevices::new(
            &[
                InputConfig::Encoder { id: 3, a: 6, b: 7 },
                InputConfig::Encoder { id: 4, a: 8, b: 9 },
            ],
            1_000_000,
        )
        .unwrap();
        assert!(inputs.encoder_steps(3, 4));
        assert!(inputs.encoder_steps(4, -4));
        assert!(!inputs.encoder_steps(3, 1024));
        inputs.advance_to(8000);
        let edges = inputs.take_edges();
        assert_eq!(
            edges
                .iter()
                .filter(|e| e.pin < 8)
                .map(|e| (e.cycle, e.pin, e.level))
                .collect::<Vec<_>>(),
            [
                (2000, 6, false),
                (4000, 7, false),
                (6000, 6, true),
                (8000, 7, true)
            ]
        );
        assert_eq!(
            edges
                .iter()
                .filter(|e| e.pin >= 8)
                .map(|e| e.pin)
                .collect::<Vec<_>>(),
            [9, 8, 9, 8]
        );
        assert!(inputs.input_levels().iter().all(|(_, level)| *level));
    }
}
