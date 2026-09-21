#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum Source {Gpio(u32),Pca9685{device:u8,channel:u8}}

/// A positional servo's calibrated response to the pulse present at its signal pin.
#[derive(Clone, Copy, Debug)]
pub struct Servo {
    pub source: Source,
    min_us: u32,
    max_us: u32,
    min_angle: i32,
    max_angle: i32,
    angle: i32,
}
impl Servo {
    pub fn new(pin: u32, min_us: u32, max_us: u32, min_angle: i32, max_angle: i32) -> Option<Self> {
        Self::from_source(Source::Gpio(pin),min_us,max_us,min_angle,max_angle)
    }
    pub fn from_source(source:Source,min_us:u32,max_us:u32,min_angle:i32,max_angle:i32)->Option<Self> {
        if !(100..=5000).contains(&min_us) || !(100..=5000).contains(&max_us) || min_us >= max_us
            || !(-360_000..=360_000).contains(&min_angle) || !(-360_000..=360_000).contains(&max_angle) || min_angle >= max_angle { return None; }
        Some(Self { source, min_us, max_us, min_angle, max_angle, angle: (min_angle + max_angle) / 2 })
    }
    pub fn configure(&mut self, mut config: Self) { config.angle = self.angle; *self = config; }
    pub fn observe(&mut self, pwm: Option<(f64,u32)>, field: u32) -> i32 {
        let pulse = pwm.filter(|(hz,duty)| (20.0..=400.0).contains(hz) && *duty > 0 && *duty < 65535)
            .map(|(hz,duty)| duty as f64 * 1_000_000.0 / 65535.0 / hz)
            .filter(|us| (100.0..=5000.0).contains(us));
        if let Some(us) = pulse {
            let fraction = ((us - self.min_us as f64) / (self.max_us - self.min_us) as f64).clamp(0.0,1.0);
            self.angle = (self.min_angle as f64 + fraction * (self.max_angle - self.min_angle) as f64).round() as i32;
        }
        match field { 0 => i32::from(pulse.is_some()), 1 => pulse.map_or(0, |us| us.round() as i32), 2 => self.angle, _ => 0 }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn pwm(us: f64) -> Option<(f64,u32)> { Some((50.0,(us/20_000.0*65535.0).round() as u32)) }
    #[test]
    fn calibration_uses_physical_pulse_and_holds_position_without_signal() {
        let mut servo=Servo::new(4,1000,2000,-90_000,90_000).unwrap();
        assert!((servo.observe(pwm(1500.0),2)).abs()<100);
        assert_eq!(servo.observe(pwm(500.0),2),-90_000);
        assert_eq!(servo.observe(pwm(2500.0),2),90_000);
        assert_eq!(servo.observe(None,0),0);
        assert_eq!(servo.observe(None,2),90_000);
        assert_eq!(servo.observe(Some((1000.0,32768)),0),0);
        assert!(Servo::new(4,2000,1000,0,180_000).is_none());
        assert!(Servo::new(4,1000,2000,180_000,0).is_none());
        assert!(Servo::new(4,99,2000,0,180_000).is_none());
    }
}
