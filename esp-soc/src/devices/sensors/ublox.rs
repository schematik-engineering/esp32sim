use super::*;
use std::collections::VecDeque;

const MAX_STREAM: usize = 2048;
pub(super) struct SamM8q {
    s: SampleState,
    pointer: u8,
    transaction: Vec<u8>,
    incoming: Vec<u8>,
    outgoing: VecDeque<u8>,
    available: u16,
    epoch_at: u64,
    next: u64,
    period_ms: u16,
    nav_rate: u16,
    time_ref: u16,
    auto_rate: u8,
    nav_count: u64,
    stopped: bool,
    solution: Option<[u8; 92]>,
}
fn checksum(bytes: &[u8]) -> [u8; 2] {
    let (mut a, mut b) = (0u8, 0u8);
    for v in bytes {
        a = a.wrapping_add(*v);
        b = b.wrapping_add(a);
    }
    [a, b]
}
fn frame(class: u8, id: u8, payload: &[u8]) -> Vec<u8> {
    let mut p = vec![0xb5, 0x62, class, id];
    p.extend((payload.len() as u16).to_le_bytes());
    p.extend(payload);
    p.extend(checksum(&p[2..]));
    p
}
impl SamM8q {
    pub fn new(clock: Arc<AtomicU64>, hz: u32) -> Self {
        let mut s = SampleState::new(clock, hz);
        s.time();
        s.inputs[14] = 1704110400.;
        s.inputs[100] = 9.;
        s.inputs[101] = 3.;
        s.inputs[102] = 1.;
        let now = s.now;
        let next = now + s.ticks(100_000);
        Self {
            s,
            pointer: 255,
            transaction: Vec::new(),
            incoming: Vec::new(),
            outgoing: VecDeque::new(),
            available: 0,
            epoch_at: now,
            next,
            period_ms: 1000,
            nav_rate: 1,
            time_ref: 0,
            auto_rate: 0,
            nav_count: 0,
            stopped: false,
            solution: None,
        }
    }
    fn queue(&mut self, class: u8, id: u8, payload: &[u8]) {
        let p = frame(class, id, payload);
        if self.outgoing.len() + p.len() <= MAX_STREAM {
            self.outgoing.extend(p);
        }
    }
    fn ack(&mut self, class: u8, id: u8, ok: bool) {
        self.queue(5, u8::from(ok), &[class, id]);
    }
    fn period(&self) -> u64 {
        self.s
            .ticks(self.period_ms as u64 * self.nav_rate as u64 * 1000)
    }
    fn port(&self) -> [u8; 20] {
        let mut p = [0; 20];
        p[4] = 0x84;
        p[12] = 1;
        p[14] = 1;
        p
    }
    fn execute(&mut self, class: u8, id: u8, p: &[u8]) {
        match (class, id, p) {
            (6, 0, []) | (6, 0, [0]) => self.queue(6, 0, &self.port()),
            (6, 0, p) if p.len() == 20 => self.ack(6, 0, p == self.port()),
            (1, 7, []) => {
                if !self.stopped {
                    if let Some(p) = self.solution {
                        self.queue(1, 7, &p);
                    }
                }
            }
            (6, 8, []) => {
                let mut p = self.period_ms.to_le_bytes().to_vec();
                p.extend(self.nav_rate.to_le_bytes());
                p.extend(self.time_ref.to_le_bytes());
                self.queue(6, 8, &p);
            }
            (6, 8, p) if p.len() == 6 => {
                let meas = u16::from_le_bytes([p[0], p[1]]);
                let nav = u16::from_le_bytes([p[2], p[3]]);
                let reference = u16::from_le_bytes([p[4], p[5]]);
                let ok = meas >= 100 && (1..=127).contains(&nav) && reference <= 1;
                if ok {
                    self.period_ms = meas;
                    self.nav_rate = nav;
                    self.time_ref = reference;
                    self.next = self.s.now + self.period();
                }
                self.ack(6, 8, ok);
            }
            (6, 1, [1, 7]) => self.queue(6, 1, &[1, 7, self.auto_rate, 0, 0, 0, 0, 0]),
            (6, 1, [1, 7, rate]) => {
                self.auto_rate = *rate;
                self.ack(6, 1, true);
            }
            (6, 1, p) if p.len() == 8 && p[..2] == [1, 7] && p[3..].iter().all(|v| *v == 0) => {
                self.auto_rate = p[2];
                self.ack(6, 1, true);
            }
            (6, 4, [0, 0, mode, 0]) if matches!(mode, 2 | 8 | 9) => {
                self.stopped = *mode == 8;
                self.solution = None;
                self.outgoing.clear();
                self.s.readings.fill(f64::NAN);
                self.s.generation = 0;
                self.next = self.s.now + self.s.ticks(100_000);
            }
            (6, _, _) => self.ack(class, id, false),
            _ => {}
        }
    }
    fn ingest(&mut self, byte: u8) {
        if self.incoming.is_empty() {
            if byte == 0xb5 {
                self.incoming.push(byte);
            }
            return;
        }
        if self.incoming.len() == 1 && byte != 0x62 {
            self.incoming.clear();
            if byte == 0xb5 {
                self.incoming.push(byte);
            }
            return;
        }
        self.incoming.push(byte);
        if self.incoming.len() < 6 {
            return;
        }
        let len = u16::from_le_bytes([self.incoming[4], self.incoming[5]]) as usize;
        if len > 512 {
            self.incoming.clear();
            return;
        }
        if self.incoming.len() == len + 8 {
            let p = std::mem::take(&mut self.incoming);
            if checksum(&p[2..len + 6]) == p[len + 6..] {
                self.execute(p[2], p[3], &p[6..len + 6]);
            }
        }
    }
    fn sample(&mut self, at: u64) {
        let epoch = self.s.inputs[14] + at.saturating_sub(self.epoch_at) as f64 / self.s.hz as f64;
        let seconds = epoch.floor() as u64;
        let mut days = (seconds / 86400).saturating_sub(10957);
        let mut year = 2000u16;
        let leap = |y: u16| y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
        while days >= if leap(year) { 366 } else { 365 } {
            days -= if leap(year) { 366 } else { 365 };
            year += 1;
        }
        let months = [
            31,
            if leap(year) { 29 } else { 28 },
            31,
            30,
            31,
            30,
            31,
            31,
            30,
            31,
            30,
            31,
        ];
        let mut month = 0;
        while days >= months[month] {
            days -= months[month];
            month += 1;
        }
        let mut p = [0u8; 92];
        let leap_seconds = 13
            + [
                1136073600u64,
                1230768000,
                1341100800,
                1435708800,
                1483228800,
            ]
            .iter()
            .filter(|t| seconds >= **t)
            .count() as u64;
        let tow = ((seconds - 315964800 + leap_seconds) % 604800) * 1000
            + ((epoch - epoch.floor()) * 1000.) as u64;
        p[..4].copy_from_slice(&(tow as u32).to_le_bytes());
        p[4..6].copy_from_slice(&year.to_le_bytes());
        p[6] = month as u8 + 1;
        p[7] = days as u8 + 1;
        p[8] = (seconds % 86400 / 3600) as u8;
        p[9] = (seconds % 3600 / 60) as u8;
        p[10] = (seconds % 60) as u8;
        p[11] = 7;
        p[16..20].copy_from_slice(&(((epoch - epoch.floor()) * 1e9).round() as i32).to_le_bytes());
        let valid = self.s.inputs[102] == 1. && matches!(self.s.inputs[101] as u8, 2 | 3);
        p[20] = self.s.inputs[101] as u8;
        p[21] = u8::from(valid);
        p[23] = self.s.inputs[100] as u8;
        p[78] = u8::from(!valid);
        for (field, offset, scale) in [
            (97, 24, 1e7),
            (96, 28, 1e7),
            (98, 32, 1000.),
            (99, 60, 1000.),
        ] {
            let raw = if valid {
                (self.s.inputs[field] * scale).round() as i32
            } else {
                0
            };
            p[offset..offset + 4].copy_from_slice(&raw.to_le_bytes());
            self.s.readings[field] = if valid { raw as f64 / scale } else { f64::NAN };
        }
        p.copy_within(32..36, 36);
        p.copy_within(60..64, 48);
        self.s.readings[14] = epoch;
        self.s.readings[100] = self.s.inputs[100];
        self.s.readings[101] = self.s.inputs[101];
        self.s.readings[102] = f64::from(valid);
        self.solution = Some(p);
    }
}
impl RegisterSensor for SamM8q {
    fn format(&self) -> WireFormat {
        WireFormat::Command
    }
    fn sync(&mut self) {
        self.s.time();
        if self.stopped || self.s.now < self.next {
            return;
        }
        let count = (self.s.now - self.next) / self.period() + 1;
        let at = self.next + (count - 1) * self.period();
        self.sample(at);
        self.next += count * self.period();
        self.s.publish(count);
        let before = self.nav_count;
        self.nav_count = self.nav_count.wrapping_add(count);
        if self.auto_rate > 0
            && (before / self.auto_rate as u64 != self.nav_count / self.auto_rate as u64)
        {
            if let Some(p) = self.solution {
                self.queue(1, 7, &p);
            }
        }
    }
    fn registers(&self) -> [u8; 256] {
        [0; 256]
    }
    fn start(&mut self, read: bool) {
        self.stop();
        if read {
            self.available = self.outgoing.len() as u16;
        }
    }
    fn stop(&mut self) {
        let data = std::mem::take(&mut self.transaction);
        if data.len() == 1 {
            self.pointer = data[0];
        } else {
            for b in data {
                self.ingest(b);
            }
        }
    }
    fn write(&mut self, _reg: u8, value: u16) -> bool {
        if self.transaction.len() >= 520 {
            return false;
        }
        self.transaction.push(value as u8);
        true
    }
    fn read_live(&mut self, _reg: u8) -> Option<u8> {
        let v = match self.pointer {
            253 => (self.available >> 8) as u8,
            254 => self.available as u8,
            255 => self.outgoing.pop_front().unwrap_or(255),
            _ => 255,
        };
        self.pointer = self.pointer.saturating_add(1);
        Some(v)
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        if !value.is_finite()
            || !match field {
                14 => (946684800. ..4102444800.).contains(&value),
                96 => (-90. ..=90.).contains(&value),
                97 => (-180. ..=180.).contains(&value),
                98 => (-1000. ..=50000.).contains(&value),
                99 => (0. ..=500.).contains(&value),
                100 => value.fract() == 0. && (0. ..=64.).contains(&value),
                101 => matches!(value, 0. | 2. | 3.),
                102 => matches!(value, 0. | 1.),
                _ => false,
            }
        {
            return false;
        }
        self.sync();
        self.s.inputs[field as usize] = value;
        if field == 14 {
            self.epoch_at = self.s.now;
        }
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
    fn device(id: u8, pins: (u8, u8), clock: Arc<AtomicU64>) -> (Arc<Mutex<Sensor>>, SensorI2c) {
        let cfg = SensorConfig {
            id,
            sda: pins.0,
            scl: pins.1,
            address: 0x42,
            model: 57,
            shunt_milliohms: 0,
        };
        assert!(cfg.valid());
        let s = Arc::new(Mutex::new(Sensor::new(cfg, clock, 1_000_000)));
        (s.clone(), SensorI2c::new(s))
    }
    fn write(d: &mut SensorI2c, p: &[u8]) {
        assert!(d.start(false));
        for b in p {
            assert!(d.write(*b));
        }
        d.stop();
    }
    fn read(d: &mut SensorI2c, n: usize) -> Vec<u8> {
        assert!(d.start(true));
        let p = (0..n).map(|_| d.read()).collect();
        d.stop();
        p
    }
    fn drain(d: &mut SensorI2c) -> Vec<u8> {
        write(d, &[253]);
        let n = read(d, 2);
        read(d, u16::from_be_bytes([n[0], n[1]]) as usize)
    }
    fn send(d: &mut SensorI2c, c: u8, id: u8, p: &[u8]) -> Vec<u8> {
        write(d, &frame(c, id, p));
        drain(d)
    }
    fn payload(p: &[u8]) -> &[u8] {
        assert_eq!(&p[..2], &[0xb5, 0x62]);
        assert_eq!(checksum(&p[2..p.len() - 2]), p[p.len() - 2..]);
        &p[6..p.len() - 2]
    }
    #[test]
    fn ublox_ddc_pointer_partial_read_and_fragmented_stream() {
        let clock = Arc::new(AtomicU64::new(100_000));
        let (_, mut d) = device(0, (4, 5), clock);
        let p = frame(6, 0, &[0]);
        write(&mut d, &p[..4]);
        assert!(drain(&mut d).is_empty());
        write(&mut d, &p[4..]);
        write(&mut d, &[253]);
        assert!(d.start(true));
        assert_eq!(d.read(), 0);
        assert_eq!(d.read(), 28);
        d.stop();
        assert_eq!(read(&mut d, 3), [0xb5, 0x62, 6]);
        let rest = read(&mut d, 25);
        assert_eq!(rest[0], 0);
        assert_eq!(read(&mut d, 4), [255; 4]);
        write(&mut d, &[252]);
        assert_eq!(read(&mut d, 4), [255, 0, 0, 255]);
    }
    #[test]
    fn ublox_checksum_unknown_messages_and_bounds() {
        let clock = Arc::new(AtomicU64::new(0));
        let (_, mut d) = device(0, (4, 5), clock);
        let mut p = frame(6, 0, &[0]);
        p[8] ^= 1;
        write(&mut d, &p);
        assert!(drain(&mut d).is_empty());
        assert_eq!(payload(&send(&mut d, 6, 127, &[0])), [6, 127]);
        assert!(send(&mut d, 127, 1, &[]).is_empty());
        write(&mut d, &[0xb5, 0x62, 6, 0, 255, 255]);
        assert!(drain(&mut d).is_empty());
        assert_eq!(payload(&send(&mut d, 6, 0, &[0])).len(), 20);
        for _ in 0..200 {
            write(&mut d, &frame(6, 0, &[0]));
        }
        let bytes = drain(&mut d);
        assert!(bytes.len() <= MAX_STREAM);
        assert_eq!(bytes.len() % 28, 0);
    }
    #[test]
    fn ublox_pvt_units_calendar_readiness_and_independent_instances() {
        let clock = Arc::new(AtomicU64::new(0));
        let (s, mut a) = device(0, (4, 5), clock.clone());
        let (other, mut b) = device(1, (6, 7), clock.clone());
        assert_eq!(a.pins(), Some((4, 5)));
        assert_eq!(b.pins(), Some((6, 7)));
        assert!(!a.matches_address(0x42, 0x43, true));
        for (f, v) in [
            (96, 12.3456789),
            (97, -45.6789123),
            (98, 123.4),
            (99, 2.5),
            (14, 1709251199.),
        ] {
            assert!(s.lock().unwrap().set(f, v));
        }
        assert!(send(&mut a, 1, 7, &[]).is_empty());
        clock.store(100_000, Ordering::Relaxed);
        let p = send(&mut a, 1, 7, &[]);
        let p = payload(&p);
        assert_eq!(p.len(), 92);
        assert_eq!(i32::from_le_bytes(p[28..32].try_into().unwrap()), 123456789);
        assert_eq!(
            i32::from_le_bytes(p[24..28].try_into().unwrap()),
            -456789123
        );
        assert_eq!(i32::from_le_bytes(p[32..36].try_into().unwrap()), 123400);
        assert_eq!(i32::from_le_bytes(p[60..64].try_into().unwrap()), 2500);
        assert_eq!(&p[4..12], &[232, 7, 2, 29, 23, 59, 59, 7]);
        assert_eq!(p[21] & 1, 1);
        let p = send(&mut b, 1, 7, &[]);
        assert_eq!(payload(&p)[28..32], [0; 4]);
        assert_eq!(other.lock().unwrap().value(96), 0.);
        clock.store(1_100_000, Ordering::Relaxed);
        let p = send(&mut a, 1, 7, &[]);
        assert_eq!(&payload(&p)[6..11], &[3, 1, 0, 0, 0]);
    }
    #[test]
    fn ublox_fix_validity_and_input_rejection() {
        let clock = Arc::new(AtomicU64::new(0));
        let (s, mut d) = device(0, (4, 5), clock.clone());
        for (f, v) in [
            (96, 91.),
            (97, -181.),
            (99, -1.),
            (100, 1.5),
            (101, 1.),
            (102, 2.),
            (14, 4102444800.),
            (96, f64::NAN),
        ] {
            assert!(!s.lock().unwrap().set(f, v));
        }
        for (i, (kind, ok)) in [(2, 1), (3, 0), (0, 1)].into_iter().enumerate() {
            assert!(s.lock().unwrap().set(101, kind as f64));
            assert!(s.lock().unwrap().set(102, ok as f64));
            clock.store(100_000 + i as u64 * 1_000_000, Ordering::Relaxed);
            let p = send(&mut d, 1, 7, &[]);
            let p = payload(&p);
            assert_eq!(p[20], kind);
            assert_eq!(p[21] & 1, u8::from(i == 0));
            assert_eq!(p[78] & 1, u8::from(i != 0));
            assert_eq!(p[11], 7);
            assert_eq!(s.lock().unwrap().value(96).is_nan(), i != 0);
            assert_eq!(s.lock().unwrap().value(102), f64::from(i == 0));
        }
    }
    #[test]
    fn ublox_rate_auto_reports_and_large_clock_jump() {
        let clock = Arc::new(AtomicU64::new(0));
        let (_, mut d) = device(0, (4, 5), clock.clone());
        assert_eq!(payload(&send(&mut d, 6, 8, &[])), [232, 3, 1, 0, 0, 0]);
        let p = send(&mut d, 6, 8, &[200, 0, 1, 0, 0, 0]);
        assert_eq!(p[3], 1);
        let p = send(&mut d, 6, 8, &[1, 0, 1, 0, 0, 0]);
        assert_eq!(p[3], 0);
        assert_eq!(send(&mut d, 6, 1, &[1, 7, 1])[3], 1);
        clock.store(199_999, Ordering::Relaxed);
        assert!(drain(&mut d).is_empty());
        clock.store(200_000, Ordering::Relaxed);
        assert_eq!(drain(&mut d).len(), 100);
        clock.store(3_600_000_000, Ordering::Relaxed);
        assert_eq!(
            drain(&mut d).len(),
            100,
            "one latest automatic packet for bounded catch-up"
        );
        assert_eq!(send(&mut d, 6, 1, &[1, 7, 0])[3], 1);
        clock.store(3_601_000_000, Ordering::Relaxed);
        assert!(drain(&mut d).is_empty());
    }
    #[test]
    fn ublox_gnss_stop_start_and_reset_keep_configuration() {
        let clock = Arc::new(AtomicU64::new(0));
        let (s, mut d) = device(0, (4, 5), clock.clone());
        clock.store(100_000, Ordering::Relaxed);
        assert_eq!(send(&mut d, 1, 7, &[]).len(), 100);
        assert!(send(&mut d, 6, 4, &[0, 0, 8, 0]).is_empty());
        clock.store(2_000_000, Ordering::Relaxed);
        assert!(send(&mut d, 1, 7, &[]).is_empty());
        assert_eq!(s.lock().unwrap().generation(), 0);
        assert!(send(&mut d, 6, 4, &[0, 0, 9, 0]).is_empty());
        clock.store(2_099_999, Ordering::Relaxed);
        assert!(send(&mut d, 1, 7, &[]).is_empty());
        clock.store(2_100_000, Ordering::Relaxed);
        assert_eq!(send(&mut d, 1, 7, &[]).len(), 100);
        send(&mut d, 6, 8, &[200, 0, 1, 0, 0, 0]);
        assert!(send(&mut d, 6, 4, &[0, 0, 2, 0]).is_empty());
        assert_eq!(payload(&send(&mut d, 6, 8, &[])), [200, 0, 1, 0, 0, 0]);
    }
}
