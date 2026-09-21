use std::collections::VecDeque;
const HEADER: [u8; 4] = [0xfd, 0xfc, 0xfb, 0xfa];
const FOOTER: [u8; 4] = [4, 3, 2, 1];
const BAUD: [u32; 8] = [9600, 19200, 38400, 57600, 115200, 230400, 256000, 460800];
#[derive(Clone)]
struct Config {
    max: [u8; 2],
    sensitivity: [[u8; 9]; 2],
    hold: u16,
    baud: u32,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            max: [8, 8],
            sensitivity: [
                [50, 50, 40, 30, 20, 15, 15, 15, 15],
                [100, 100, 40, 40, 30, 30, 20, 20, 20],
            ],
            hold: 5,
            baud: 256000,
        }
    }
}
pub struct Radar {
    pub tx_pin: u8,
    pub rx_pin: u8,
    pub baud: u32,
    input: [u16; 6],
    pub generation: u32,
    pub readings: [f64; 7],
    config: Config,
    pending_config: Config,
    command: Vec<u8>,
    bytes: VecDeque<u8>,
    next_byte: u64,
    next_report: u64,
    configuring: bool,
    engineering: bool,
    restart: bool,
    last_seen: Option<u64>,
    last_target: u8,
    last_distance: u16,
}
impl Radar {
    pub fn new(tx_pin: u8, rx_pin: u8, now: u64) -> Self {
        Self {
            tx_pin,
            rx_pin,
            baud: 256000,
            input: [0, 0, 225, 225, 80, 80],
            generation: 0,
            readings: [f64::NAN; 7],
            config: Config::default(),
            pending_config: Config::default(),
            command: Vec::with_capacity(74),
            bytes: VecDeque::with_capacity(512),
            next_byte: now,
            next_report: now,
            configuring: false,
            engineering: false,
            restart: false,
            last_seen: None,
            last_target: 0,
            last_distance: 0,
        }
    }
    pub fn set(&mut self, field: u32, value: f64) -> bool {
        let max = match field {
            0 | 1 => 1,
            2 | 3 => 600,
            4 | 5 => 100,
            _ => return false,
        };
        if !value.is_finite() || value.fract() != 0. || value < 0. || value > max as f64 {
            return false;
        }
        self.input[field as usize] = value as u16;
        true
    }
    pub fn deadline(&self) -> u64 {
        if self.bytes.is_empty() {
            self.next_report
        } else {
            self.next_byte
        }
    }
    fn enqueue(&mut self, frame: &[u8], now: u64, hz: u64) {
        if self.bytes.len() + frame.len() > 512 {
            return;
        }
        if self.bytes.is_empty() {
            self.next_byte = now + (hz * 10).div_ceil(self.baud as u64);
        }
        self.bytes.extend(frame);
    }
    fn framed(header: [u8; 4], body: &[u8], footer: [u8; 4]) -> Vec<u8> {
        let mut out = header.to_vec();
        out.extend((body.len() as u16).to_le_bytes());
        out.extend(body);
        out.extend(footer);
        out
    }
    pub fn receive(&mut self, byte: u8, now: u64, hz: u64) {
        if self.command.len() < 4 {
            if byte == HEADER[self.command.len()] {
                self.command.push(byte);
            } else {
                self.command.clear();
                if byte == HEADER[0] {
                    self.command.push(byte);
                }
            }
            return;
        }
        self.command.push(byte);
        if self.command.len() < 6 {
            return;
        }
        let size = u16::from_le_bytes([self.command[4], self.command[5]]) as usize;
        if !(2..=64).contains(&size) {
            self.command.clear();
            return;
        }
        if self.command.len() < size + 10 {
            return;
        }
        let command = std::mem::take(&mut self.command);
        if command[size + 6..] != FOOTER {
            return;
        }
        self.execute(&command[6..size + 6], now, hz);
    }
    fn execute(&mut self, body: &[u8], now: u64, hz: u64) {
        let op = u16::from_le_bytes([body[0], body[1]]);
        if op != 0xff && !self.configuring {
            return;
        }
        let data = &body[2..];
        let mut result = vec![op as u8, 1, 0, 0];
        let mut valid = true;
        match op {
            0xff if data == [1, 0] => {
                self.configuring = true;
                result.extend([1, 0, 64, 0]);
            }
            0xfe if data.is_empty() => self.configuring = false,
            0xa0 if data.is_empty() => result.extend([0, 0, 0, 0, 0, 0, 0, 0]),
            0x61 if data.is_empty() => {
                result.extend([0xaa, 8, self.config.max[0], self.config.max[1]]);
                result.extend(self.config.sensitivity[0]);
                result.extend(self.config.sensitivity[1]);
                result.extend(self.config.hold.to_le_bytes());
            }
            0x60 | 0x64 if data.len() == 18 => {
                let mut values = [0u32; 3];
                let mut mask = 0;
                for chunk in data.chunks_exact(6) {
                    let key = u16::from_le_bytes([chunk[0], chunk[1]]) as usize;
                    if key > 2 || mask & (1 << key) != 0 {
                        valid = false;
                        break;
                    }
                    mask |= 1 << key;
                    values[key] = u32::from_le_bytes(chunk[2..6].try_into().unwrap());
                }
                if valid && op == 0x60 {
                    valid = (2..=8).contains(&values[0])
                        && (2..=8).contains(&values[1])
                        && values[2] <= 65535;
                    if valid {
                        self.config.max = [values[0] as u8, values[1] as u8];
                        self.config.hold = values[2] as u16;
                    }
                } else if valid {
                    valid = (values[0] <= 8 || values[0] == 65535)
                        && values[1] <= 100
                        && values[2] <= 100;
                    if valid {
                        for gate in 0..9 {
                            if values[0] == 65535 || values[0] == gate as u32 {
                                self.config.sensitivity[0][gate] = values[1] as u8;
                                if gate >= 2 {
                                    self.config.sensitivity[1][gate] = values[2] as u8;
                                }
                            }
                        }
                    }
                }
                if valid {
                    self.pending_config.max = self.config.max;
                    self.pending_config.hold = self.config.hold;
                    self.pending_config.sensitivity = self.config.sensitivity;
                }
            }
            0x62 if data.is_empty() => self.engineering = true,
            0x63 if data.is_empty() => self.engineering = false,
            0xa1 if data.len() == 2 => {
                let index = u16::from_le_bytes([data[0], data[1]]) as usize;
                if let Some(baud) = index.checked_sub(1).and_then(|i| BAUD.get(i)) {
                    self.pending_config.baud = *baud;
                } else {
                    valid = false;
                }
            }
            0xa2 if data.is_empty() => self.pending_config = Config::default(),
            0xa3 if data.is_empty() => {
                self.restart = true;
                self.configuring = false;
            }
            _ => valid = false,
        }
        if !valid {
            result = vec![op as u8, 1, 1, 0];
        }
        self.enqueue(&Self::framed(HEADER, &result, FOOTER), now, hz);
    }
    fn report(&mut self, now: u64, hz: u64) -> Vec<u8> {
        let mut target = 0;
        let mut distance = 601;
        for i in 0..2 {
            let gate = (self.input[i + 2] / 75).min(8) as usize;
            if self.input[i] != 0
                && gate <= self.config.max[i] as usize
                && self.input[i + 4] > self.config.sensitivity[i][gate] as u16
            {
                target |= 1 << i;
                distance = distance.min(self.input[i + 2]);
            }
        }
        if target != 0 {
            self.last_seen = Some(now);
            self.last_target = target;
            self.last_distance = distance;
        } else if self
            .last_seen
            .is_some_and(|at| now < at + self.config.hold as u64 * hz)
        {
            target = self.last_target;
            distance = self.last_distance;
        }
        if target == 0 {
            distance = 0;
        }
        let mut body = vec![if self.engineering { 1 } else { 2 }, 0xaa, target];
        for i in 0..2 {
            body.extend(self.input[i + 2].to_le_bytes());
            body.push(if self.input[i] != 0 {
                self.input[i + 4] as u8
            } else {
                0
            });
        }
        body.extend(distance.to_le_bytes());
        self.readings = [
            (target & 1) as f64,
            ((target >> 1) & 1) as f64,
            self.input[2] as f64,
            self.input[3] as f64,
            body[5] as f64,
            body[8] as f64,
            distance as f64,
        ];
        self.generation = if self.generation >= u32::MAX - 1 {
            1
        } else {
            self.generation + 1
        };

        if self.engineering {
            body.extend(self.config.max);
            for i in 0..2 {
                for gate in 0..9 {
                    body.push(
                        if self.input[i] != 0 && gate == (self.input[i + 2] / 75).min(8) as usize {
                            self.input[i + 4] as u8
                        } else {
                            0
                        },
                    );
                }
            }
        }
        body.extend([0x55, 0]);
        Self::framed([0xf4, 0xf3, 0xf2, 0xf1], &body, [0xf8, 0xf7, 0xf6, 0xf5])
    }
    pub fn take_byte(&mut self, now: u64, hz: u64) -> Option<u8> {
        if self.bytes.is_empty() && self.restart {
            self.restart = false;
            self.config = self.pending_config.clone();
            self.baud = self.config.baud;
            self.engineering = false;
            self.last_seen = None;
            self.next_report = now + hz * 8 / 10;
            self.command.clear();
        }
        if self.bytes.is_empty() && now >= self.next_report {
            let frame = self.report(now, hz);
            self.enqueue(&frame, now, hz);
            self.next_report = now + hz / 10;
        }
        if now < self.next_byte {
            return None;
        }
        let byte = self.bytes.pop_front()?;
        self.next_byte += (hz * 10).div_ceil(self.baud as u64);
        Some(byte)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn command(r: &mut Radar, op: u16, data: &[u8]) {
        let mut body = op.to_le_bytes().to_vec();
        body.extend(data);
        for byte in Radar::framed(HEADER, &body, FOOTER) {
            r.receive(byte, 0, 1_000_000);
        }
    }
    #[test]
    fn commands_bounds_presence_threshold_hold_and_engineering() {
        let mut r = Radar::new(4, 5, 0);
        command(&mut r, 0xa0, &[]);
        assert!(r.bytes.is_empty());
        command(&mut r, 0xff, &[1, 0]);
        assert_eq!(r.bytes.len(), 18);
        r.bytes.clear();
        command(&mut r, 0xa0, &[]);
        assert_eq!(r.bytes.len(), 22);
        r.bytes.clear();
        assert!(!r.set(0, 2.));
        assert!(!r.set(2, 601.));
        assert!(!r.set(4, f64::NAN));
        r.set(0, 1.);
        let frame = r.report(0, 1_000_000);
        assert_eq!(frame[8], 1);
        assert_eq!(&frame[15..17], &225u16.to_le_bytes());
        r.set(0, 0.);
        assert_eq!(r.report(4_999_999, 1_000_000)[8], 1);
        assert_eq!(r.report(5_000_000, 1_000_000)[8], 0);
        command(&mut r, 0x62, &[]);
        r.set(1, 1.);
        let frame = r.report(6_000_000, 1_000_000);
        assert_eq!(frame[6], 1);
        assert_eq!(frame[8], 2);
        assert_eq!(frame[28 + 3], 80);
        for _ in 0..1000 {
            command(&mut r, 0xa0, &[]);
        }
        assert!(r.bytes.len() <= 512);
        for _ in 0..1000 {
            r.receive(0xff, 0, 1_000_000);
        }
        assert!(r.command.len() <= 74);
    }
    #[test]
    fn separate_instances_and_staged_baud_survive_configuration() {
        let mut a = Radar::new(4, 5, 0);
        assert_eq!(a.generation, 0);
        assert!(a.readings[6].is_nan());
        let mut b = Radar::new(6, 7, 0);
        a.set(0, 1.);
        b.set(1, 1.);
        b.set(3, 350.);
        assert_eq!(a.report(0, 1_000_000)[8], 1);
        assert_eq!(a.generation, 1);
        assert_eq!(a.readings[6], 225.);
        assert_eq!(b.report(0, 1_000_000)[8], 2);
        command(&mut a, 0xff, &[1, 0]);
        command(&mut a, 0xa1, &[5, 0]);
        command(
            &mut a,
            0x60,
            &[0, 0, 8, 0, 0, 0, 1, 0, 8, 0, 0, 0, 2, 0, 0, 0, 0, 0],
        );
        assert_eq!(a.baud, 256000);
        assert_eq!(a.pending_config.baud, 115200);
        command(&mut a, 0xa3, &[]);
        a.bytes.clear();
        a.take_byte(1000, 1_000_000);
        assert_eq!(a.baud, 115200);
        assert_eq!(b.baud, 256000);
        assert_eq!(a.config.hold, 0);
        b.set(1, 0.);
        assert_eq!(b.report(1_000_000, 1_000_000)[8], 2);
        command(&mut b, 0xff, &[1, 0]);
        command(
            &mut b,
            0x60,
            &[0, 0, 8, 0, 0, 0, 1, 0, 8, 0, 0, 0, 2, 0, 0, 0, 0, 0],
        );
        assert_eq!(b.report(1_000_000, 1_000_000)[8], 0);
    }
}
