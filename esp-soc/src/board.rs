//! Boards around the SoC. The SoC model emits generic events (GPIO edges, RMT symbol streams,
//! SPI bytes, LCD frames, camera requests); a `BoardModel` interprets them as the devices wired to
//! the pins and offers what the UI and the scripts need back.
use esp_periph::i2c::I2cDevice;

pub type VirtualCycle = u64;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SpiPins {
    pub sclk: u64,
    pub mosi: u64,
    pub cs: u64,
    pub miso: Option<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BoardEdge {
    pub cycle: VirtualCycle,
    pub pin: u8,
    pub level: bool,
}

/// What a board does with the SoC's pin-level activity.
pub trait BoardModel {
    fn configure_steppers(&mut self,_configs:&[crate::devices::stepper::StepperConfig])->Result<(),String>{Err("board cannot attach step/direction drivers".into())}
    fn stepper_position(&self,_id:u8)->f64{f64::NAN}
    fn configure_pwm_expanders(&mut self,_configs:&[crate::devices::pca9685::Config],_hz:u64)->Result<(),String>{Err("board cannot attach PWM expanders".into())}
    fn pwm_expander_clock(&mut self,_id:u8,_hz:u32)->bool{false}
    fn pwm_expander_output(&self,_id:u8,_channel:u8)->Option<(f64,u32)>{None}
    fn has_pwm_expander(&self,_id:u8)->bool{false}
    fn configure_load_cells(&mut self,_configs:&[crate::devices::hx711::LoadCellConfig],_hz:u64)->Result<(),String>{Err("board does not support load cells".into())}
    fn load_cell_weight(&mut self,_id:u8,_value:f64)->bool{false}
    fn load_cell_calibrate(&mut self,_id:u8,_capacity:f64,_sensitivity:f64,_offset:i32)->bool{false}
    fn configure_gestures(&mut self,_configs:&[crate::devices::gesture::GestureConfig],_hz:u64)->Result<(),String>{Err("board does not support gesture sensors".into())}
    fn gesture(&mut self,_id:u8,_direction:u8)->bool{false}
    fn proximity(&mut self,_id:u8,_value:f64)->bool{false}
    fn proximity_reading(&self,_id:u8)->u32{u32::MAX}
    fn configure_lcds(&mut self,_configs:&[crate::devices::lcd::LcdConfig],_hz:u64)->Result<(),String>{Err("board cannot attach character LCDs".into())}
    fn configure_led_displays(&mut self,_configs:&[crate::devices::led_display::LedDisplayConfig],_hz:u64)->Result<(),String>{Err("board cannot attach LED displays".into())}

    fn configure_rfid(&mut self,_configs:&[crate::devices::rfid::RfidConfig],_hz:u64)->Result<(),String>{Err("board does not support RFID readers".into())}
    fn rfid_card(&mut self,_id:u8,_uid:&[u8])->bool{false}
    fn configure_inputs(&mut self, _configs:&[crate::devices::inputs::InputConfig], _hz:u64)->Result<(),String> {Err("board does not support project inputs".into())}
    fn distance_mm(&mut self, _id:u8, _value:u32)->bool {false}
    fn keypad_press(&mut self, _id:u8, _row:usize, _column:usize)->bool {false}
    fn encoder_steps(&mut self, _id:u8, _steps:i32)->bool {false}
    fn gpio_drive(&mut self, _cycle:u64, _enabled:u64, _output:u64) {}
    fn released_inputs(&self)->Vec<u8> {Vec::new()}
    fn configure_pin_sensors(&mut self,_configs:&[crate::devices::pin_sensor::Config],_hz:u64)->Result<(),String>{Err("board cannot attach GPIO sensors".into())}
    fn pin_sensor_set(&mut self,_id:u8,_field:u32,_value:f64)->bool{false}
    fn pin_sensor_generation(&self,_id:u8)->u32{u32::MAX}
    fn pin_sensor_value(&self,_id:u8,_field:u32)->f64{f64::NAN}

    fn sensor_generation(&mut self, _id:u8)->u32 {u32::MAX}
    fn sensor_value(&mut self, _id:u8, _field:u32)->f64 {f64::NAN}
    fn sensor_set(&mut self, _id:u8, _field:u32, _value:f64)->bool { false }
    fn configure_touches(&mut self, _configs:&[crate::devices::touch::TouchConfig], _hz:u64)->Result<(),String> {Err("board does not support project touch controllers".into())}
    fn touch_device(&mut self, _id:u8)->Option<std::sync::Arc<std::sync::Mutex<crate::devices::touch::TouchController>>> {None}
    fn configure_resistive_touches(&mut self, _configs:&[crate::devices::resistive_touch::ResistiveConfig], _hz:u64)->Result<(),String> {Err("board does not support project resistive touch".into())}
    fn resistive_touch_device(&mut self, _id:u8)->Option<std::sync::Arc<std::sync::Mutex<crate::devices::resistive_touch::ResistiveTouch>>> {None}
    fn name(&self) -> &'static str;
    /// GPIO output level changes, in order.
    fn gpio_changes(&mut self, _changes: &[(u8, bool)]) {}
    /// A completed RMT transmission, decoded to bits by the peripheral model, with the pin the
    /// GPIO matrix has that channel routed to. Drivers that take a fresh channel per refresh
    /// (the Arduino NeoPixel one does) make the channel meaningless; the pin names the strip.
    fn rmt_frame(&mut self, _pin: u8, _bits: &[bool]) {}
    /// GPIO-routed parallel samples at the configured peripheral clock rate.
    fn parallel_output(&mut self, _pins: &[(u8,u8)], _samples:&[u16], _clock_hz:u32) {}
    /// Bytes a GP-SPI master (`host` = 2 or 3) shifted out on MOSI.
    fn spi_tx(&mut self, _host: u8, _data: &[u8]) {}
    /// One complete GP-SPI transaction. The default preserves transmit-only boards and models an
    /// unattached MISO line.
    fn spi_transfer(&mut self, host: u8, tx: &[u8], rx_len: usize) -> Vec<u8> {
        self.spi_tx(host, tx);
        vec![0xff; rx_len]
    }
    fn spi_transfer_pins(&mut self, host: u8, _pins: SpiPins, tx: &[u8], rx_len: usize) -> Vec<u8> {
        self.spi_transfer(host, tx, rx_len)
    }
    fn gpio_events(&self) -> u64 { 0 }
    /// Project LED chains: data pin, RGB pixels, and change counter.
    fn strip_frames(&self) -> Vec<(u8, &[[u8; 3]], u64)> { Vec::new() }
    /// Project display identity, dimensions, pixels, change counter and RGB565 format flag.
    fn project_displays(&self) -> Vec<(u8, u16, u16, Vec<u8>, u64, bool)> { Vec::new() }
    fn display_backlight_pins(&self) -> Vec<u8> { Vec::new() }
    fn display_backlight_duty(&mut self, _pin:u8, _duty:u32) {}
    fn configure_spi_displays(&mut self, _configs: &[crate::devices::spi_display::SpiDisplayConfig]) -> Result<(), String> { Err("board does not support project SPI displays".into()) }
    /// Devices on the I2C buses: (bus, 7-bit address, device).
    fn i2c_devices(&mut self) -> Vec<(u8, u8, Box<dyn I2cDevice>)> { Vec::new() }
    fn camera(&self) -> Option<std::sync::Arc<std::sync::Mutex<crate::devices::camera::Camera>>> { None }
    fn configure_camera(&mut self, _config: crate::devices::camera::CameraConfig) -> bool { false }
    /// Give the board's camera a picture to look at (RGB888).
    fn set_camera_picture(&mut self, _p: crate::picture::Picture) {}
    /// Next camera frame as the sensor would put it on the DVP bus, with its size. None = no camera / nothing to show.
    fn camera_frame(&mut self) -> Option<(u32, u32, std::sync::Arc<Vec<u8>>)> { None }
    /// Small RGB preview of what the camera is looking at (for the UI), if a picture is loaded.
    fn camera_preview(&self, _w: u32, _h: u32) -> Option<Vec<u8>> { None }
    /// A complete frame from the LCD_CAM RGB interface (RGB565 little-endian, `w`x`h`).
    fn lcd_frame(&mut self, _w: u32, _h: u32, _rgb565: &[u8]) {}
    /// The board's display for the UI/PNG: (width, height, RGB565 pixels, change counter).
    fn display(&self) -> Option<(u32, u32, Vec<u16>, u64)> { None }
    /// Completed display frames (for the UI's statistics line).
    fn display_frames(&self) -> u64 { 0 }
    /// Cheap change counter of the display (`display().3` without building the frame).
    fn display_version(&self) -> u64 { 0 }
    /// Prefer waiting one push interval for a quiet pixel stream. The UI still publishes on
    /// the next opportunity during continuous changes so animation cannot starve.
    fn display_quiet_push(&self) -> bool { false }
    /// Raw display memory for a debug PNG: (pixels, columns, rows).
    fn gram(&self) -> Option<(Vec<u16>, usize, usize)> { None }
    /// LED ring / strip: colours and a change counter.
    fn leds(&self) -> Option<(&[[u8; 3]], u64)> { None }
    /// Addressable LED modules besides `leds()`, each with the port it sits in and a change
    /// counter: (id, colours, updates). The UI draws one square grid per entry.
    fn led_grids(&self) -> Vec<(&'static str, &[[u8; 3]], u64)> { Vec::new() }
    /// Touch input from the UI (panel coordinates).
    fn touch(&mut self, _x: u16, _y: u16, _down: bool) {}
    /// Touch input observed at a specific bus horizon. Untimed boards use the ordinary input path.
    fn touch_at(&mut self, _cycle: VirtualCycle, x: u16, y: u16, down: bool) { self.touch(x, y, down); }
    /// Current board-driven GPIO input levels, used to reconnect a persistent board after reset.
    fn input_levels(&self) -> Vec<(u8, bool)> { Vec::new() }
    /// Earliest autonomous transition strictly after the board's current cycle.
    fn next_deadline(&self) -> Option<VirtualCycle> { None }
    /// Advance monotonically through every board transition due by `cycle`.
    fn advance_to(&mut self, _cycle: VirtualCycle) {}
    /// Timestamped GPIO input edges emitted by the last advance.
    fn take_edges(&mut self) -> Vec<BoardEdge> { Vec::new() }
    /// A pin by the name scripts and the UI use (`btn1`, `sw`, ...).
    fn named_pin(&self, _name: &str) -> Option<u8> { None }
    /// The rotary encoder's (CLK, DT) pins, if there is one.
    fn encoder(&self) -> Option<(u8, u8)> { None }
    /// Lines for the end-of-run report.
    fn report(&self) -> String { String::new() }
}

pub type Board = Box<dyn BoardModel>;

/// A bare module: nothing on the pins, console only.
pub struct NoBoard;
impl BoardModel for NoBoard { fn name(&self) -> &'static str { "none" } }
