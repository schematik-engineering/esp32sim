use super::*;
use std::collections::VecDeque;

const REPORTS: [u8; 2] = [2, 5];
const GYRO_MAX: f64 = 34.90658503988659;
#[derive(Default)]
struct Feature {
    interval: u32,
    next: u64,
    sequence: u8,
    wake: bool,
}
struct Packet {
    channel: u8,
    payload: Vec<u8>,
    consumed: usize,
}
pub(super) struct Bno085 {
    s: SampleState,
    queue: VecDeque<Packet>,
    outgoing: [u8; 6],
    incoming: [Option<u8>; 6],
    command: Vec<u8>,
    reading: bool,
    cursor: usize,
    header: [u8; 4],
    boot_at: Option<u64>,
    asleep: bool,
    reset_cause: u8,
    features: [Feature; 2],
}
impl Bno085 {
    pub fn new(clock: Arc<AtomicU64>, hz: u32) -> Self {
        let mut s = SampleState::new(clock, hz);
        s.inputs[32] = 1.;
        let mut d = Self {
            s,
            queue: VecDeque::new(),
            outgoing: [0; 6],
            incoming: [None; 6],
            command: Vec::new(),
            reading: false,
            cursor: 0,
            header: [0; 4],
            boot_at: None,
            asleep: false,
            reset_cause: 1,
            features: Default::default(),
        };
        d.reset(1);
        d
    }
    fn push(&mut self, channel: u8, payload: Vec<u8>) {
        // ponytail: bounded transport queue; drop new reports when an unread host exhausts it.
        if self.queue.len() < 16 {
            self.queue.push_back(Packet {
                channel,
                payload,
                consumed: 0,
            });
        }
    }
    fn reset(&mut self, cause: u8) {
        self.reset_cause = cause;
        self.queue.clear();
        self.outgoing = [0; 6];
        self.incoming = [None; 6];
        self.features = Default::default();
        self.asleep = false;
        self.s.readings.fill(f64::NAN);
        self.boot_at = Some(self.s.now + self.s.ticks(100_000));
    }
    fn advertise(&mut self) {
        fn tag(p: &mut Vec<u8>, id: u8, value: &[u8]) {
            p.extend([id, value.len() as u8]);
            p.extend(value);
        }
        let mut p = vec![0];
        tag(&mut p, 1, &0u32.to_le_bytes());
        tag(&mut p, 8, b"shtp\0");
        for t in 2..=5 {
            tag(&mut p, t, &256u16.to_le_bytes());
        }
        tag(&mut p, 0x80, b"1.0.0\0");
        tag(&mut p, 1, &1u32.to_le_bytes());
        tag(&mut p, 8, b"executable\0");
        tag(&mut p, 6, &[1]);
        tag(&mut p, 9, b"device\0");
        tag(&mut p, 1, &2u32.to_le_bytes());
        tag(&mut p, 8, b"sensorhub\0");
        for (channel, name) in [
            (2, "control"),
            (3, "inputNormal"),
            (4, "inputWake"),
            (5, "inputGyroRv"),
        ] {
            tag(&mut p, if channel == 4 { 7 } else { 6 }, &[channel]);
            let mut name = name.as_bytes().to_vec();
            name.push(0);
            tag(&mut p, 9, &name);
        }
        tag(&mut p, 0x80, b"1.0.0\0");
        tag(&mut p, 0x81, &[0xf8, 16, 0xfc, 17, 0xfb, 5, 2, 10, 5, 14]);
        self.push(0, p);
    }
    fn feature_response(&mut self, index: usize) {
        let f = &self.features[index];
        let mut p = vec![0xfc, REPORTS[index], if f.wake { 4 } else { 0 }, 0, 0];
        p.extend(f.interval.to_le_bytes());
        p.extend([0; 8]);
        self.push(2, p);
    }
    fn execute(&mut self, packet: &[u8]) -> bool {
        let channel = packet[2] as usize;
        let data = &packet[4..];
        if channel == 1 && data == [1] {
            self.reset(2);
            return true;
        }
        if self.boot_at.is_some() || channel >= 6 {
            return false;
        }
        if self.incoming[channel] == Some(packet[3]) {
            return true;
        }
        let accepted = match (channel, data) {
            (0, [0, 0 | 1]) => {
                self.advertise();
                true
            }
            (1, [2]) => {
                self.asleep = false;
                for f in &mut self.features {
                    f.next = self.s.now + self.s.ticks(f.interval as u64);
                }
                true
            }
            (1, [3]) => {
                self.asleep = true;
                self.queue.retain(|p| p.channel < 3);
                true
            }
            (2, [0xf9, 0]) => {
                // Adafruit BNO085 Arduino guide's published serial-output profile.
                for (index, (part, major, minor, patch, build)) in [
                    (10004148u32, 3, 2, 13u16, 6u32),
                    (10003606, 1, 2, 4, 230),
                    (10003254, 4, 4, 3, 485),
                    (10003171, 4, 2, 10, 548),
                ]
                .into_iter()
                .enumerate()
                {
                    let mut p = vec![
                        0xf8,
                        if index == 0 { self.reset_cause } else { 0 },
                        major,
                        minor,
                    ];
                    p.extend(part.to_le_bytes());
                    p.extend(build.to_le_bytes());
                    p.extend(patch.to_le_bytes());
                    p.extend([0, 0]);
                    self.push(2, p);
                }
                true
            }
            (2, [0xfe, id]) => {
                if let Some(i) = REPORTS.iter().position(|r| r == id) {
                    self.feature_response(i);
                    true
                } else {
                    false
                }
            }
            (2, p) if p.len() == 17 && p[0] == 0xfd => {
                if let Some(i) = REPORTS.iter().position(|r| *r == p[1]) {
                    if p[2] & !4 != 0 || p[3..5] != [0; 2] || p[9..17] != [0; 8] {
                        return false;
                    }
                    let interval = u32::from_le_bytes(p[5..9].try_into().unwrap());
                    let interval = if interval == 0 { 0 } else { interval.max(2500) };
                    self.features[i] = Feature {
                        interval,
                        next: self.s.now + self.s.ticks(interval as u64),
                        sequence: self.features[i].sequence,
                        wake: p[2] & 4 != 0,
                    };
                    self.queue
                        .retain(|q| q.channel < 3 || q.payload.get(5) != Some(&p[1]));
                    self.feature_response(i);
                    true
                } else {
                    false
                }
            }
            _ => false,
        };
        if accepted {
            self.incoming[channel] = Some(packet[3]);
        }
        accepted
    }
    fn report(&mut self, i: usize) {
        let f = &mut self.features[i];
        let channel = if f.wake { 4 } else { 3 };
        // Nominal 100us sample-to-transport latency; SH2 timebase is relative to transport HINT.
        let mut p = vec![0xfb, 1, 0, 0, 0, REPORTS[i], f.sequence, 3, 0];
        f.sequence = f.sequence.wrapping_add(1);
        if i == 0 {
            for field in 7..10 {
                let raw = (self.s.inputs[field] * 512.).round() as i16;
                p.extend(raw.to_le_bytes());
                self.s.readings[field] = raw as f64 / 512.;
            }
        } else {
            let norm = self.s.inputs[32..36]
                .iter()
                .fold(0f64, |norm, v| norm.hypot(*v));
            for field in [33, 34, 35, 32] {
                let raw = (self.s.inputs[field] / norm * 16384.).round() as i16;
                p.extend(raw.to_le_bytes());
                self.s.readings[field] = raw as f64 / 16384.;
            }
            p.extend([0, 0]);
        }
        self.s.publish(1);
        self.push(channel, p);
    }
}
impl RegisterSensor for Bno085 {
    fn format(&self) -> WireFormat {
        WireFormat::Command
    }
    fn sync(&mut self) {
        self.s.time();
        if self.boot_at.is_some_and(|at| self.s.now >= at) {
            self.boot_at = None;
            self.advertise();
            self.push(1, vec![1]);
        }
        if self.asleep || self.boot_at.is_some() {
            return;
        }
        for i in 0..2 {
            let f = &mut self.features[i];
            if f.interval == 0 || self.s.now < f.next {
                continue;
            }
            let period = self.s.ticks(f.interval as u64);
            let count = (self.s.now - f.next) / period + 1;
            // Only the finite transport capacity can survive a host that stopped polling.
            let emitted = count.min(16);
            f.sequence = f.sequence.wrapping_add((count - emitted) as u8);
            f.next += count * period;
            for _ in 0..emitted {
                self.report(i);
            }
        }
    }
    fn registers(&self) -> [u8; 256] {
        [0; 256]
    }
    fn start(&mut self, read: bool) {
        self.stop();
        self.reading = read;
        self.cursor = 0;
        self.command.clear();
        self.header = [0; 4];
        if read {
            if let Some(p) = self.queue.front() {
                let len = (p.payload.len() - p.consumed + 4) as u16
                    | if p.consumed > 0 { 0x8000 } else { 0 };
                self.header[..2].copy_from_slice(&len.to_le_bytes());
                self.header[2] = p.channel;
                self.header[3] = self.outgoing[p.channel as usize];
            }
        }
    }
    fn read_live(&mut self, _reg: u8) -> Option<u8> {
        let value = if self.cursor < 4 {
            self.header[self.cursor]
        } else {
            self.queue
                .front()
                .and_then(|p| p.payload.get(p.consumed + self.cursor - 4))
                .copied()
                .unwrap_or(0)
        };
        self.cursor = self.cursor.saturating_add(1);
        Some(value)
    }
    fn write(&mut self, _reg: u8, value: u16) -> bool {
        if self.command.len() >= 256 {
            return false;
        }
        self.command.push(value as u8);
        if self.command.len() >= 2 {
            let length = u16::from_le_bytes([self.command[0], self.command[1]]) as usize;
            if !(5..=256).contains(&length) {
                return false;
            }
            if self.command.len() == length {
                let packet = self.command.clone();
                return self.execute(&packet);
            }
            if self.command.len() > length {
                return false;
            }
        }
        true
    }
    fn stop(&mut self) {
        if self.reading && self.cursor > 4 {
            if let Some(p) = self.queue.front_mut() {
                p.consumed = (p.consumed + self.cursor - 4).min(p.payload.len());
                self.outgoing[p.channel as usize] =
                    self.outgoing[p.channel as usize].wrapping_add(1);
                if p.consumed == p.payload.len() {
                    self.queue.pop_front();
                }
            }
        }
        self.reading = false;
        self.command.clear();
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        if !value.is_finite()
            || !match field {
                7..=9 => (-GYRO_MAX..=GYRO_MAX).contains(&value),
                32..=35 => (-1. ..=1.).contains(&value),
                _ => false,
            }
        {
            return false;
        }
        if (32..=35).contains(&field)
            && value == 0.
            && (32..36)
                .filter(|f| *f != field as usize)
                .all(|f| self.s.inputs[f] == 0.)
        {
            return false;
        }
        self.sync();
        self.s.inputs[field as usize] = value;
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
    fn device() -> (Arc<AtomicU64>, Arc<Mutex<Sensor>>, SensorI2c) {
        let clock = Arc::new(AtomicU64::new(0));
        let cfg = SensorConfig {
            id: 0,
            sda: 4,
            scl: 5,
            address: 0x4a,
            model: 53,
            shunt_milliohms: 0,
        };
        assert!(cfg.valid());
        let s = Arc::new(Mutex::new(Sensor::new(cfg, clock.clone(), 1_000_000)));
        (clock, s.clone(), SensorI2c::new(s))
    }
    fn write(d: &mut SensorI2c, ch: u8, seq: u8, data: &[u8]) -> bool {
        assert!(d.start(false));
        let len = (data.len() + 4) as u16;
        let mut p = len.to_le_bytes().to_vec();
        p.extend([ch, seq]);
        p.extend(data);
        let mut ack = true;
        for b in p {
            ack &= d.write(b);
        }
        d.stop();
        ack
    }
    fn read(d: &mut SensorI2c, n: usize) -> Vec<u8> {
        assert!(d.start(true));
        let p = (0..n).map(|_| d.read()).collect();
        d.stop();
        p
    }
    fn packet(d: &mut SensorI2c) -> Vec<u8> {
        let h = read(d, 4);
        let n = u16::from_le_bytes([h[0], h[1]]) as usize & 0x7fff;
        if n == 0 {
            h
        } else {
            read(d, n)
        }
    }
    fn boot(clock: &AtomicU64, d: &mut SensorI2c) {
        clock.store(99_999, Ordering::Relaxed);
        assert_eq!(packet(d), [0; 4]);
        clock.store(100_000, Ordering::Relaxed);
        let p = packet(d);
        assert_eq!(p[2], 0);
        assert!(p.windows(9).any(|w| w == b"sensorhub"));
        assert_eq!(packet(d), [5, 0, 1, 0, 1]);
        assert_eq!(packet(d), [0; 4]);
    }
    fn enable(d: &mut SensorI2c, id: u8, seq: u8, interval: u32) {
        let mut p = vec![0xfd, id, 0, 0, 0];
        p.extend(interval.to_le_bytes());
        p.extend([0; 8]);
        assert!(write(d, 2, seq, &p));
        let response = packet(d);
        assert_eq!(response[4], 0xfc);
        assert_eq!(response[5], id);
    }
    #[test]
    fn bno08x_boot_advertisement_partial_read_and_bad_packets() {
        let (clock, _, mut d) = device();
        clock.store(100_000, Ordering::Relaxed);
        let h = read(&mut d, 4);
        assert_eq!(h, read(&mut d, 4));
        let first = read(&mut d, 12);
        assert_eq!(&first[..4], &h);
        let h2 = read(&mut d, 4);
        assert_eq!(h2[1] & 0x80, 0x80);
        assert_eq!(h2[3], 1);
        assert_eq!(
            (u16::from_le_bytes([h2[0], h2[1]]) & 0x7fff) + 8,
            u16::from_le_bytes([h[0], h[1]])
        );
        let rest = packet(&mut d);
        assert!(rest.len() > 4);
        assert_eq!(packet(&mut d), [5, 0, 1, 0, 1]);
        assert!(!write(&mut d, 2, 0, &[0xfe, 99]));
        assert!(!write(&mut d, 5, 0, &[1]));
        assert!(d.start(false));
        assert!(d.write(0));
        assert!(!d.write(128));
        d.stop();
        assert!(d.start(false));
        assert!(d.write(6));
        assert!(d.write(0));
        assert!(d.write(2));
        d.stop();
        assert_eq!(packet(&mut d), [0; 4], "aborted write never executes");
    }
    #[test]
    fn bno08x_reports_units_readiness_sequence_disable_and_instances() {
        let (clock, state, mut d) = device();
        boot(&clock, &mut d);
        let (_, second, _) = device();
        assert!(state.lock().unwrap().set(33, 1.));
        assert!(state.lock().unwrap().set(32, 0.));
        assert!(
            !state.lock().unwrap().set(33, 0.),
            "zero norm retains prior input"
        );
        assert!(state.lock().unwrap().set(7, -1.25));
        enable(&mut d, 5, 0, 10_000);
        enable(&mut d, 2, 1, 10_000);
        clock.store(109_999, Ordering::Relaxed);
        assert_eq!(packet(&mut d), [0; 4]);
        clock.store(110_000, Ordering::Relaxed);
        let g = packet(&mut d);
        let q = packet(&mut d);
        assert_eq!(g[9], 2);
        assert_eq!(i16::from_le_bytes([g[13], g[14]]), -640);
        assert_eq!(q[9], 5);
        assert_eq!(i16::from_le_bytes([q[13], q[14]]), 16384);
        assert_eq!(&q[15..21], &[0; 6]);
        assert_eq!(state.lock().unwrap().value(33), 1.);
        assert!(second.lock().unwrap().value(33).is_nan());
        assert_eq!(packet(&mut d), [0; 4]);
        enable(&mut d, 5, 2, 0);
        enable(&mut d, 2, 3, 0);
        clock.store(1_000_000, Ordering::Relaxed);
        assert_eq!(packet(&mut d), [0; 4]);
        assert!(!state.lock().unwrap().set(7, 35.));
        assert!(!state.lock().unwrap().set(32, f64::NAN));
    }
    #[test]
    fn bno08x_sleep_reset_and_bounded_backlog() {
        let (clock, _, mut d) = device();
        boot(&clock, &mut d);
        enable(&mut d, 5, 0, 10_000);
        assert!(write(&mut d, 1, 0, &[3]));
        clock.store(200_000, Ordering::Relaxed);
        assert_eq!(packet(&mut d), [0; 4]);
        assert!(write(&mut d, 1, 1, &[2]));
        clock.store(210_000, Ordering::Relaxed);
        assert_eq!(packet(&mut d)[9], 5);
        clock.store(3_600_000_000, Ordering::Relaxed);
        for _ in 0..16 {
            assert_eq!(packet(&mut d)[9], 5);
        }
        assert_eq!(packet(&mut d), [0; 4]);
        assert!(write(&mut d, 1, 2, &[1]));
        assert_eq!(packet(&mut d), [0; 4]);
        clock.store(3_600_100_000, Ordering::Relaxed);
        assert_eq!(packet(&mut d)[2], 0);
        assert_eq!(packet(&mut d)[4], 1);
        clock.store(3_601_000_000, Ordering::Relaxed);
        assert_eq!(packet(&mut d), [0; 4], "reset disables reports");
    }
    #[test]
    fn bno08x_product_features_and_independent_routes() {
        let (clock, state, mut a) = device();
        let cfg = SensorConfig {
            id: 1,
            sda: 6,
            scl: 7,
            address: 0x4b,
            model: 53,
            shunt_milliohms: 0,
        };
        let other = Arc::new(Mutex::new(Sensor::new(cfg, clock.clone(), 1_000_000)));
        let mut b = SensorI2c::new(other.clone());
        assert_eq!(a.pins(), Some((4, 5)));
        assert_eq!(b.pins(), Some((6, 7)));
        assert!(!a.matches_address(0x4a, 0x4b, true));
        assert!(b.matches_address(0x4b, 0x4b, true));
        boot(&clock, &mut a);
        assert_eq!(packet(&mut b)[2], 0);
        assert_eq!(packet(&mut b)[4], 1);
        assert!(write(&mut a, 2, 0, &[0xf9, 0]));
        for n in 0..4 {
            let p = packet(&mut a);
            assert_eq!(p[4], 0xf8);
            assert_eq!(
                u32::from_le_bytes(p[8..12].try_into().unwrap()),
                [10004148, 10003606, 10003254, 10003171][n as usize]
            );
            assert_eq!(p[5], if n == 0 { 1 } else { 0 });
            assert_eq!(p[3], n);
        }
        enable(&mut a, 5, 1, 1);
        enable(&mut b, 5, 0, 10_000);
        assert!(write(&mut a, 2, 2, &[0xfe, 5]));
        let p = packet(&mut a);
        assert_eq!(u32::from_le_bytes(p[9..13].try_into().unwrap()), 2500);
        assert!(other.lock().unwrap().set(34, 1.));
        assert!(other.lock().unwrap().set(32, 0.));
        clock.store(110_000, Ordering::Relaxed);
        let p = packet(&mut a);
        assert_eq!(i16::from_le_bytes([p[19], p[20]]), 16384);
        let p = packet(&mut b);
        assert_eq!(i16::from_le_bytes([p[15], p[16]]), 16384);
        assert_eq!(state.lock().unwrap().value(32), 1.);
        assert_eq!(other.lock().unwrap().value(32), 0.);
        assert_eq!(state.lock().unwrap().value(34), 0.);
        assert_eq!(other.lock().unwrap().value(34), 1.);
    }
    #[test]
    fn bno08x_tiny_nonzero_quaternion_and_repeated_start() {
        let (clock, state, mut d) = device();
        boot(&clock, &mut d);
        assert!(state.lock().unwrap().set(32, 1e-300));
        enable(&mut d, 5, 0, 10_000);
        clock.store(110_000, Ordering::Relaxed);
        assert!(d.start(true));
        let first: Vec<_> = (0..9).map(|_| d.read()).collect();
        assert_eq!(first[4], 0xfb);
        assert!(d.start(true));
        let header: Vec<_> = (0..4).map(|_| d.read()).collect();
        d.stop();
        assert_eq!(header[1] & 0x80, 0x80);
        let rest = packet(&mut d);
        assert_eq!(rest[4], 5);
        assert_eq!(i16::from_le_bytes([rest[14], rest[15]]), 16384);
        assert_eq!(state.lock().unwrap().value(32), 1.);
    }
}
