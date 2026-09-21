use super::*;
const EPOCH: u64 = 946684800;
const END: u64 = 4102444799;
fn bcd(v: u32) -> u8 {
    ((v / 10) * 16 + v % 10) as u8
}
fn decimal(v: u8) -> Option<u32> {
    let (a, b) = (v >> 4, v & 15);
    (a < 10 && b < 10).then_some((a * 10 + b) as u32)
}
fn days_in(year: u32, month: u32) -> u32 {
    [
        31,
        if year % 4 == 0 { 29 } else { 28 },
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
    ][month as usize - 1]
}
fn date(seconds: u64) -> [u8; 7] {
    let mut days = (seconds - EPOCH) / 86400;
    let weekday = ((seconds / 86400 + 4) % 7 + 1) as u32;
    let mut year = 2000;
    while days >= if year % 4 == 0 { 366 } else { 365 } {
        days -= if year % 4 == 0 { 366 } else { 365 };
        year += 1;
    }
    let mut month = 1;
    while days >= days_in(year, month) as u64 {
        days -= days_in(year, month) as u64;
        month += 1;
    }
    [
        bcd((seconds % 60) as u32),
        bcd((seconds / 60 % 60) as u32),
        bcd((seconds / 3600 % 24) as u32),
        weekday as u8,
        bcd(days as u32 + 1),
        bcd(month),
        bcd(year - 2000),
    ]
}
fn epoch(regs: &[u8]) -> Option<u64> {
    let sec = decimal(regs[0] & 0x7f)?;
    let min = decimal(regs[1] & 0x7f)?;
    let hour = if regs[2] & 0x40 != 0 {
        let h = decimal(regs[2] & 0x1f)?;
        if !(1..=12).contains(&h) {
            return None;
        }
        h % 12 + if regs[2] & 0x20 != 0 { 12 } else { 0 }
    } else {
        decimal(regs[2] & 0x3f)?
    };
    let day = decimal(regs[4] & 0x3f)?;
    let month = decimal(regs[5] & 0x1f)?;
    let year = 2000 + decimal(regs[6])?;
    if sec > 59
        || min > 59
        || hour > 23
        || !(1..=12).contains(&month)
        || day == 0
        || day > days_in(year, month)
        || regs[5] & 0x80 != 0
    {
        return None;
    }
    let mut days = 0;
    for y in 2000..year {
        days += if y % 4 == 0 { 366 } else { 365 };
    }
    for m in 1..month {
        days += days_in(year, m);
    }
    days += day - 1;
    Some(EPOCH + days as u64 * 86400 + hour as u64 * 3600 + min as u64 * 60 + sec as u64)
}
pub(super) struct Ds3231 {
    s: SampleState,
    epoch: u64,
    anchor: u64,
    last_second: u64,
    temp_next: u64,
    temp_repeat: u64,
    time_written: bool,
    write_time: [u8; 7],
}
impl Ds3231 {
    pub fn new(clock: Arc<AtomicU64>, hz: u32) -> Self {
        let mut s = SampleState::new(clock, hz);
        s.inputs[0] = 25.;
        s.regs[0x0e] = 0x1c;
        s.regs[0x0f] = 0x8c;
        s.regs[..7].copy_from_slice(&date(EPOCH));
        let temp_next = s.ticks(200000);
        let temp_repeat = s.hz * 64;
        Self {
            s,
            epoch: EPOCH,
            anchor: 0,
            last_second: EPOCH,
            temp_next,
            temp_repeat,
            time_written: false,
            write_time: date(EPOCH),
        }
    }
    fn current(&self) -> u64 {
        self.epoch
            .saturating_add(self.s.now.saturating_sub(self.anchor) / self.s.hz)
            .min(END)
    }
    fn update_clock(&mut self) {
        let current = self.current();
        let mode12 = self.s.regs[2] & 0x40 != 0;
        self.s.regs[..7].copy_from_slice(&date(current));
        if mode12 {
            let h = (current / 3600 % 24) as u32;
            self.s.regs[2] =
                0x40 | if h >= 12 { 0x20 } else { 0 } | bcd(if h % 12 == 0 { 12 } else { h % 12 });
        }
    }
}
impl RegisterSensor for Ds3231 {
    fn sync(&mut self) {
        self.s.time();
        let seconds = self.current();
        self.update_clock();
        let mut count = seconds.saturating_sub(self.last_second);
        if count > 0 {
            self.last_second = seconds;
            self.s.readings[14] = seconds as f64;
        }
        if self.s.now >= self.temp_next {
            let t = (self.s.inputs[0] * 4.).round().clamp(-160., 340.) as i16;
            self.s.regs[0x11] = (t >> 2) as u8;
            self.s.regs[0x12] = ((t & 3) << 6) as u8;
            self.s.readings[0] = t as f64 / 4.;
            self.s.regs[0x0e] &= !0x20;
            self.s.regs[0x0f] &= !4;
            let n = 1 + (self.s.now - self.temp_next) / self.temp_repeat;
            self.temp_next += n * self.temp_repeat;
            count += n;
        }
        if count > 0 {
            self.s.publish(count);
        }
    }
    fn registers(&self) -> [u8; 256] {
        self.s.regs
    }
    fn write(&mut self, reg: u8, value: u16) -> bool {
        self.sync();
        let v = value as u8;
        match reg {
            0..=6 => {
                if !self.time_written {
                    self.write_time.copy_from_slice(&self.s.regs[..7]);
                }
                self.write_time[reg as usize] = v;
                self.time_written = true;
            }
            7..=0x0d | 0x10 => self.s.regs[reg as usize] = v,
            0x0e => {
                self.s.regs[reg as usize] = v;
                if v & 0x20 != 0 && self.s.regs[0x0f] & 4 == 0 {
                    self.s.regs[0x0f] |= 4;
                    self.temp_next = self.s.now + self.s.ticks(200000);
                }
            }
            0x0f => {
                self.s.regs[0x0f] =
                    (self.s.regs[0x0f] & 4) | (v & 8) | (self.s.regs[0x0f] & v & 0x83)
            }
            _ => {}
        }
        true
    }
    fn stop(&mut self) {
        if self.time_written {
            if let Some(epoch) = epoch(&self.write_time) {
                self.epoch = epoch;
                self.anchor = self.s.now;
                self.last_second = epoch;
                self.s.readings[14] = f64::NAN;
                self.s.regs[..7].copy_from_slice(&self.write_time);
            }
            self.time_written = false;
        }
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        if !value.is_finite() {
            return false;
        }
        match field {
            0 if (-40.0..=85.0).contains(&value) => {
                self.sync();
                self.s.inputs[0] = value;
            }
            14 if value.fract() == 0. && (EPOCH as f64..=END as f64).contains(&value) => {
                self.sync();
                self.epoch = value as u64;
                self.anchor = self.s.now;
                self.last_second = self.epoch;
                self.s.readings[14] = f64::NAN;
                self.s.regs[0x0f] &= !0x80;
                self.update_clock();
            }
            _ => return false,
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
    #[test]
    fn bcd_clock_advances_through_leap_day_and_respects_12_hour_mode() {
        let clock = Arc::new(AtomicU64::new(0));
        let mut d = Ds3231::new(clock.clone(), 1_000_000);
        assert_eq!(d.registers()[0xf] & 0x80, 0x80);
        assert!(!d.set(14, 0.));
        assert!(d.set(14, 1704067200.));
        assert_eq!(d.registers()[0xf] & 0x80, 0);
        let leap = [0x58, 0x59, 0x71, 7, 0x28, 0x02, 0x32];
        for (i, v) in leap.into_iter().enumerate() {
            d.write(i as u8, v as u16);
        }
        d.stop();
        assert_eq!(d.registers()[2], 0x71);
        clock.store(4_000_000, Ordering::Relaxed);
        d.sync();
        assert_eq!(&d.registers()[..3], &[0x02, 0x00, 0x52]);
        assert_eq!(d.registers()[4], 0x29);
        let converted = epoch(&d.registers()[..7]).unwrap();
        assert_eq!(d.value(14), converted as f64);
        assert_eq!(epoch(&date(converted)), Some(converted));
        d.write(4, 0x30);
        d.stop();
        assert_eq!(d.value(14), converted as f64);
        d.write(0xf, 0xff);
        assert_eq!(d.registers()[0xf] & 0x80, 0);
    }
    #[test]
    fn temperature_is_latched_only_on_periodic_or_forced_conversion() {
        let clock = Arc::new(AtomicU64::new(0));
        let mut d = Ds3231::new(clock.clone(), 1_000_000);
        d.set(0, 26.25);
        clock.store(199999, Ordering::Relaxed);
        assert_eq!(d.generation(), 0);
        clock.store(200000, Ordering::Relaxed);
        assert_eq!(d.value(0), 26.25);
        assert_eq!(&d.registers()[0x11..0x13], &[26, 64]);
        d.set(0, -3.25);
        clock.store(400000, Ordering::Relaxed);
        assert_eq!(d.value(0), 26.25);
        d.write(0xe, 0x3c);
        assert_eq!(d.registers()[0xf] & 4, 4);
        clock.store(599999, Ordering::Relaxed);
        assert_eq!(d.value(0), 26.25);
        clock.store(600000, Ordering::Relaxed);
        assert_eq!(d.value(0), -3.25);
        assert_eq!(&d.registers()[0x11..0x13], &[252, 192]);
        assert_eq!(d.registers()[0xf] & 4, 0);
        assert_eq!(d.registers()[0xe] & 0x20, 0);
        clock.store(31_536_000_000_000, Ordering::Relaxed);
        assert!(d.generation() > 1);
    }
}
