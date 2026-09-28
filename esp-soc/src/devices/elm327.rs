use std::collections::VecDeque;
pub const BAUD: u32 = 38400;
pub struct Elm327 {
    pub tx_pin: u8,
    pub rx_pin: u8,
    input: [f64; 6],
    command: Vec<u8>,
    previous: String,
    overflow: bool,
    last_byte: u64,
    bytes: VecDeque<u8>,
    next_byte: u64,
    echo: bool,
    spaces: bool,
    linefeeds: bool,
    headers: bool,
    long: bool,
    automatic: bool,
    searched: bool,
    timeout_ms: u64,
    pub generation: u32,
    pub readings: [f64; 6],
}
impl Elm327 {
    pub fn new(tx_pin: u8, rx_pin: u8) -> Self {
        Self {
            tx_pin,
            rx_pin,
            input: [800., 0., 85., 50., 0., 1.],
            command: Vec::new(),
            previous: String::new(),
            overflow: false,
            last_byte: 0,
            bytes: VecDeque::new(),
            next_byte: u64::MAX,
            echo: true,
            spaces: true,
            linefeeds: false,
            headers: false,
            long: false,
            automatic: true,
            searched: false,
            timeout_ms: 200,
            generation: 0,
            readings: [f64::NAN; 6],
        }
    }
    fn reset(&mut self) {
        self.echo = true;
        self.spaces = true;
        self.linefeeds = false;
        self.headers = false;
        self.long = false;
        self.automatic = true;
        self.searched = false;
        self.timeout_ms = 200;
        self.readings.fill(f64::NAN);
        self.generation = 0;
    }
    pub fn set(&mut self, field: u32, value: f64) -> bool {
        let (lo, hi) = match field {
            0 => (0., 16383.75),
            1 => (0., 255.),
            2 => (-40., 215.),
            3 | 4 => (0., 100.),
            5 => (0., 1.),
            _ => return false,
        };
        if !value.is_finite() || !(lo..=hi).contains(&value) || (field == 5 && value.fract() != 0.)
        {
            return false;
        }
        self.input[field as usize] = value;
        true
    }
    fn publish(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 || self.generation == u32::MAX {
            self.generation = 1;
        }
    }
    pub fn deadline(&self, hz: u64) -> u64 {
        self.next_byte
            .min(if self.command.is_empty() && !self.overflow {
                u64::MAX
            } else {
                self.last_byte.saturating_add(hz * 20)
            })
    }
    fn respond(&mut self, text: &str, now: u64, hz: u64, delay_ms: u64) {
        let text = if self.linefeeds {
            text.replace('\r', "\r\n")
        } else {
            text.to_owned()
        };
        if text.len() > 512 {
            self.bytes = b"BUFFER FULL\r>".iter().copied().collect();
        } else {
            self.bytes = text.into_bytes().into();
        }
        self.next_byte = now + (hz * delay_ms).div_ceil(1000) + (hz * 10).div_ceil(BAUD as u64);
    }
    pub fn take_byte(&mut self, now: u64, hz: u64) -> Option<u8> {
        if (!self.command.is_empty() || self.overflow)
            && now.saturating_sub(self.last_byte) >= hz * 20
        {
            self.command.clear();
            self.overflow = false;
            self.respond("?\r>", now, hz, 0);
        }
        if now < self.next_byte {
            return None;
        }
        let byte = self.bytes.pop_front()?;
        self.next_byte = if self.bytes.is_empty() {
            u64::MAX
        } else {
            now + (hz * 10).div_ceil(BAUD as u64)
        };
        Some(byte)
    }
    pub fn receive(&mut self, byte: u8, now: u64, hz: u64) {
        if !self.bytes.is_empty() {
            self.command.clear();
            self.overflow = false;
            self.respond("STOPPED\r>", now, hz, 0);
            return;
        }
        if !byte.is_ascii() {
            self.overflow = true;
            self.last_byte = now;
            return;
        }
        if byte == b'\r' {
            let raw = String::from_utf8(std::mem::take(&mut self.command)).unwrap_or_default();
            let echo = if self.echo {
                format!("{raw}\r")
            } else {
                String::new()
            };
            let command = raw
                .chars()
                .filter(|c| !c.is_ascii_whitespace())
                .collect::<String>()
                .to_ascii_uppercase();
            let command = if command.is_empty() {
                self.previous.clone()
            } else {
                self.previous = command.clone();
                command
            };
            let (body, delay) = if self.overflow {
                self.overflow = false;
                ("?".to_owned(), 1)
            } else {
                self.execute(&command)
            };
            self.respond(&format!("{echo}{body}\r>"), now, hz, delay);
        } else if !byte.is_ascii_control() {
            self.last_byte = now;
            if self.command.len() < 64 {
                self.command.push(byte);
            } else {
                self.overflow = true;
            }
        }
    }
    fn execute(&mut self, command: &str) -> (String, u64) {
        if let Some(at) = command.strip_prefix("AT") {
            let mut response = "OK".to_owned();
            let mut delay = 1;
            match at {
                "D" => self.reset(),
                "Z" | "WS" => {
                    self.reset();
                    response = "ELM327 v2.2".into();
                    delay = 100;
                }
                "I" => response = "ELM327 v2.2".into(),
                "E0" => self.echo = false,
                "E1" => self.echo = true,
                "S0" => self.spaces = false,
                "S1" => self.spaces = true,
                "L0" => self.linefeeds = false,
                "L1" => self.linefeeds = true,
                "H0" => self.headers = false,
                "H1" => self.headers = true,
                "AL" => self.long = true,
                "NL" => self.long = false,
                "SPA0" | "TP0" | "TPA0" => {
                    self.automatic = true;
                    self.searched = false;
                }
                "TP6" => {
                    self.automatic = false;
                    self.searched = false;
                }
                "TPA6" => {
                    self.automatic = true;
                    self.searched = false;
                }
                "DPN" => response = if self.automatic { "A6" } else { "6" }.into(),
                "DP" => {
                    response = if self.automatic {
                        "AUTO, ISO 15765-4 (CAN 11/500)"
                    } else {
                        "ISO 15765-4 (CAN 11/500)"
                    }
                    .into()
                }
                "PC" => self.searched = false,
                s if s.len() == 4 && s.starts_with("ST") => match u8::from_str_radix(&s[2..], 16) {
                    Ok(n) => self.timeout_ms = if n == 0 { 200 } else { n as u64 * 4 },
                    Err(_) => response = "?".into(),
                },
                _ => response = "?".into(),
            }
            return (response, delay);
        }
        if command.is_empty() || !command.bytes().all(|b| b.is_ascii_hexdigit()) {
            return ("?".into(), 1);
        }
        let chars = command.len();
        let count = if chars % 2 == 1 {
            u8::from_str_radix(&command[chars - 1..], 16).ok()
        } else {
            None
        };
        let data = &command[..chars - usize::from(count.is_some())];
        if data.len() < 2 || data.len() > 32 || count == Some(0) {
            return ("?".into(), 1);
        }
        let request: Vec<_> = (0..data.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&data[i..i + 2], 16).unwrap())
            .collect();
        if self.input[5] == 0. {
            self.readings.fill(f64::NAN);
            self.readings[5] = 0.;
            self.publish();
            return ("NO DATA".into(), self.timeout_ms);
        }
        let payload = match request.as_slice() {
            [1, 0] => {
                let mut mask = 0u32;
                for pid in [5, 12, 13, 17, 32] {
                    mask |= 1 << (32 - pid);
                }
                let mut p = vec![0x41, 0];
                p.extend(mask.to_be_bytes());
                p
            }
            [1, 0x20] => {
                let mut p = vec![0x41, 0x20];
                p.extend((1u32 << (64 - 47)).to_be_bytes());
                p
            }
            [1, pid] if [5, 12, 13, 17, 47].contains(pid) => {
                let field = match pid {
                    12 => 0,
                    13 => 1,
                    5 => 2,
                    47 => 3,
                    _ => 4,
                };
                let raw = match field {
                    0 => (self.input[0] * 4.).round() as u16,
                    1 => self.input[1].round() as u16,
                    2 => (self.input[2] + 40.).round() as u16,
                    _ => (self.input[field] * 255. / 100.).round() as u16,
                };
                let mut p = vec![0x41, *pid];
                if field == 0 {
                    p.extend(raw.to_be_bytes());
                } else {
                    p.push(raw as u8);
                }
                self.readings[field] = match field {
                    0 => raw as f64 / 4.,
                    1 => raw as f64,
                    2 => raw as f64 - 40.,
                    _ => raw as f64 * 100. / 255.,
                };
                self.readings[5] = 1.;
                self.publish();
                p
            }
            _ => return ("NO DATA".into(), self.timeout_ms),
        };
        let mut pieces: Vec<String> = Vec::new();
        if self.headers {
            pieces.push("7E8".into());
            pieces.push(format!("{:02X}", payload.len()));
        }
        pieces.extend(payload.iter().map(|v| format!("{v:02X}")));
        if self.headers {
            for _ in payload.len() + 1..8 {
                pieces.push("00".into());
            }
        }
        let mut response = pieces.join(if self.spaces { " " } else { "" });
        if self.automatic && !self.searched {
            response = format!("SEARCHING...\r{response}");
        }
        self.searched = true;
        (response, if count == Some(1) { 2 } else { self.timeout_ms })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const HZ: u64 = 1_000_000;
    fn drain(d: &mut Elm327, now: &mut u64) -> String {
        let mut bytes = Vec::new();
        while !d.bytes.is_empty() {
            *now = (*now).max(d.deadline(HZ));
            if let Some(b) = d.take_byte(*now, HZ) {
                bytes.push(b);
            }
        }
        String::from_utf8(bytes).unwrap()
    }
    fn command(d: &mut Elm327, now: &mut u64, s: &str) -> String {
        for b in s.bytes() {
            d.receive(b, *now, HZ);
            *now += 261;
        }
        d.receive(b'\r', *now, HZ);
        drain(d, now)
    }
    fn init(d: &mut Elm327, now: &mut u64) {
        for s in [
            "AT D", "AT Z", "AT E0", "AT S0", "AT AL", "AT ST 00", "AT SP A0",
        ] {
            assert!(command(d, now, s).ends_with(">"));
        }
        assert!(command(d, now, "0100").contains("410008188001"));
    }
    #[test]
    fn elm327_real_initialization_and_five_pid_units() {
        let mut d = Elm327::new(4, 5);
        let mut now = 0;
        init(&mut d, &mut now);
        assert_eq!(d.generation, 0);
        for (f, v) in [1234.75, 87., -12., 60., 20.].into_iter().enumerate() {
            assert!(d.set(f as u32, v));
        }
        for (cmd, want) in [
            ("010C1", "410C134B\r>"),
            ("010D1", "410D57\r>"),
            ("01051", "41051C\r>"),
            ("012F1", "412F99\r>"),
            ("01111", "411133\r>"),
        ] {
            assert_eq!(command(&mut d, &mut now, cmd), want);
        }
        assert_eq!(d.readings, [1234.75, 87., -12., 60., 20., 1.]);
        assert_eq!(d.generation, 5);
        assert_eq!(command(&mut d, &mut now, "0120"), "412000020000\r>");
    }
    #[test]
    fn elm327_echo_case_spaces_headers_linefeeds_and_repeat() {
        let mut d = Elm327::new(4, 5);
        let mut now = 0;
        assert_eq!(command(&mut d, &mut now, "at i"), "at i\rELM327 v2.2\r>");
        command(&mut d, &mut now, "ATE0");
        command(&mut d, &mut now, "ATTP6");
        command(&mut d, &mut now, "ATH1");
        command(&mut d, &mut now, "ATL1");
        assert_eq!(
            command(&mut d, &mut now, "01 0c 1"),
            "7E8 04 41 0C 0C 80 00 00 00\r\n>"
        );
        assert_eq!(
            command(&mut d, &mut now, ""),
            "7E8 04 41 0C 0C 80 00 00 00\r\n>"
        );
        command(&mut d, &mut now, "ATS0");
        assert_eq!(
            command(&mut d, &mut now, "010D1"),
            "7E803410D0000000000\r\n>"
        );
        assert_eq!(
            command(&mut d, &mut now, "ATSP6"),
            "?\r\n>",
            "persistent protocol writes unsupported"
        );
    }
    #[test]
    fn elm327_latency_baud_framing_timeout_and_abort() {
        let mut d = Elm327::new(4, 5);
        let mut now = 0;
        init(&mut d, &mut now);
        for b in b"010C1\r" {
            d.receive(*b, now, HZ);
        }
        let at = d.deadline(HZ);
        assert!(at >= now + 2000);
        assert_eq!(d.take_byte(at - 1, HZ), None);
        assert_eq!(d.take_byte(at, HZ), Some(b'4'));
        assert_eq!(d.deadline(HZ), at + 261);
        d.receive(b'x', at, HZ);
        now = at;
        assert_eq!(drain(&mut d, &mut now), "STOPPED\r>");
        d.receive(b'A', now, HZ);
        let deadline = d.deadline(HZ);
        assert_eq!(deadline, now + 20 * HZ);
        assert_eq!(d.take_byte(deadline, HZ), None);
        now = deadline;
        assert_eq!(drain(&mut d, &mut now), "?\r>");
        command(&mut d, &mut now, "ATST01");
        for b in b"0199\r" {
            d.receive(*b, now, HZ);
        }
        assert_eq!(d.deadline(HZ), now + 4000 + 261);
        assert_eq!(drain(&mut d, &mut now), "NO DATA\r>");
    }
    #[test]
    fn elm327_invalid_input_command_and_bounded_queue() {
        let mut d = Elm327::new(4, 5);
        let mut now = 0;
        for (f, v) in [
            (0, -1.),
            (0, 16384.),
            (1, 256.),
            (2, -41.),
            (2, 216.),
            (3, 101.),
            (4, f64::NAN),
            (5, 0.5),
            (6, 1.),
        ] {
            assert!(!d.set(f, v));
        }
        for cmd in ["ATINVALID", "01GG", "0", "010C0"] {
            assert!(command(&mut d, &mut now, cmd).contains("?\r>"));
        }
        assert!(command(&mut d, &mut now, &"A".repeat(2000)).ends_with("?\r>"));
        assert!(d.command.is_empty());
        assert!(d.bytes.len() <= 512);
        assert!(command(&mut d, &mut now, "ATI").contains("ELM327 v2.2"));
        d.receive(0xff, now, HZ);
        d.receive(b'\r', now, HZ);
        assert!(drain(&mut d, &mut now).ends_with("?\r>"));
    }
    #[test]
    fn elm327_disconnect_reconnect_and_independent_instances() {
        let mut a = Elm327::new(4, 5);
        let mut b = Elm327::new(6, 7);
        let mut now = 0;
        init(&mut a, &mut now);
        init(&mut b, &mut now);
        a.set(0, 4000.);
        b.set(0, 1000.);
        command(&mut a, &mut now, "010C1");
        command(&mut b, &mut now, "010C1");
        assert_eq!(a.readings[0], 4000.);
        assert_eq!(b.readings[0], 1000.);
        a.set(5, 0.);
        assert_eq!(command(&mut a, &mut now, "010C1"), "NO DATA\r>");
        assert!(a.readings[..5].iter().all(|v| v.is_nan()));
        assert_eq!(a.readings[5], 0.);
        assert_eq!(b.readings[0], 1000.);
        a.set(5, 1.);
        assert_eq!(command(&mut a, &mut now, "010C1"), "410C3E80\r>");
        assert_eq!(a.readings[5], 1.);
        command(&mut a, &mut now, "ATZ");
        assert_eq!(a.generation, 0);
        assert!(a.readings.iter().all(|v| v.is_nan()));
        assert_eq!(b.readings[0], 1000.);
        init(&mut a, &mut now);
        command(&mut a, &mut now, "010C1");
        assert_eq!(a.readings[0], 4000.);
    }
}
