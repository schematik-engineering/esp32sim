use std::collections::VecDeque;

pub const BAUD: u32 = 9600;

pub struct Pzem {
    pub tx_pin: u8,
    pub rx_pin: u8,
    current_range: u8,
    address: u8,
    alarm_watts: u16,
    input: [f64; 4],
    energy_wh: f64,
    energy_at: u64,
    command: Vec<u8>,
    last_byte: u64,
    bytes: VecDeque<u8>,
    next_byte: u64,
    pub generation: u32,
    pub readings: [f64; 7],
}

fn crc(bytes: &[u8]) -> u16 {
    let mut crc = 0xffffu16;
    for byte in bytes {
        crc ^= *byte as u16;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xa001
            } else {
                crc >> 1
            };
        }
    }
    crc
}

impl Pzem {
    pub fn new(tx_pin: u8, rx_pin: u8, current_range: u8, address: u8, now: u64) -> Self {
        Self {
            tx_pin,
            rx_pin,
            current_range,
            address,
            alarm_watts: 230 * current_range as u16,
            input: [230., 1., 50., 1.],
            energy_wh: 0.,
            energy_at: now,
            command: Vec::with_capacity(8),
            last_byte: now,
            bytes: VecDeque::with_capacity(25),
            next_byte: u64::MAX,
            generation: 0,
            readings: [f64::NAN; 7],
        }
    }
    fn current(&self) -> f64 {
        if self.input[1] < if self.current_range == 10 { 0.01 } else { 0.02 } {
            0.
        } else {
            self.input[1]
        }
    }
    fn power(&self) -> f64 {
        let watts = self.input[0] * self.current() * self.input[3];
        if watts < 0.4 {
            0.
        } else {
            watts.min(230. * self.current_range as f64)
        }
    }
    fn integrate(&mut self, now: u64, hz: u64) {
        self.energy_wh = (self.energy_wh
            + self.power() * now.saturating_sub(self.energy_at) as f64 / hz as f64 / 3600.)
            .min(9_999_990.);
        self.energy_at = now;
    }
    pub fn set(&mut self, field: u32, value: f64, now: u64, hz: u64) -> bool {
        let (min, max) = match field {
            0 => (80., 260.),
            1 => (0., self.current_range as f64),
            2 => (45., 65.),
            3 => (0., 1.),
            _ => return false,
        };
        if !value.is_finite() || !(min..=max).contains(&value) {
            return false;
        }
        self.integrate(now, hz);
        self.input[field as usize] = value;
        true
    }
    pub fn deadline(&self) -> u64 {
        self.next_byte
    }
    pub fn take_byte(&mut self, now: u64, hz: u64) -> Option<u8> {
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
            return;
        }
        // RTU framing resumes after at least 3.5 character times of silence.
        if now.saturating_sub(self.last_byte) > (hz * 35).div_ceil(BAUD as u64) {
            self.command.clear();
        }
        self.last_byte = now;
        self.command.push(byte);
        if self.command.len() < 2 {
            return;
        }
        let length = match self.command[1] {
            0x42 => 4,
            0x41 => 6,
            _ => 8,
        };
        if self.command.len() < length {
            return;
        }
        let command = std::mem::replace(&mut self.command, Vec::with_capacity(8));
        if crc(&command) != 0 || ![0, self.address, 0xf8].contains(&command[0]) {
            return;
        }
        self.execute(&command, now, hz);
    }
    fn measurements(&mut self, now: u64, hz: u64) -> [u16; 10] {
        self.integrate(now, hz);
        let voltage = (self.input[0] * 10.).round() as u16;
        let current = (self.current() * 1000.).round() as u32;
        let power = (self.power() * 10.).round() as u32;
        let energy = self.energy_wh.floor() as u32;
        let frequency = (self.input[2] * 10.).round() as u16;
        let pf = (self.input[3] * 100.).round() as u16;
        let alarm = if power > self.alarm_watts as u32 * 10 {
            0xffff
        } else {
            0
        };
        self.readings = [
            voltage as f64 / 10.,
            current as f64 / 1000.,
            power as f64 / 10.,
            energy as f64 / 1000.,
            frequency as f64 / 10.,
            pf as f64 / 100.,
            f64::from(alarm != 0),
        ];
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 || self.generation == u32::MAX {
            self.generation = 1;
        }
        [
            voltage,
            current as u16,
            (current >> 16) as u16,
            power as u16,
            (power >> 16) as u16,
            energy as u16,
            (energy >> 16) as u16,
            frequency,
            pf,
            alarm,
        ]
    }
    fn execute(&mut self, command: &[u8], now: u64, hz: u64) {
        let (address, op) = (command[0], command[1]);
        let mut response = vec![address, op];
        let mut exception = 0;
        let mut delay = (hz * 35).div_ceil(BAUD as u64);
        match op {
            3 | 4 if address != 0 => {
                let start = u16::from_be_bytes([command[2], command[3]]) as usize;
                let count = u16::from_be_bytes([command[4], command[5]]) as usize;
                if count == 0 || count > 10 {
                    exception = 3;
                } else if (op == 4 && start + count <= 10)
                    || (op == 3 && start >= 1 && start + count <= 3)
                {
                    let regs = if op == 4 {
                        self.measurements(now, hz).to_vec()
                    } else {
                        vec![0, self.alarm_watts, self.address as u16]
                    };
                    response.push((count * 2) as u8);
                    for value in &regs[start..start + count] {
                        response.extend(value.to_be_bytes());
                    }
                } else {
                    exception = 2;
                }
            }
            6 => {
                let register = u16::from_be_bytes([command[2], command[3]]);
                let value = u16::from_be_bytes([command[4], command[5]]);
                match register {
                    1 => self.alarm_watts = value,
                    2 if (1..=247).contains(&value) => self.address = value as u8,
                    2 => exception = 3,
                    _ => exception = 2,
                }
                response.extend(&command[2..6]);
            }
            0x42 => {
                self.energy_wh = 0.;
                self.energy_at = now;
            }
            0x41 if address == 0xf8 && command[2..4] == [0x37, 0x21] => {
                delay = hz * 7 / 2;
                response.extend([0x37, 0x21]);
            }
            0x41 => exception = 3,
            _ => exception = 1,
        }
        if address == 0 {
            return;
        }
        if exception != 0 {
            response = vec![address, op | 0x80, exception];
        }
        response.extend(crc(&response).to_le_bytes());
        self.bytes.extend(response);
        self.next_byte = now + delay + (hz * 10).div_ceil(BAUD as u64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const HZ: u64 = 1_000_000;
    fn query(m: &mut Pzem, body: &[u8], now: u64) -> Vec<u8> {
        let mut request = body.to_vec();
        request.extend(crc(body).to_le_bytes());
        for b in request {
            m.receive(b, now, HZ);
        }
        let mut reply = Vec::new();
        while m.deadline() != u64::MAX {
            reply.push(m.take_byte(m.deadline(), HZ).unwrap());
        }
        reply
    }
    #[test]
    fn physical_values_crc_low_word_order_energy_alarm_address_and_reset() {
        let mut m = Pzem::new(4, 5, 100, 1, 0);
        assert!(m.set(0, 240., 0, HZ));
        assert!(m.set(1, 80., 0, HZ));
        assert!(m.set(3, 0.75, 0, HZ));
        let reply = query(&mut m, &[1, 4, 0, 0, 0, 10], HZ * 3600);
        assert_eq!(crc(&reply), 0);
        assert_eq!(reply.len(), 25);
        assert_eq!(m.readings, [240., 80., 14400., 14.4, 50., 0.75, 0.]);
        assert_eq!(&reply[5..9], &[0x38, 0x80, 0, 1]);
        assert_eq!(query(&mut m, &[1, 6, 0, 1, 0, 100], HZ * 3601)[1], 6);
        query(&mut m, &[1, 4, 0, 0, 0, 10], HZ * 3602);
        assert_eq!(m.readings[6], 1.);
        query(&mut m, &[0xf8, 6, 0, 2, 0, 7], HZ * 3603);
        assert!(query(&mut m, &[1, 4, 0, 0, 0, 10], HZ * 3604).is_empty());
        assert_eq!(query(&mut m, &[7, 3, 0, 2, 0, 1], HZ * 3605)[3..5], [0, 7]);
        assert_eq!(query(&mut m, &[7, 0x42], HZ * 3606).len(), 4);
        query(&mut m, &[7, 4, 0, 0, 0, 10], HZ * 3607);
        assert_eq!(m.readings[3], 0.004);
    }
    #[test]
    fn invalid_commands_bounds_broadcast_and_bounded_pending_calibration() {
        let mut m = Pzem::new(4, 5, 10, 1, 0);
        assert!(!m.set(1, 11., 0, HZ));
        assert!(!m.set(0, f64::NAN, 0, HZ));
        assert!(!m.set(4, 1., 0, HZ));
        assert_eq!(query(&mut m, &[1, 4, 0, 9, 0, 2], HZ)[1..3], [0x84, 2]);
        assert_eq!(
            query(&mut m, &[1, 6, 0, 2, 0, 248], HZ * 2)[1..3],
            [0x86, 3]
        );
        assert!(query(&mut m, &[0, 6, 0, 2, 0, 8], HZ * 3).is_empty());
        assert_eq!(m.address, 8);
        let mut cmd = vec![0xf8, 0x41, 0x37, 0x21];
        cmd.extend(crc(&cmd).to_le_bytes());
        for b in cmd {
            m.receive(b, HZ * 4, HZ);
        }
        assert!(m.deadline() > HZ * 7);
        for _ in 0..10000 {
            m.receive(0xff, HZ * 5, HZ);
        }
        assert_eq!(m.bytes.len(), 6);
        assert!(m.command.is_empty());
    }
    #[test]
    fn readings_wait_for_query_thresholds_and_bad_crc_are_physical() {
        let mut m = Pzem::new(4, 5, 100, 1, 0);
        assert!(m.set(1, 0.019, 0, HZ));
        assert_eq!(m.generation, 0);
        assert!(m.readings[0].is_nan());
        query(&mut m, &[1, 4, 0, 0, 0, 10], HZ);
        assert_eq!(m.readings[1], 0.);
        assert_eq!(m.readings[2], 0.);
        let generation = m.generation;
        for b in [1, 4, 0, 0, 0, 10, 0, 0] {
            m.receive(b, HZ * 2, HZ);
        }
        assert_eq!(m.generation, generation);
        assert_eq!(m.deadline(), u64::MAX);
        assert_eq!(query(&mut m, &[1, 4, 0, 0, 0, 0], HZ * 3)[2], 3);
    }
}
