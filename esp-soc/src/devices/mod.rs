//! Device models that are not tied to a chip: the things a board wires to the pins, fed with
//! what the SoC model already decoded (GPIO levels, SPI bytes, RMT bit streams). A board owns
//! them and adds what is module-specific — pin numbers, the visible window of a glass, where
//! each LED of a chain sits physically.
pub mod dcs_panel;
pub mod spi_bitbang;
pub mod ws2812;

pub use dcs_panel::DcsPanel;
pub use spi_bitbang::SpiBitBang;
pub use ws2812::Ws2812Chain;

pub mod circuit;
pub use circuit::CircuitBoard;

pub mod ssd1306;
pub use ssd1306::{OledConfig, OledController, Ssd1306, Ssd1306I2c};

pub mod gps;
pub mod camera;

pub mod sensors;
pub use sensors::{SensorConfig, Sensor, SensorI2c};

pub mod spi_display;
pub mod servo;

pub mod touch;
pub mod inputs;

pub mod resistive_touch;

pub mod pin_sensor;
pub mod rfid;

pub mod gesture;
pub mod led_display;

pub mod hx711;
pub mod max7219;

pub mod radar;

pub mod pzem;
pub mod lcd;

pub mod pca9685;

pub mod stepper;

pub mod thermocouple;

pub mod elm327;
pub mod four_wire_stepper;
