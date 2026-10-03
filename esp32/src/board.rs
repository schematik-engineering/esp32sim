//! Boards wired for the classic ESP32, independent of S3 pin assignments.
pub fn make_board(name: &str) -> Option<esp_soc::Board> {
    match name { "none" | "bare" | "esp32dev" => Some(Box::new(esp_soc::NoBoard)), "esp32dev-i2c" => Some(Box::new(I2cBoard)), _ => crate::spi::make_board(name) }
}

#[cfg(test)]
mod tests {
    #[test]
    fn classic_boards_reject_s3_names() {
        for name in ["none", "bare", "esp32dev"] { assert!(super::make_board(name).is_some()); }
        for name in ["waveshare-amoled18-v2", "waveshare-lcd4b", "atech14"] { assert!(super::make_board(name).is_none()); }
    }
}

/// DevKit reference wiring: a register-addressed QMI8658 on I2C0, SDA21/SCL22.
struct I2cBoard;
impl esp_soc::BoardModel for I2cBoard {
    fn name(&self) -> &'static str { "esp32dev-i2c" }
    fn i2c_devices(&mut self) -> Vec<(u8, u8, Box<dyn esp_periph::i2c::I2cDevice>)> {
        vec![(0, 0x6b, Box::new(esp_periph::i2c::Reg8Device::new("qmi8658", &[(0, 5)])))]
    }
}
