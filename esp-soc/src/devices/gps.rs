//! A UART GPS transmitter. NMEA sentences enter the chip through its GPIO matrix and UART FIFO.
use std::collections::VecDeque;

#[derive(Clone, Copy)]
pub struct Fix {
    pub lat: f64,
    pub lng: f64,
    pub altitude: f64,
    pub speed: f64,
}

pub struct Gps {
    pub pin: u8,
    pub baud: u32,
    fix: Option<Fix>,
    epoch_ms: u64,
    epoch_cycles: u64,
    next_sentence: u64,
    next_byte: u64,
    bytes: VecDeque<u8>,
}
impl Gps {
    pub fn new(pin: u8, baud: u32, now: u64) -> Self {
        Self {
            pin,
            baud,
            fix: None,
            epoch_ms: 946684800000,
            epoch_cycles: now,
            next_sentence: now,
            next_byte: now,
            bytes: VecDeque::new(),
        }
    }
    pub fn set_fix(&mut self, fix: Option<Fix>, unix_ms: f64, now: u64) -> bool {
        if !unix_ms.is_finite() || !(946684800000.0..4102444800000.0).contains(&unix_ms) {
            return false;
        }
        if fix.is_some_and(|f| {
            ![f.lat, f.lng, f.altitude, f.speed]
                .iter()
                .all(|v| v.is_finite())
                || f.lat.abs() > 90.0
                || f.lng.abs() > 180.0
                || f.altitude.abs() > 100000.0
                || !(0.0..=10000.0).contains(&f.speed)
        }) {
            return false;
        }
        self.fix = fix;
        self.epoch_ms = unix_ms as u64;
        self.epoch_cycles = now;
        if fix.is_none() {
            self.bytes.clear();
            self.next_sentence = now;
            self.next_byte = now;
        }
        true
    }
    pub fn deadline(&self) -> u64 {
        if self.bytes.is_empty() {
            self.next_sentence
        } else {
            self.next_byte
        }
    }
    pub fn take_byte(&mut self, now: u64, cpu_hz: u64) -> Option<u8> {
        if self.bytes.is_empty() && now >= self.next_sentence {
            self.bytes = self
                .sentences(
                    self.epoch_ms
                        + now.saturating_sub(self.epoch_cycles).saturating_mul(1000) / cpu_hz,
                )
                .bytes()
                .collect();
            self.next_sentence = now.saturating_add(cpu_hz);
            self.next_byte = now.saturating_add((cpu_hz * 10).div_ceil(self.baud as u64));
        }
        if now < self.next_byte {
            return None;
        }
        let byte = self.bytes.pop_front()?;
        self.next_byte = self
            .next_byte
            .saturating_add((cpu_hz * 10).div_ceil(self.baud as u64));
        Some(byte)
    }
    fn sentences(&self, millis: u64) -> String {
        let seconds = millis / 1000;
        let day_secs = seconds % 86400;
        let time = format!(
            "{:02}{:02}{:02}.00",
            day_secs / 3600,
            (day_secs / 60) % 60,
            day_secs % 60
        );
        let mut days = (seconds / 86400).saturating_sub(10957);
        let mut year = 2000u64;
        let leap = |year: u64| year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
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
        let date = format!("{:02}{:02}{:02}", days + 1, month + 1, year % 100);
        let (gga, rmc) = if let Some(f) = self.fix {
            let coord = |v: f64, width: usize| {
                let minutes = (v.abs() * 60.0 * 10000.0).round() as u64;
                format!(
                    "{:0width$}{:02}.{:04}",
                    minutes / 600000,
                    (minutes / 10000) % 60,
                    minutes % 10000
                )
            };
            let lat = coord(f.lat, 2);
            let lng = coord(f.lng, 3);
            let ns = if f.lat < 0.0 { "S" } else { "N" };
            let ew = if f.lng < 0.0 { "W" } else { "E" };
            (
                format!(
                    "GPGGA,{time},{lat},{ns},{lng},{ew},1,08,1.0,{:.2},M,0.0,M,,",
                    f.altitude
                ),
                format!(
                    "GPRMC,{time},A,{lat},{ns},{lng},{ew},{:.3},0.0,{date},,,A",
                    f.speed * 1.9438444924406
                ),
            )
        } else {
            (
                format!("GPGGA,{time},,,,,0,00,99.9,,M,,M,,"),
                format!("GPRMC,{time},V,,,,,,,{date},,,N"),
            )
        };
        let sentence = |s: String| {
            let sum = s.bytes().fold(0u8, |acc, b| acc ^ b);
            format!("${s}*{sum:02X}\r\n")
        };
        sentence(gga) + &sentence(rmc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timed_bytes_checksum_coordinates_and_clear() {
        let mut gps = Gps::new(4, 9600, 0);
        assert!(gps.set_fix(
            Some(Fix {
                lat: -33.8688,
                lng: 151.2093,
                altitude: 42.5,
                speed: 10.0
            }),
            1704067200000.0,
            0
        ));
        assert_eq!(gps.take_byte(0, 160000000), None);
        assert_eq!(gps.take_byte(166666, 160000000), None);
        assert_eq!(gps.take_byte(166667, 160000000), Some(b'$'));
        let text = gps.sentences(1704067200000);
        assert!(text.contains("3352.1280,S,15112.5580,E"));
        assert!(text.contains(",19.438,0.0,010124,"));
        for line in text.lines() {
            let (body, sum) = line[1..].split_once('*').unwrap();
            assert_eq!(
                u8::from_str_radix(sum, 16).unwrap(),
                body.bytes().fold(0, |a, b| a ^ b)
            );
        }
        assert!(!gps.set_fix(
            Some(Fix {
                lat: f64::NAN,
                lng: 0.0,
                altitude: 0.0,
                speed: 0.0
            }),
            1704067200000.0,
            0
        ));
        assert!(gps.set_fix(None, 1704067200000.0, 200000));
        assert!(gps.bytes.is_empty());
        assert!(gps.sentences(1704067200000).contains("GPRMC,000000.00,V,"));
    }
    #[test]
    fn frequent_updates_do_not_truncate_a_transmitting_sentence() {
        let mut gps = Gps::new(4, 9600, 0);
        let fix = Some(Fix {
            lat: 1.0,
            lng: 2.0,
            altitude: 3.0,
            speed: 4.0,
        });
        assert!(gps.set_fix(fix, 1704067200000.0, 0));
        assert_eq!(gps.take_byte(0, 160000000), None);
        let length = gps.bytes.len();
        let deadline = gps.deadline();
        for now in 1..100 {
            assert!(gps.set_fix(fix, 1704067200000.0, now));
        }
        assert_eq!(gps.bytes.len(), length);
        assert_eq!(gps.deadline(), deadline);
        assert_eq!(gps.take_byte(deadline, 160000000), Some(b'$'));
    }
}
