#[derive(Clone, Copy, Debug)]
pub struct LoadCellConfig {
    pub id: u8,
    pub dout: u8,
    pub sck: u8,
    pub rate: u8,
    pub capacity: f64,
    pub sensitivity: f64,
    pub offset: i32,
}
impl LoadCellConfig {
    pub fn calibration_valid(capacity: f64, sensitivity: f64, offset: i32) -> bool {
        capacity.is_finite()
            && capacity > 0.
            && capacity <= 1e9
            && sensitivity.is_finite()
            && sensitivity > 0.
            && sensitivity <= 20.
            && (-8_388_608..=8_388_607).contains(&offset)
    }
    pub fn valid(&self) -> bool {
        self.dout < 49
            && self.sck < 49
            && self.dout != self.sck
            && [10, 80].contains(&self.rate)
            && Self::calibration_valid(self.capacity, self.sensitivity, self.offset)
    }
}

pub struct LoadCell {
    pub config: LoadCellConfig,
    hz: u64,
    weight: f64,
    clock: bool,
    high_at: u64,
    sleeping: bool,
    ready_at: Option<u64>,
    data: Option<i32>,
    pulses: u8,
    selection: u8,
    next_selection: u8,
    output: bool,
}
impl LoadCell {
    pub fn new(config: LoadCellConfig, hz: u64, now: u64) -> Self {
        Self {
            config,
            hz,
            weight: 0.,
            clock: false,
            high_at: 0,
            sleeping: false,
            ready_at: Some(now + hz * 4 / config.rate as u64),
            data: None,
            pulses: 0,
            selection: 25,
            next_selection: 25,
            output: true,
        }
    }
    fn period(&self) -> u64 {
        self.hz / self.config.rate as u64
    }
    fn raw(&self) -> i32 {
        let gain = match self.selection {
            25 => 128.,
            27 => 64.,
            _ => 0.,
        };
        let code = self.weight / self.config.capacity * self.config.sensitivity / 1000.
            * 2.
            * gain
            * 8_388_608.
            + f64::from(self.config.offset);
        code.round().clamp(-8_388_608., 8_388_607.) as i32
    }
    pub fn weight(&mut self, value: f64) -> bool {
        if !value.is_finite() || value.abs() > 1e9 {
            return false;
        }
        self.weight = value;
        true
    }
    pub fn calibrate(&mut self, capacity: f64, sensitivity: f64, offset: i32) -> bool {
        if !LoadCellConfig::calibration_valid(capacity, sensitivity, offset) {
            return false;
        }
        self.config.capacity = capacity;
        self.config.sensitivity = sensitivity;
        self.config.offset = offset;
        true
    }
    pub fn level(&self) -> (u8, bool) {
        (self.config.dout, self.output)
    }
    pub fn next_deadline(&self) -> Option<u64> {
        self.ready_at
            .into_iter()
            .chain(
                (self.clock && !self.sleeping)
                    .then_some(self.high_at + self.hz * 60 / 1_000_000 + 1),
            )
            .min()
    }
    pub fn advance(&mut self, now: u64) {
        if self.clock
            && !self.sleeping
            && now.saturating_sub(self.high_at) > self.hz * 60 / 1_000_000
        {
            self.sleeping = true;
            self.ready_at = None;
            self.data = None;
            self.output = true;
            self.pulses = 0;
        }
        if !self.sleeping && self.ready_at.is_some_and(|at| at <= now) {
            self.selection = self.next_selection;
            self.data = Some(self.raw());
            self.ready_at = None;
            self.pulses = 0;
            self.output = false;
        }
    }
    pub fn gpio_drive(&mut self, now: u64, enabled: u64, output: u64) {
        self.advance(now);
        let high = enabled & output & (1u64 << self.config.sck) != 0;
        if high == self.clock {
            return;
        }
        self.clock = high;
        if high {
            self.high_at = now;
            if self.sleeping {
                return;
            }
            if let Some(data) = self.data {
                if self.pulses < 24 {
                    self.output = ((data as u32) >> (23 - self.pulses)) & 1 != 0;
                    self.pulses += 1;
                } else if self.pulses < 27 {
                    self.pulses += 1;
                    self.next_selection = self.pulses;
                    self.output = true;
                }
            }
        } else if self.sleeping {
            self.sleeping = false;
            self.selection = 25;
            self.next_selection = 25;
            self.pulses = 0;
            self.ready_at = Some(now + self.period() * 4);
        } else if self.pulses >= 25 {
            let settling = if self.selection != self.next_selection {
                4
            } else {
                1
            };
            self.ready_at = Some(now + self.period() * settling);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sensor() -> LoadCell {
        LoadCell::new(
            LoadCellConfig {
                id: 0,
                dout: 4,
                sck: 5,
                rate: 10,
                capacity: 5000.,
                sensitivity: 2.,
                offset: 123,
            },
            1_000_000,
            0,
        )
    }
    fn read(s: &mut LoadCell, at: u64, clocks: u8) -> i32 {
        s.advance(at);
        assert!(!s.level().1);
        let mut bits = 0u32;
        for i in 0..clocks {
            s.gpio_drive(at + u64::from(i) * 4, 1 << 5, 1 << 5);
            if i < 24 {
                bits = bits << 1 | u32::from(s.level().1);
            }
            s.gpio_drive(at + u64::from(i) * 4 + 2, 1 << 5, 0);
        }
        ((bits << 8) as i32) >> 8
    }
    #[test]
    fn physical_bridge_voltage_is_latched_and_shifted_msb_first_with_real_gain() {
        let mut s = sensor();
        assert!(s.weight(1000.));
        s.advance(399999);
        assert!(s.level().1);
        assert_eq!(read(&mut s, 400000, 25), 859116);
        assert!(s.level().1);
        s.advance(500095);
        assert!(s.level().1);
        assert_eq!(read(&mut s, 500100, 27), 859116);
        s.advance(899999);
        assert!(s.level().1);
        assert_eq!(read(&mut s, 900210, 27), 429620);
        assert!(s.weight(-1000.));
        assert_eq!(read(&mut s, 1000320, 26), -429374);
        assert_eq!(read(&mut s, 1400430, 25), 123);
    }
    #[test]
    fn power_down_requires_long_clock_and_wake_resets_gain_and_conversion() {
        let mut s = sensor();
        s.advance(400000);
        s.gpio_drive(400001, 1 << 5, 1 << 5);
        s.advance(400061);
        assert!(!s.sleeping);
        s.advance(400062);
        assert!(s.sleeping);
        assert!(s.level().1);
        s.gpio_drive(500000, 1 << 5, 0);
        assert!(!s.sleeping);
        s.advance(899999);
        assert!(s.level().1);
        s.advance(900000);
        assert!(!s.level().1);
        assert_eq!(s.selection, 25);
    }
    #[test]
    fn calibration_and_saturation_are_independent_of_driver_scale_or_offset() {
        let mut s = sensor();
        assert!(!s.calibrate(0., 2., 0));
        assert!(!s.weight(f64::NAN));
        assert!(s.calibrate(5000., 2., -100));
        assert!(s.weight(1e9));
        assert_eq!(read(&mut s, 400000, 25), 8_388_607);
        assert!(s.weight(-1e9));
        assert_eq!(read(&mut s, 500100, 25), -8_388_608);
        s.config.rate = 80;
        s.gpio_drive(500300, 1 << 5, 1 << 5);
        s.advance(500361);
        s.gpio_drive(500400, 1 << 5, 0);
        assert_eq!(s.next_deadline(), Some(550400));
    }
}
