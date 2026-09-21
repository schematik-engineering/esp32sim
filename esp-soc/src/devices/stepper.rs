#[derive(Clone, Copy, Debug)]
pub struct StepperConfig {
    pub id: u8,
    pub step: u8,
    pub dir: u8,
    pub enable: u8,
    pub enable_active_low: bool,
    pub microsteps: u16,
}
impl StepperConfig {
    pub fn valid(&self) -> bool {
        self.id < 16
            && self.step < 49
            && self.dir < 49
            && self.step != self.dir
            && (self.enable == 255
                || (self.enable < 49 && self.enable != self.step && self.enable != self.dir))
            && self.microsteps.is_power_of_two()
            && self.microsteps <= 256
    }
    pub fn pins(&self) -> Vec<u8> {
        [self.step, self.dir, self.enable]
            .into_iter()
            .filter(|p| *p != 255)
            .collect()
    }
}
pub struct Stepper {
    pub config: StepperConfig,
    high: bool,
    pulses: i64,
}
impl Stepper {
    pub fn new(config: StepperConfig) -> Result<Self, String> {
        if !config.valid() {
            return Err("invalid step/direction driver wiring or microstep ratio".into());
        }
        Ok(Self {
            config,
            high: false,
            pulses: 0,
        })
    }
    pub fn drive(&mut self, enabled: u64, output: u64) {
        let c = self.config;
        let high = enabled & (1 << c.step) != 0 && output & (1 << c.step) != 0;
        let active = c.enable == 255
            || (enabled & (1 << c.enable) != 0
                && ((output & (1 << c.enable) != 0) != c.enable_active_low));
        if high && !self.high && active && enabled & (1 << c.dir) != 0 {
            let direction = if output & (1 << c.dir) != 0 { 1 } else { -1 };
            self.pulses = (self.pulses + direction).clamp(-(1i64 << 53), 1i64 << 53);
        }
        self.high = high;
    }
    pub fn position(&self) -> f64 {
        self.pulses as f64 / self.config.microsteps as f64
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_edges_direction_and_enable_determine_position() {
        let mut d = Stepper::new(StepperConfig {
            id: 0,
            step: 4,
            dir: 5,
            enable: 6,
            enable_active_low: true,
            microsteps: 4,
        })
        .unwrap();
        let enabled = (1 << 4) | (1 << 5) | (1 << 6);
        d.drive(enabled, 1 << 5);
        d.drive(enabled, (1 << 5) | (1 << 4));
        d.drive(enabled, (1 << 5) | (1 << 4));
        assert_eq!(d.position(), 0.25);
        d.drive(enabled, 0);
        d.drive(enabled, 1 << 4);
        assert_eq!(d.position(), 0.0);
        d.drive(enabled, 1 << 6);
        d.drive(enabled, (1 << 6) | (1 << 4));
        assert_eq!(d.position(), 0.0);
        d.drive(enabled, 1 << 4);
        assert_eq!(
            d.position(),
            0.0,
            "enabling with STEP already high must not invent an edge"
        );
        d.drive(1 << 4, 0);
        d.drive(1 << 4, 1 << 4);
        assert_eq!(
            d.position(),
            0.0,
            "floating DIR cannot determine displacement"
        );
    }
    #[test]
    fn physical_position_survives_mcu_pin_reset() {
        let mut d = Stepper::new(StepperConfig {
            id: 0,
            step: 4,
            dir: 5,
            enable: 255,
            enable_active_low: false,
            microsteps: 1,
        })
        .unwrap();
        d.drive((1 << 4) | (1 << 5), 1 << 5);
        d.drive((1 << 4) | (1 << 5), (1 << 4) | (1 << 5));
        d.drive(0, 0);
        assert_eq!(d.position(), 1.0);
        assert!(Stepper::new(StepperConfig {
            step: 5,
            ..d.config
        })
        .is_err());
    }
}
