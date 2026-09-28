#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub id: u8,
    pub pins: [u8; 4],
}
impl Config {
    pub fn valid(&self) -> bool {
        self.id < 16
            && self
                .pins
                .iter()
                .enumerate()
                .all(|(i, p)| *p < 49 && !self.pins[..i].contains(p))
    }
}
pub struct FourWireStepper {
    pub config: Config,
    phase: Option<u8>,
    displacement: i64,
    available: bool,
    lost: bool,
}
impl FourWireStepper {
    pub fn new(config: Config) -> Result<Self, String> {
        if !config.valid() {
            return Err("invalid four-winding driver identity or GPIO route".into());
        }
        Ok(Self {
            config,
            phase: None,
            displacement: 0,
            available: false,
            lost: false,
        })
    }
    pub fn drive(&mut self, enabled: u64, output: u64) {
        let mut coils = 0u8;
        let mut all_enabled = true;
        for (i, pin) in self.config.pins.iter().enumerate() {
            let mask = 1u64 << pin;
            all_enabled &= enabled & mask != 0;
            if enabled & output & mask != 0 {
                coils |= 1 << i;
            }
        }
        if coils == 0 {
            self.available = self.phase.is_some() && !self.lost;
            return;
        }
        self.available = false;
        if !all_enabled || self.lost {
            return;
        }
        let Some(phase) = [3, 6, 12, 9]
            .iter()
            .position(|p| *p == coils)
            .map(|p| p as u8)
        else {
            return;
        };
        if let Some(previous) = self.phase {
            match (phase + 4 - previous) % 4 {
                0 => {}
                1 => self.displacement = self.displacement.saturating_add(1).min(1i64 << 53),
                3 => self.displacement = self.displacement.saturating_sub(1).max(-(1i64 << 53)),
                _ => {
                    self.lost = true;
                    return;
                }
            }
        }
        self.phase = Some(phase);
        self.available = true;
    }
    pub fn position(&self) -> f64 {
        if self.available {
            self.displacement as f64
        } else {
            f64::NAN
        }
    }
}

#[cfg(test)]
mod four_wire_stepper_tests {
    use super::*;
    fn device() -> FourWireStepper {
        FourWireStepper::new(Config {
            id: 0,
            pins: [0, 1, 2, 3],
        })
        .unwrap()
    }
    fn driver_step(d: &mut FourWireStepper, output: &mut u64, phase: usize) {
        let mask = [3u64, 6, 12, 9][phase];
        for pin in [0, 2, 1, 3] {
            *output = (*output & !(1 << pin)) | (mask & (1 << pin));
            d.drive(15, *output);
        }
    }
    #[test]
    fn four_wire_stepper_validates_identity_and_explicit_distinct_windings() {
        for config in [
            Config {
                id: 16,
                pins: [0, 1, 2, 3],
            },
            Config {
                id: 0,
                pins: [0, 1, 2, 49],
            },
            Config {
                id: 0,
                pins: [0, 1, 2, 2],
            },
        ] {
            assert!(FourWireStepper::new(config).is_err());
        }
        assert!(device().position().is_nan());
    }
    #[test]
    fn four_wire_stepper_official_driver_intermediate_writes_and_relative_origin() {
        let mut d = device();
        let mut output = 0;
        for step in 1..=8 {
            driver_step(&mut d, &mut output, step % 4);
            assert_eq!(d.position(), (step - 1) as f64);
        }
        for (phase, position) in [(3, 6.), (2, 5.), (1, 4.)] {
            driver_step(&mut d, &mut output, phase);
            assert_eq!(d.position(), position);
        }
        driver_step(&mut d, &mut output, 1);
        assert_eq!(d.position(), 4.);
    }
    #[test]
    fn four_wire_stepper_held_unsupported_phase_is_unavailable_and_can_return() {
        let mut d = device();
        d.drive(15, 3);
        d.drive(15, 6);
        assert_eq!(d.position(), 1.);
        for mask in [1, 2, 4, 8, 5, 10, 7, 11, 13, 14, 15] {
            d.drive(15, mask);
            assert!(d.position().is_nan());
            d.drive(15, 6);
            assert_eq!(d.position(), 1.);
        }
        d.drive(15, 2);
        d.drive(15, 12);
        assert_eq!(d.position(), 2.);
    }
    #[test]
    fn four_wire_stepper_opposite_jump_loses_reference_until_fresh_device() {
        let mut d = device();
        d.drive(15, 3);
        d.drive(15, 12);
        assert!(d.position().is_nan());
        for mask in [9, 3, 6, 12, 0] {
            d.drive(15, mask);
            assert!(d.position().is_nan());
        }
        d.drive(0, 0);
        d.drive(15, 3);
        assert!(d.position().is_nan());
        let mut fresh = device();
        fresh.drive(15, 3);
        assert_eq!(fresh.position(), 0.);
    }
    #[test]
    fn four_wire_stepper_reset_retains_physical_displacement_not_software_origin() {
        let mut d = device();
        d.drive(15, 3);
        d.drive(15, 6);
        d.drive(15, 12);
        assert_eq!(d.position(), 2.);
        d.drive(0, 0);
        assert_eq!(d.position(), 2.);
        d.drive(15, 0);
        assert_eq!(d.position(), 2.);
        d.drive(15, 6);
        assert_eq!(d.position(), 1.);
    }
    #[test]
    fn four_wire_stepper_instances_wrong_routes_and_floating_coils() {
        let mut a = device();
        let mut b = FourWireStepper::new(Config {
            id: 1,
            pins: [4, 5, 6, 7],
        })
        .unwrap();
        for mask in [3, 6, 12] {
            a.drive(255, mask);
            b.drive(255, mask);
        }
        assert_eq!(a.position(), 2.);
        assert!(b.position().is_nan());
        for mask in [0x30, 0x90] {
            a.drive(255, mask);
            b.drive(255, mask);
        }
        assert_eq!(a.position(), 2.);
        assert_eq!(b.position(), -1.);
        a.drive(3, 3);
        assert!(a.position().is_nan());
        a.drive(15, 12);
        assert_eq!(a.position(), 2.);
    }
    #[test]
    fn four_wire_stepper_board_routes_allow_intentional_gpio_fanout() {
        use crate::board::BoardModel;
        let mut board = crate::devices::CircuitBoard::new(&[], &[]).unwrap();
        let a = Config {
            id: 0,
            pins: [0, 1, 2, 3],
        };
        let b = Config { id: 1, ..a };
        assert!(board.configure_four_wire_steppers(&[a, a]).is_err());
        board.configure_four_wire_steppers(&[a, b]).unwrap();
        for mask in [3, 6, 12] {
            board.gpio_drive(0, 15, mask);
        }
        assert_eq!(board.four_wire_stepper_position(0), 2.);
        assert_eq!(board.four_wire_stepper_position(1), 2.);
        assert!(board.four_wire_stepper_position(2).is_nan());
    }
}
