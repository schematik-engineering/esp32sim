//! The emulator as a WebAssembly module. A C ABI, hand-driven from `web/wasm/worker.js` — no
//! bindgen, no dependencies. The machine produces exactly the messages the WebSocket UI speaks
//! (`docs/web-ui.md`); here they are queued in a `WebServer::queued()` sink and handed to JS.
//!
//! Lifecycle: `esp32sim_new` → `esp32sim_load` (ROM, bootloader, partition table, app, ELF
//! symbols, script) → optional `esp32sim_wifi` → `esp32sim_boot` → repeated `esp32sim_run(cycles,
//! unix_ms)` with `esp32sim_out_*` draining the outbox after each slice and `esp32sim_in_*`
//! feeding the page's inputs.
use esp_soc::observers::{BlockProfile, Coverage, IrqLatency};
use esp_soc::web::{json_escape, WebServer};
use esp_soc::{Machine, Soc, SocBus, Stop};
use std::any::Any;

#[cfg(target_arch = "wasm32")]
use esp32sim_wasm_jit::{compile_shared_sram_block, REGISTER_COUNT};
#[cfg(target_arch = "wasm32")]
use xtensa_lx7::{decode, Bus as _, Op};

/// The dozen calls the ABI makes, over whichever chip this instance is.
trait MachineApi {
    fn configure_pwm_expanders(&mut self,configs:&[esp_soc::devices::pca9685::Config])->Result<(),String>;
    fn pwm_expander_clock(&mut self,id:u8,hz:u32)->bool;
    fn pwm_expander_output(&self,id:u8,channel:u8)->Option<(f64,u32)>;
    fn has_pwm_expander(&self,id:u8)->bool;
    fn configure_load_cells(&mut self,configs:&[esp_soc::devices::hx711::LoadCellConfig])->Result<(),String>;
    fn load_cell_weight(&mut self,id:u32,value:f64)->u32;
    fn load_cell_calibrate(&mut self,id:u32,capacity:f64,sensitivity:f64,offset:i32)->u32;
    fn configure_gestures(&mut self,configs:&[esp_soc::devices::gesture::GestureConfig])->Result<(),String>;
    fn gesture(&mut self,id:u32,direction:u32)->u32;
    fn proximity(&mut self,id:u32,value:f64)->u32;
    fn proximity_reading(&self,id:u32)->u32;
    fn configure_rfid(&mut self,configs:&[esp_soc::devices::rfid::RfidConfig])->Result<(),String>;
    fn rfid_card(&mut self,id:u32,uid:&[u8])->u32;
    fn load(&mut self, kind: u32, d: &[u8], txt: &str) -> Result<(), String>;
    fn write_flash(&mut self, off: usize, d: &[u8]) -> Result<(), String>;
    fn boot(&mut self, app_direct: bool) -> Result<(), String>;
    fn board_name(&self) -> String;
    fn gpio_state(&self, pin: u32) -> u32;
    fn configure_inputs(&mut self,configs:&[esp_soc::devices::inputs::InputConfig])->Result<(),String>;
    fn input_action(&mut self,kind:u32,id:u32,a:i32,b:i32)->u32;
    fn configure_touches(&mut self,configs:&[esp_soc::devices::touch::TouchConfig])->Result<(),String>;
    fn touch_rate(&mut self,id:u32,hz:u32)->u32;
    fn configure_resistive_touches(&mut self,configs:&[esp_soc::devices::resistive_touch::ResistiveConfig])->Result<(),String>;
    fn touch_calibrate(&mut self,id:u32,field:u32,value:u32)->u32;
    fn touch_input(&mut self,id:u32,point:Option<(u16,u16,bool)>)->u32;
    fn configure_circuit(&mut self, strips: &[(u8, usize)], oleds: &[esp_soc::devices::OledConfig], sensors: &[esp_soc::devices::SensorConfig]) -> Result<(), String>;
    fn configure_lcds(&mut self,configs:&[esp_soc::devices::lcd::LcdConfig])->Result<(),String>;
    fn configure_steppers(&mut self,configs:&[esp_soc::devices::stepper::StepperConfig])->Result<(),String>;
    fn stepper_position(&self,id:u32)->f64;
    fn configure_led_displays(&mut self,configs:&[esp_soc::devices::led_display::LedDisplayConfig])->Result<(),String>;
    fn configure_spi_displays(&mut self, configs: &[esp_soc::devices::spi_display::SpiDisplayConfig]) -> Result<(), String>;
    fn configure_pin_sensors(&mut self,configs:&[esp_soc::devices::pin_sensor::Config])->Result<(),String>;
    fn pin_sensor_set(&mut self,id:u32,field:u32,value:f64)->u32;
    fn pin_sensor_generation(&self,id:u32)->u32;
    fn pin_sensor_value(&self,id:u32,field:u32)->f64;
    fn sensor_set(&mut self,id:u32,field:u32,value:f64)->u32;
    fn sensor_generation(&mut self,id:u32)->u32;
    fn sensor_value(&mut self,id:u32,field:u32)->f64;
    fn set_adc(&mut self, pin: u32, value: u32) -> u32;
    fn adc_info(&self,pin:u32,field:u32)->u32;
    fn audio_input(&mut self, target: (u32, u32, u32)) -> Option<&mut Option<esp_periph::pcm::PcmInput>>;
    fn audio_info(&mut self, target: (u32, u32, u32), field: u32) -> u32;
    fn gps_configure(&mut self,id:u32,pin:u32,baud:u32)->u32;
    fn pzem_configure(&mut self,id:u32,tx:u32,rx:u32,range:u32,address:u32)->u32;
    fn pzem_set(&mut self,id:u32,field:u32,value:f64)->u32;
    fn pzem_generation(&self,id:u32)->u32;
    fn pzem_value(&self,id:u32,field:u32)->f64;
    fn radar_configure(&mut self,id:u32,tx:u32,rx:u32)->u32;
    fn radar_set(&mut self,id:u32,field:u32,value:f64)->u32;
    fn radar_generation(&self,id:u32)->u32;
    fn radar_value(&self,id:u32,field:u32)->f64;
    fn gps_fix(&mut self,id:u32,fix:Option<esp_soc::devices::gps::Fix>,unix_ms:f64)->u32;
    fn pwm_output(&self, pin: u32) -> Option<(f64, u32)>;
    fn network_enable(&mut self, enabled: bool) -> bool;
    fn network_receive(&mut self, frame: &[u8]) -> bool;
    fn take_network_frames(&mut self) -> Vec<Vec<u8>>;
    fn web(&self) -> Option<&WebServer>;
    fn run_slice(&mut self, cycles: u32) -> u32;
    fn cpu_hz(&self) -> f64;
    fn cycles(&self) -> f64;
    fn insns(&self) -> f64;
    fn stub(&mut self, name: &str, value: u32) -> u32;
    fn observer(&mut self, name: &str, arg: &str) -> u32;
    fn reports(&mut self) -> String;
    fn set_jit(&mut self, enabled: bool);
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

impl<S: Soc> MachineApi for Machine<S> {
    fn load(&mut self, kind: u32, d: &[u8], txt: &str) -> Result<(), String> {
        match kind {
            0 => self.load_rom(d),
            1 => self.write_flash(0, d), 2 => self.write_flash(0x8000, d), 3 => self.write_flash(0x10000, d),
            4 => self.add_symbols(d),
            5 => self.write_flash(0, d),
            6 => self.load_script(txt),
            7 => esp_soc::picture::parse(d).map(|p| self.bus.board().set_camera_picture(p)),
            _ => Err(format!("unknown load kind {}", kind)),
        }
    }
    fn write_flash(&mut self, off: usize, d: &[u8]) -> Result<(), String> { Machine::write_flash(self, off, d) }
    fn boot(&mut self, app_direct: bool) -> Result<(), String> { if app_direct { self.boot_app(0x10000).map(|_| ()) } else { self.boot_rom(); Ok(()) } }
    fn configure_pin_sensors(&mut self,configs:&[esp_soc::devices::pin_sensor::Config])->Result<(),String> {
        if configs.iter().any(|c|self.gpio_state(c.pin as u32)==u32::MAX){return Err("pin sensor GPIO outside chip range".into());}
        if self.bus.board_ref().name()=="none" {self.bus.set_board(Box::new(esp_soc::devices::CircuitBoard::new(&[],&[])?));}
        self.bus.board().configure_pin_sensors(configs,S::CPU_HZ)?;
        Ok(())
    }
    fn pin_sensor_set(&mut self,id:u32,field:u32,value:f64)->u32 {
        if id>=16{return 1;}u32::from(!self.bus.board().pin_sensor_set(id as u8,field,value))
    }
    fn pin_sensor_generation(&self,id:u32)->u32 {if id>=16{u32::MAX}else{self.bus.board_ref().pin_sensor_generation(id as u8)}}
    fn pin_sensor_value(&self,id:u32,field:u32)->f64 {if id>=16{f64::NAN}else{self.bus.board_ref().pin_sensor_value(id as u8,field)}}
    fn gpio_state(&self, pin: u32) -> u32 {
        let count = match S::NAME { "esp32s3" => 49, "esp32c3" => 22, "esp32c6" => 31, _ => 0 };
        if pin >= count { return u32::MAX; }
        let (out, enable) = self.bus.gpio_output();
        let (pull_up, pull_down) = self.bus.gpio_pulls();
        let mask = 1u64 << pin;
        u32::from(enable & mask != 0) | (u32::from(out & mask != 0) << 1)
            | (u32::from(self.bus.gpio_input() & mask != 0) << 2)
            | (u32::from(pull_up & mask != 0) << 3) | (u32::from(pull_down & mask != 0) << 4)
    }
    fn configure_load_cells(&mut self,configs:&[esp_soc::devices::hx711::LoadCellConfig])->Result<(),String>{
        if configs.iter().flat_map(|c|[c.dout,c.sck]).any(|pin|self.gpio_state(pin as u32)==u32::MAX||(S::NAME=="esp32s3"&&(22..=25).contains(&pin))){return Err("load cell pin outside chip register range".into());}
        if self.bus.board_ref().name()=="none"{self.bus.set_board(Box::new(esp_soc::devices::CircuitBoard::new(&[],&[])?));}
        let old=self.bus.board_ref().input_levels();
        self.bus.board().configure_load_cells(configs,S::CPU_HZ)?;
        let levels=self.bus.board_ref().input_levels();
        for (pin,_) in old {if !levels.iter().any(|(p,_)|*p==pin){self.bus.gpio_set_input(pin,true);}}
        self.bus.refresh_board_inputs();Ok(())
    }
    fn load_cell_weight(&mut self,id:u32,value:f64)->u32{
        if id>255{return 1;}let now=self.bus.cycles();self.bus.board().advance_to(now);
        u32::from(!self.bus.board().load_cell_weight(id as u8,value))
    }
    fn load_cell_calibrate(&mut self,id:u32,capacity:f64,sensitivity:f64,offset:i32)->u32{
        if id>255{return 1;}let now=self.bus.cycles();self.bus.board().advance_to(now);
        u32::from(!self.bus.board().load_cell_calibrate(id as u8,capacity,sensitivity,offset))
    }
    fn configure_gestures(&mut self,configs:&[esp_soc::devices::gesture::GestureConfig])->Result<(),String>{
        if configs.iter().flat_map(|c|[c.sda,c.scl,c.irq]).any(|pin|pin!=255&&(self.gpio_state(pin as u32)==u32::MAX||(S::NAME=="esp32s3"&&(22..=25).contains(&pin)))){return Err("gesture pin outside chip register range".into());}
        if self.bus.board_ref().name()=="none"{self.bus.set_board(Box::new(esp_soc::devices::CircuitBoard::new(&[],&[])?));}
        let old=self.bus.board_ref().input_levels();
        self.bus.board().configure_gestures(configs,S::CPU_HZ)?;
        let levels=self.bus.board_ref().input_levels();
        for (pin,_) in old {if !levels.iter().any(|(p,_)|*p==pin){self.bus.gpio_set_input(pin,true);}}
        self.bus.refresh_board_devices();self.bus.refresh_board_inputs();Ok(())
    }
    fn gesture(&mut self,id:u32,direction:u32)->u32{
        if id>255||!(1..=4).contains(&direction){return 1;}
        let cycle=self.bus.cycles();self.bus.board().advance_to(cycle);
        u32::from(!self.bus.board().gesture(id as u8,direction as u8))
    }
    fn proximity_reading(&self,id:u32)->u32{if id>255{u32::MAX}else{self.bus.board_ref().proximity_reading(id as u8)}}
    fn proximity(&mut self,id:u32,value:f64)->u32{
        if id>255{return 1;}
        let cycle=self.bus.cycles();self.bus.board().advance_to(cycle);
        u32::from(!self.bus.board().proximity(id as u8,value))
    }
    fn configure_rfid(&mut self,configs:&[esp_soc::devices::rfid::RfidConfig])->Result<(),String>{
        if configs.iter().flat_map(|c|[c.sclk,c.mosi,c.miso,c.cs,c.reset]).any(|pin|pin!=255&&(self.gpio_state(pin as u32)==u32::MAX||(S::NAME=="esp32s3"&&(22..=25).contains(&pin)))){return Err("RFID pin outside chip register range".into());}
        if self.bus.board_ref().name()=="none"{self.bus.set_board(Box::new(esp_soc::devices::CircuitBoard::new(&[],&[])?));}
        self.bus.board().configure_rfid(configs,S::CPU_HZ)
    }
    fn rfid_card(&mut self,id:u32,uid:&[u8])->u32{if id>255{return 1;}u32::from(!self.bus.board().rfid_card(id as u8,uid))}
    fn configure_inputs(&mut self,configs:&[esp_soc::devices::inputs::InputConfig])->Result<(),String> {
        if configs.iter().flat_map(|c|c.pins()).any(|pin| self.gpio_state(pin as u32)==u32::MAX || (S::NAME=="esp32s3" && (22..=25).contains(&pin))) {return Err("input pin outside chip register range".into());}
        if self.bus.board_ref().name()=="none" {self.bus.set_board(Box::new(esp_soc::devices::CircuitBoard::new(&[],&[])?));}
        self.bus.board().configure_inputs(configs,S::CPU_HZ)?;
        self.bus.refresh_board_inputs();
        Ok(())
    }
    fn input_action(&mut self,kind:u32,id:u32,a:i32,b:i32)->u32 {
        if id>255 {return 1;}
        let cycle=self.bus.cycles();self.bus.board().advance_to(cycle);
        let accepted=match kind {
            1 if a>=0=>self.bus.board().distance_mm(id as u8,a as u32),
            2 if a>=0 && b>=0=>self.bus.board().keypad_press(id as u8,a as usize,b as usize),
            3=>self.bus.board().encoder_steps(id as u8,a),
            _=>false,
        };
        if accepted {self.bus.refresh_board_inputs();}
        u32::from(!accepted)
    }
    fn configure_touches(&mut self,configs:&[esp_soc::devices::touch::TouchConfig])->Result<(),String> {
        if configs.iter().any(|c|[c.sda,c.scl,c.irq,c.reset].iter().any(|p|*p!=255 && self.gpio_state(*p as u32)==u32::MAX)) {return Err("touch pin outside chip register range".into());}
        if self.bus.board_ref().name()=="none" {self.bus.set_board(Box::new(esp_soc::devices::CircuitBoard::new(&[],&[])?));}
        let old_levels=self.bus.board_ref().input_levels();
        self.bus.board().configure_touches(configs,S::CPU_HZ)?;
        let levels=self.bus.board_ref().input_levels();
        for (pin,_) in old_levels {if !levels.iter().any(|(p,_)|*p==pin) {self.bus.gpio_set_input(pin,true);}}
        for (pin,level) in levels {self.bus.gpio_set_input(pin,level);}
        self.bus.refresh_board_devices();
        Ok(())
    }
    fn configure_resistive_touches(&mut self,configs:&[esp_soc::devices::resistive_touch::ResistiveConfig])->Result<(),String> {
        if configs.iter().any(|c|c.gpio_pins().iter().any(|p|self.gpio_state(*p as u32)==u32::MAX)) {return Err("resistive touch pin outside chip register range".into());}
        if self.bus.board_ref().name()=="none" {self.bus.set_board(Box::new(esp_soc::devices::CircuitBoard::new(&[],&[])?));}
        let old_levels=self.bus.board_ref().input_levels();
        self.bus.board().configure_resistive_touches(configs,S::CPU_HZ)?;
        let levels=self.bus.board_ref().input_levels();
        for (pin,_) in old_levels {if !levels.iter().any(|(p,_)|*p==pin) {self.bus.gpio_set_input(pin,true);}}
        for (pin,level) in levels {self.bus.gpio_set_input(pin,level);}
        self.bus.refresh_board_devices();Ok(())
    }
    fn touch_calibrate(&mut self,id:u32,field:u32,value:u32)->u32 {
        if id>=16 {return 2;}
        let Some(device)=self.bus.board().resistive_touch_device(id as u8) else {return 1;};
        let valid=device.lock().unwrap().calibrate(field,value);if valid {0}else{2}
    }
    fn touch_rate(&mut self,id:u32,hz:u32)->u32 {
        if id>=16 {return 1;}
        let Some(device)=self.bus.board().touch_device(id as u8) else {return 1;};
        let accepted=device.lock().unwrap().set_report_hz(hz);u32::from(!accepted)
    }
    fn touch_input(&mut self,id:u32,point:Option<(u16,u16,bool)>)->u32 {
        if id>=16 {return 2;}
        let now=self.bus.cycles();
        if let Some(device)=self.bus.board().touch_device(id as u8) {
            let mut device=device.lock().unwrap();
            if let Some((x,y,down))=point {device.input(x,y,down);}else{device.clear();}device.advance(now);
        } else if let Some(device)=self.bus.board().resistive_touch_device(id as u8) {
            let mut device=device.lock().unwrap();
            if let Some((x,y,down))=point {device.input(x,y,down);}else{device.clear();}device.advance(now);
        } else {return 1;}
        for (pin,level) in self.bus.board_ref().input_levels() {self.bus.gpio_set_input(pin,level);}
        0
    }
    fn configure_circuit(&mut self, strips: &[(u8, usize)], oleds: &[esp_soc::devices::OledConfig], sensors: &[esp_soc::devices::SensorConfig]) -> Result<(), String> {
        if strips.iter().any(|(pin, _)| self.gpio_state(*pin as u32) == u32::MAX) {
            return Err("LED strip pin is outside the chip's register range".into());
        }
        if oleds.iter().any(|device| [device.sda, device.scl].iter().any(|pin| self.gpio_state(*pin as u32) == u32::MAX)) {
            return Err("OLED pin is outside the chip register range".into());
        }
        if sensors.iter().any(|device| [device.sda,device.scl].iter().any(|pin|self.gpio_state(*pin as u32)==u32::MAX)) {return Err("Sensor pin outside chip register range".into());}
        let mut board=esp_soc::devices::CircuitBoard::new(strips,oleds)?;
        board.configure_sensors(sensors,S::CPU_HZ as u32)?;
        board.camera=self.bus.board_ref().camera();
        self.bus.set_board(Box::new(board));
        Ok(())
    }
    fn configure_lcds(&mut self,configs:&[esp_soc::devices::lcd::LcdConfig])->Result<(),String> {
        if configs.iter().flat_map(|c|[c.sda,c.scl]).any(|pin|self.gpio_state(pin as u32)==u32::MAX || (S::NAME=="esp32s3"&&(22..=25).contains(&pin))) {return Err("LCD GPIO outside chip range".into());}
        if self.bus.board_ref().name()=="none" {self.bus.set_board(Box::new(esp_soc::devices::CircuitBoard::new(&[],&[])?));}
        self.bus.board().configure_lcds(configs,S::CPU_HZ)?;
        self.bus.refresh_board_devices();Ok(())
    }
    fn configure_pwm_expanders(&mut self,configs:&[esp_soc::devices::pca9685::Config])->Result<(),String>{
        if configs.iter().flat_map(|c|[c.sda,c.scl,c.oe]).filter(|p|*p!=255).any(|p|self.gpio_state(p as u32)==u32::MAX || S::NAME=="esp32s3" && (22..=25).contains(&p)){return Err("PWM expander GPIO outside chip range".into());}
        if self.bus.board_ref().name()=="none" {self.bus.set_board(Box::new(esp_soc::devices::CircuitBoard::new(&[],&[])?));}
        self.bus.board().configure_pwm_expanders(configs,S::CPU_HZ)?;self.bus.refresh_board_devices();Ok(())
    }
    fn pwm_expander_clock(&mut self,id:u8,hz:u32)->bool{self.bus.board().pwm_expander_clock(id,hz)}
    fn pwm_expander_output(&self,id:u8,channel:u8)->Option<(f64,u32)>{self.bus.board_ref().pwm_expander_output(id,channel)}
    fn has_pwm_expander(&self,id:u8)->bool{self.bus.board_ref().has_pwm_expander(id)}
    fn configure_steppers(&mut self,configs:&[esp_soc::devices::stepper::StepperConfig])->Result<(),String> {
        if configs.iter().flat_map(|c|c.pins()).any(|pin|self.gpio_state(pin as u32)==u32::MAX || (S::NAME=="esp32s3" && (22..=25).contains(&pin))) {return Err("step/direction GPIO outside chip range".into());}
        if self.bus.board_ref().name()=="none" {self.bus.set_board(Box::new(esp_soc::devices::CircuitBoard::new(&[],&[])?));}
        self.bus.board().configure_steppers(configs)
    }
    fn stepper_position(&self,id:u32)->f64 {if id>=16 {f64::NAN}else{self.bus.board_ref().stepper_position(id as u8)}}
    fn configure_led_displays(&mut self,configs:&[esp_soc::devices::led_display::LedDisplayConfig])->Result<(),String> {
        if configs.iter().flat_map(|c|c.pins()).any(|pin|self.gpio_state(pin as u32)==u32::MAX || (S::NAME=="esp32s3" && (22..=25).contains(&pin))) {return Err("LED display GPIO outside chip range".into());}
        if self.bus.board_ref().name()=="none" {self.bus.set_board(Box::new(esp_soc::devices::CircuitBoard::new(&[],&[])?));}
        self.bus.board().configure_led_displays(configs,S::CPU_HZ)?;
        self.bus.refresh_board_devices();Ok(())
    }
    fn configure_spi_displays(&mut self, configs: &[esp_soc::devices::spi_display::SpiDisplayConfig]) -> Result<(), String> {
        if configs.iter().any(|c| [Some(c.sclk),Some(c.mosi),c.cs,Some(c.dc),c.reset,c.backlight].into_iter().flatten().any(|pin| self.gpio_state(pin as u32)==u32::MAX)) { return Err("SPI display pin outside chip register range".into()); }
        self.bus.board().configure_spi_displays(configs)
    }
    fn audio_input(&mut self, (kind, port, pin): (u32, u32, u32)) -> Option<&mut Option<esp_periph::pcm::PcmInput>> {
        match kind { 0 => self.bus.audio_adc(pin), 1 => self.bus.audio_i2s(port)?.inputs.get_mut(pin as usize), _ => None }
    }
    fn audio_info(&mut self, target: (u32, u32, u32), field: u32) -> u32 {
        if target.0 == 1 {
            let Some(i2s) = self.bus.audio_i2s(target.1) else { return 0; };
            if i2s.selected_input != Some(target.2 as usize) { return 0; }
            match field { 0 => i2s.rx_rate(), 1 => i2s.rx_bits(), 2 => i2s.rx_channels(), 3 => 1, 4 => target.1, _ => 0 }
        } else {
            let Some(Some(input)) = self.audio_input(target) else { return 0; };
            match field { 0 => input.sample_rate, 1 => 16, 2 => input.channels, 3 => 1, _ => 0 }
        }
    }
    fn pzem_configure(&mut self,id:u32,tx:u32,rx:u32,range:u32,address:u32)->u32 {
        if self.gpio_state(tx)==u32::MAX || self.gpio_state(rx)==u32::MAX || (S::NAME=="esp32s3" && [tx,rx].iter().any(|p|(22..=25).contains(p))) || ![10,100].contains(&range) || !(1..=247).contains(&address) {return 1;}
        u32::from(!self.configure_pzem(id as usize,tx as u8,rx as u8,range as u8,address as u8))
    }
    fn pzem_set(&mut self,id:u32,field:u32,value:f64)->u32 {u32::from(!self.set_pzem(id as usize,field,value))}
    fn pzem_generation(&self,id:u32)->u32 {self.pzem_generation(id as usize)}
    fn pzem_value(&self,id:u32,field:u32)->f64 {self.pzem_value(id as usize,field)}
    fn radar_configure(&mut self, id: u32, tx: u32, rx: u32) -> u32 {
        if self.gpio_state(tx) == u32::MAX || self.gpio_state(rx) == u32::MAX || (S::NAME == "esp32s3" && [tx,rx].iter().any(|p|(22..=25).contains(p))) {
            return 1;
        }
        u32::from(!self.configure_radar(id as usize, tx as u8, rx as u8))
    }
    fn radar_set(&mut self, id: u32, field: u32, value: f64) -> u32 {
        u32::from(!self.set_radar(id as usize, field, value))
    }
    fn radar_generation(&self,id:u32)->u32 {self.radar_generation(id as usize)}
    fn radar_value(&self,id:u32,field:u32)->f64 {self.radar_value(id as usize,field)}
    fn gps_configure(&mut self,id:u32,pin:u32,baud:u32)->u32 {
        if id>=4 || self.gpio_state(pin)==u32::MAX || !(1200..=115200).contains(&baud){return 1;}
        u32::from(!self.configure_gps(id as usize,pin as u8,baud))
    }
    fn gps_fix(&mut self,id:u32,fix:Option<esp_soc::devices::gps::Fix>,unix_ms:f64)->u32 {
        u32::from(!self.set_gps_fix(id as usize,fix,unix_ms))
    }
    fn sensor_generation(&mut self,id:u32)->u32 {if id>255{u32::MAX}else{self.bus.board().sensor_generation(id as u8)}}
    fn sensor_value(&mut self,id:u32,field:u32)->f64 {if id>255{f64::NAN}else{self.bus.board().sensor_value(id as u8,field)}}
    fn sensor_set(&mut self,id:u32,field:u32,value:f64)->u32 {if id>255{return 1;}u32::from(!self.bus.board().sensor_set(id as u8,field,value))}
    fn adc_info(&self,pin:u32,field:u32)->u32 {
        let Some(sample)=self.bus.adc_observation(pin) else {return u32::MAX;};
        match field {0=>sample.generation,1=>u32::from(sample.counts),_=>u32::MAX}
    }
    fn set_adc(&mut self, pin: u32, value: u32) -> u32 { u32::from(!self.bus.adc_set_input(pin, value)) }
    fn pwm_output(&self, pin: u32) -> Option<(f64, u32)> {
        if self.gpio_state(pin) == u32::MAX { return None; }
        self.bus.pwm_output(pin)
    }
    fn network_enable(&mut self, enabled: bool) -> bool { self.bus.network_enable(enabled) }
    fn network_receive(&mut self, frame: &[u8]) -> bool { self.bus.network_receive(frame) }
    fn take_network_frames(&mut self) -> Vec<Vec<u8>> { self.bus.take_network_frames() }
    fn board_name(&self) -> String { self.bus.board_ref().name().to_string() }
    fn web(&self) -> Option<&WebServer> { self.web.as_ref() }
    fn run_slice(&mut self, cycles: u32) -> u32 {
        // The browser consumes serial through the outbox; retain only this slice for diagnostics.
        self.console.all.clear();
        self.max_cycles = self.bus.cycles() + cycles as u64;
        loop {
            match self.run(u64::MAX) {
                Stop::Halted | Stop::MaxInsns => return 0,
                Stop::SwReset => {
                    let cause = self.bus.reset_cause();
                    let note = format!("[emu] chip reset at t={:.3}s: cause {:#x} ({})", self.seconds(), cause, esp_periph::reset_cause_name(cause));
                    log(&note);
                    if let Some(w) = &self.web { w.send_text(&format!("{{\"t\":\"emu\",\"msg\":\"{}\"}}", json_escape(&note))); }
                    self.reboot();
                }
                Stop::Unimplemented(pc, raw) => { log(&format!("[emu] unimplemented instruction at {:08x} {} (raw {:#x})", pc, self.sym(pc), raw)); return 2; }
                Stop::Ebreak(pc) => { log(&format!("[emu] ebreak at {:08x} {}", pc, self.sym(pc))); return 3; }
                Stop::Breakpoint(_) => return 3,
                Stop::Exceptions(_) => return 4,
                Stop::Simcall(_) => return 5,
                Stop::Watch(..) => return 6,
                Stop::CostModel { reason, .. } | Stop::CostModelLifecycle { reason, .. } => { log(&format!("[emu] cost model: {}", reason)); return 7; }
            }
        }
    }
    fn cpu_hz(&self) -> f64 { S::CPU_HZ as f64 }
    fn cycles(&self) -> f64 { self.bus.cycles() as f64 }
    fn insns(&self) -> f64 { Machine::insns(self) as f64 }
    fn stub(&mut self, name: &str, value: u32) -> u32 {
        let by_addr = name.strip_prefix("0x").and_then(|h| u32::from_str_radix(h, 16).ok());
        match by_addr.or_else(|| self.sym_addr(name)) {
            Some(addr) => { self.stubs.insert(addr, value); log(&format!("[emu] stub {} @ {:#x} -> returns {:#x}", name, addr, value)); 0 }
            None => { log(&format!("[emu] stub: no symbol '{}' (load the app ELF first)", name)); 1 }
        }
    }
    fn observer(&mut self, name: &str, arg: &str) -> u32 {
        match name {
            "profile-blocks" => { self.add_observer(Box::new(BlockProfile::new(20))); 0 }
            "coverage" => { self.add_observer(Box::new(Coverage::new(None))); 0 }
            "irq-latency" => { self.add_observer(Box::new(IrqLatency::new(S::CORES))); 0 }
            "trace-fn" => { let n: Vec<(u32, String)> = self.symbols.iter().filter(|(_, s)| s.starts_with(arg)).map(|(a, s)| (*a, s.clone())).collect(); for (a, s) in n { self.fn_probes.insert(a, s); } 0 }
            _ => { log(&format!("[emu] unknown observer '{}'", name)); 1 }
        }
    }
    fn reports(&mut self) -> String { Machine::reports(self) }
    fn set_jit(&mut self, enabled: bool) { for core in &mut self.cores { xtensa_lx7::Core::set_jit(core, enabled); } }
    fn as_any_mut(&mut self) -> &mut dyn Any { self }
}

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "env")]
extern "C" { fn host_log(ptr: *const u8, len: usize); }
#[cfg(not(target_arch = "wasm32"))]
unsafe fn host_log(ptr: *const u8, len: usize) {
    // SAFETY: The caller provides a readable string pointer for exactly `len` bytes.
    let message = unsafe { std::slice::from_raw_parts(ptr, len) };
    eprintln!("{}", String::from_utf8_lossy(message));
}

fn log(s: &str) {
    // SAFETY: `s` is readable for its length and the host does not retain the pointer.
    unsafe { host_log(s.as_ptr(), s.len()); }
}

pub struct Emu {
    m: Box<dyn MachineApi>,
    /// the last drained outbox: (1 text | 2 binary, payload), addressed by index from JS
    out: Vec<(u8, Vec<u8>)>,
    network_frames: Vec<Vec<u8>>,
    audio_targets: [Vec<(u32, u32, u32)>; 16],
    servos: [Option<esp_soc::devices::servo::Servo>; 16],
    booted: bool,
    #[cfg(target_arch = "wasm32")]
    jit: BrowserJit,
}

#[cfg(target_arch = "wasm32")]
const JIT_STATE_LEN: usize = 80;
#[cfg(target_arch = "wasm32")]
const JIT_MODULE_LIMIT: usize = 1024;

#[cfg(target_arch = "wasm32")]
struct BrowserJit {
    state: Box<[u8; JIT_STATE_LEN]>,
    modules: Vec<CachedJitModule>,
    ticket: Option<JitTicket>,
}

#[cfg(target_arch = "wasm32")]
struct CachedJitModule {
    pc: u32,
    code: Vec<u8>,
    module: Vec<u8>,
    receipt_cycles: u64,
}

#[cfg(target_arch = "wasm32")]
struct JitTicket {
    module_id: u32,
    pc: u32,
    next_pc: u32,
    last_pc: u32,
    ccount: u32,
    insns: u64,
    bus_cycles: u64,
    instruction_count: u32,
    receipt_cycles: u64,
    code_pages: Vec<(u32, u32)>,
}

/// Borrow an ABI buffer.
///
/// # Safety
/// For nonzero `len`, `ptr` must be non-null and readable for `len` bytes. The memory must remain
/// unchanged and valid for the returned lifetime. A null pointer is accepted only when `len` is 0.
unsafe fn bytes<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    if len == 0 { &[] } else {
        // SAFETY: The caller supplies the validity, immutability, and lifetime guarantees above.
        unsafe { std::slice::from_raw_parts(ptr, len) }
    }
}

/// Borrow a UTF-8 ABI buffer, treating invalid UTF-8 as an empty string.
///
/// # Safety
/// The pointer and length must satisfy `bytes`'s contract.
unsafe fn text<'a>(ptr: *const u8, len: usize) -> &'a str {
    // SAFETY: The caller satisfies `bytes`'s pointer and lifetime contract.
    std::str::from_utf8(unsafe { bytes(ptr, len) }).unwrap_or("")
}

/// Buffers the page fills before handing them to `esp32sim_load` / `esp32sim_in_*`.
#[no_mangle] pub extern "C" fn esp32sim_alloc(len: usize) -> *mut u8 { let mut v = vec![0u8; len.max(1)]; let p = v.as_mut_ptr(); std::mem::forget(v); p }
/// Release a buffer returned by `esp32sim_alloc`.
///
/// # Safety
/// `ptr` must be the live pointer returned by `esp32sim_alloc(len)`. It must not be used again.
#[no_mangle] pub unsafe extern "C" fn esp32sim_free(ptr: *mut u8, len: usize) {
    // SAFETY: The caller returns the allocation with the same length and unique ownership.
    drop(unsafe { Vec::from_raw_parts(ptr, len.max(1), len.max(1)) });
}

/// `board` is a CLI board name (atech14, waveshare-cam, waveshare-lcd4b,
/// waveshare-amoled18-v2, none) for the ESP32-S3,
/// or `esp32c3` for the RISC-V chip, which is console-only and takes no board. Null on failure.
///
/// # Safety
/// For nonzero `board_len`, `board` must be non-null and readable for `board_len` bytes throughout
/// this call. A null pointer is accepted only when `board_len` is 0.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_new(board: *const u8, board_len: usize, flash_mb: u32, psram_mb: u32) -> *mut Emu {
    std::panic::set_hook(Box::new(|info| log(&format!("[emu] panic: {}", info))));
    // SAFETY: The caller provides a readable board-name buffer for this call.
    let board = unsafe { text(board, board_len) }.to_string();
    let (flash_mb, psram_mb) = (flash_mb.max(1) as usize, psram_mb as usize);
    let m: Box<dyn MachineApi> = if board == "esp32c3" || board == "c3" {
        let mut m = esp32c3::machine([0x3c, 0x84, 0x27, 0xb6, 0xa7, 0x1c], flash_mb << 20);
        m.bus.set_flash_size(flash_mb << 20);
        m.console.mask = 2;                                  // the ROM mirrors its console to UART0 and USB-Serial/JTAG
        prepare(&mut m);
        Box::new(m)
    } else if board == "esp32c6" || board == "c6" || board.starts_with("waveshare-c6") || board.ends_with("lcd147") {
        let mut m = esp32c6::machine([0xdc, 0x1e, 0xd5, 0x6e, 0x8c, 0xdc], flash_mb << 20);
        let name = if board == "esp32c6" || board == "c6" { "none" } else { board.as_str() };
        let Some(b) = esp32c6::board::make_board(name) else { log(&format!("[emu] unknown board '{}'", board)); return std::ptr::null_mut() };
        m.bus.board = b;
        m.bus.set_flash_size(flash_mb << 20);
        m.console.mask = 2;
        prepare(&mut m);
        Box::new(m)
    } else {
        let mut m = esp32s3::machine([0x44, 0x1b, 0xf6, 0x75, 0xdc, 0xe0]);
        let Some(b) = esp32s3::board::make_board(&board) else { log(&format!("[emu] unknown board '{}'", board)); return std::ptr::null_mut() };
        m.bus.board = b;
        m.bus.attach_board_devices();
        m.bus.set_flash_size(flash_mb << 20);
        let _ = m.bus.set_psram_size(psram_mb << 20);
        m.bus.periph.lcd_cam.frame_cycles = esp32s3::periph::CPU_HZ / 10;
        prepare(&mut m);
        Box::new(m)
    };
    Box::into_raw(Box::new(Emu {
        m,
        out: Vec::new(),
        network_frames: Vec::new(),
        audio_targets: std::array::from_fn(|_| Vec::new()),
        servos: [None;16],
        booted: false,
        #[cfg(target_arch = "wasm32")]
        jit: BrowserJit {
            state: Box::new([0; JIT_STATE_LEN]),
            modules: Vec::new(),
            ticket: None,
        },
    }))
}

/// The page is the one client: messages queue in a `WebServer` sink; the worker paces the run.
fn prepare<S: Soc>(m: &mut Machine<S>) {
    m.web = Some(WebServer::queued());
    m.rt.enabled = false;                                    // std::time does not exist here
    m.console.capture = true;
}

/// Destroy an emulator returned by `esp32sim_new`. A null pointer is ignored.
///
/// # Safety
/// A non-null `e` must be the live pointer returned by `esp32sim_new`. The caller must have
/// exclusive access, and the pointer must not be used again.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_delete(e: *mut Emu) {
    if !e.is_null() {
        // SAFETY: The caller returns the live allocation with unique ownership.
        drop(unsafe { Box::from_raw(e) });
    }
}

/// kind: 0 mask-ROM ELF, 1 bootloader (flash 0x0), 2 partition table (0x8000), 3 app (0x10000),
/// 4 ELF for symbols, 5 whole flash image (0x0), 6 script text, 7 camera picture (BMP/PPM).
/// Returns 0, or 1 with the reason logged.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access. For nonzero `len`,
/// `ptr` must be non-null and readable for `len` bytes throughout this call.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_load(e: *mut Emu, kind: u32, ptr: *const u8, len: usize) -> u32 {
    // SAFETY: The caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    // SAFETY: The caller provides a readable input buffer for this call.
    let data = unsafe { bytes(ptr, len) };
    let input_text = std::str::from_utf8(data).unwrap_or("");
    match e.m.load(kind, data, input_text) { Ok(()) => 0, Err(msg) => { log(&format!("[emu] load kind {}: {}", kind, msg)); 1 } }
}

/// Write bytes into flash at an arbitrary offset (a data partition's contents).
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access. For nonzero `len`,
/// `ptr` must be non-null and readable for `len` bytes throughout this call.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_load_at(e: *mut Emu, offset: u32, ptr: *const u8, len: usize) -> u32 {
    // SAFETY: The caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    // SAFETY: The caller provides a readable input buffer for this call.
    let data = unsafe { bytes(ptr, len) };
    match e.m.write_flash(offset as usize, data) { Ok(()) => 0, Err(msg) => { log(&format!("[emu] flash {:#x}: {}", offset, msg)); 1 } }
}

/// Attach the virtual access point and subnet: `ssid=NAME,psk=PASS,chan=N`. No NAT — the browser
/// has no sockets — so DHCP, DNS, SNTP and ICMP answer, and connections past the gateway are refused.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access. For nonzero `len`,
/// `spec` must be non-null and readable for `len` bytes throughout this call.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_wifi(e: *mut Emu, spec: *const u8, len: usize) {
    // SAFETY: The caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };

    let mut cfg = esp32s3::wifi::ApConfig { ssid: "esp32sim".into(), bssid: [0x02, 0x53, 0x49, 0x4d, 0x00, 0x01], channel: 6, psk: None };
    // SAFETY: The caller provides a readable setup string for this call.
    for kv in unsafe { text(spec, len) }.split(',') {
        match kv.split_once('=') {
            Some(("ssid", v)) => cfg.ssid = v.to_string(),
            Some(("chan", v)) | Some(("channel", v)) => cfg.channel = v.parse().unwrap_or(6),
            Some(("psk", v)) | Some(("password", v)) => cfg.psk = Some(v.to_string()),
            _ => {}
        }
    }
    if configure_wifi(e, cfg) != 0 { log("[emu] wifi: radio is not modelled for this chip"); }

}

fn wifi_mac(e: &mut Emu) -> Option<&mut esp32s3::periph::WifiMac> {
    let machine = e.m.as_any_mut();
    if machine.is::<esp32s3::Machine>() { machine.downcast_mut::<esp32s3::Machine>().map(|m| &mut m.bus.periph.wifi) }
    else if machine.is::<esp32c3::Machine>() { machine.downcast_mut::<esp32c3::Machine>().map(|m| &mut m.bus.periph.wifi) }
    else { machine.downcast_mut::<esp32c6::Machine>().map(|m| &mut m.bus.periph.wifi.mac) }
}

fn configure_wifi(e: &mut Emu, cfg: esp32s3::wifi::ApConfig) -> u32 {
    let Some(wifi) = wifi_mac(e) else { return 1; };
    wifi.ap = Some(esp32s3::wifi::VirtualAp::new(cfg, false));
    wifi.net = Some(esp32s3::net::VirtualNet::new(false));
    0
}

/// Configure Wi-Fi with literal UTF-8 credentials. Status:0 accepted,1 unsupported chip,
/// 2 invalid config. Empty password means open;64 hex digits supply a raw WPA2 PSK.
/// Credentials are never logged. Channel1..14 and SSID1..32 bytes are accepted.
/// # Safety
/// Non-null `e` must be live/exclusive; non-null buffers must be readable for their lengths.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_wifi_configure(e: *mut Emu, ssid: *const u8, ssid_len: usize,
    password: *const u8, password_len: usize, channel: u32) -> u32 {
    if e.is_null() || ssid.is_null() || !(1..=32).contains(&ssid_len) || password_len > 64
        || (password.is_null() && password_len != 0) || !(1..=14).contains(&channel) { return 2; }
    let Ok(ssid) = std::str::from_utf8(unsafe { bytes(ssid, ssid_len) }) else { return 2; };
    let Ok(password) = std::str::from_utf8(unsafe { bytes(password, password_len) }) else { return 2; };
    let valid_password = password.is_empty() || (8..=63).contains(&password_len)
        || password_len == 64 && password.bytes().all(|b| b.is_ascii_hexdigit());
    if !valid_password { return 2; }
    configure_wifi(unsafe { &mut *e }, esp32s3::wifi::ApConfig {
        ssid: ssid.into(), bssid:[2,0x53,0x49,0x4d,0,1], channel:channel as u8,
        psk: if password.is_empty() { None } else { Some(password.into()) },
    })
}

/// Real AP protocol state:0 disconnected,1 authenticating/key negotiation,2 associated
/// with WPA2 keys installed (or an open network); u32::MAX means unsupported/invalid.
/// # Safety
/// A non-null `e` must be a live exclusively borrowed emulator.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_wifi_state(e: *mut Emu) -> u32 {
    if e.is_null() { return u32::MAX; }
    let Some(wifi) = wifi_mac(unsafe { &mut *e }) else { return u32::MAX; };
    let Some(ap) = &wifi.ap else { return 0; };
    match ap.state {
        esp32s3::wifi::StaState::Idle => 0,
        esp32s3::wifi::StaState::Authenticated => 1,
        esp32s3::wifi::StaState::Associated => if ap.cfg.psk.is_none() || ap.wpa.msg == 4 { 2 } else { 1 },
    }
}

/// `--stub NAME[=value]`: return `value` immediately when execution reaches the function's entry.
/// NAME is a symbol (needs the ELF loaded) or a hex address. Returns 1 if it cannot be resolved.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access. For nonzero `len`,
/// `name` must be non-null and readable for `len` bytes throughout this call.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_stub(e: *mut Emu, name: *const u8, len: usize, value: u32) -> u32 {
    // SAFETY: The caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    // SAFETY: The caller provides a readable symbol name for this call.
    e.m.stub(unsafe { text(name, len) }, value)
}

/// Attach an analysis: `profile-blocks`, `coverage`, `irq-latency` (no argument), `trace-fn`
/// (arg = symbol prefix as text). Returns 1 for an unknown name.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access. For each nonzero
/// length, its corresponding `name` or `arg` pointer must be non-null and readable throughout this
/// call.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_observer(e: *mut Emu, name: *const u8, len: usize, arg: *const u8, arg_len: usize) -> u32 {
    // SAFETY: The caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    // SAFETY: The caller provides readable name and argument buffers for this call.
    e.m.observer(unsafe { text(name, len) }, unsafe { text(arg, arg_len) })
}

/// Every observer's report so far, as `emu` messages in the outbox.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_reports(e: *mut Emu) {
    // SAFETY: The caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    let r = e.m.reports();
    if let Some(w) = e.m.web() { for line in r.lines() { w.send_text(&format!("{{\"t\":\"emu\",\"msg\":\"{}\"}}", json_escape(line))); } }
}

/// Start from the mask ROM (the normal path) or, with `app_direct` set, straight into the app image.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_boot(e: *mut Emu, app_direct: u32) -> u32 {
    // SAFETY: The caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    if let Err(msg) = e.m.boot(app_direct != 0) { log(&format!("[emu] boot: {}", msg)); return 1; }
    // the WebSocket server announces the board in its per-client hello; here there is one client
    let name = e.m.board_name();
    if let Some(w) = e.m.web() { w.send_text(&format!("{{\"t\":\"board\",\"name\":\"{}\"}}", name)); }
    e.booted = true; 0
}

/// Configure project devices before boot using eight-byte records.
/// Kind 1: [1, pin, count_lo, count_hi, 0, 0, 0, 0].
/// Kind 2: [2, id, SDA, SCL, address, width, height, 0] for SSD1306.
/// Kind 3: [3, id, SDA, SCL, address, model, shunt_lo, shunt_hi].
/// Models: 1 BME280, 2 BMP280, 3 BH1750, 4 MPU6050, 5 INA219, 6 DS3231.
/// Shunt is milliohms for INA219 (0 defaults to 100); other models require 0.
/// Returns 0 on success or 1 for invalid configuration/lifecycle.
///
/// # Safety
/// `e` must point to an exclusively borrowed live emulator and `data` must be
/// readable for `len` bytes. No data pointer is retained.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_configure_circuit(e: *mut Emu, data: *const u8, len: usize) -> u32 {
    if e.is_null() || len > 48 * 8 || len % 8 != 0 || (len != 0 && data.is_null()) { return 1; }
    let e = unsafe { &mut *e };
    if e.booted { return 1; }
    let bytes = if len == 0 { &[] } else { unsafe { std::slice::from_raw_parts(data, len) } };
    let mut strips = Vec::new();
    let mut oleds = Vec::new();
    let mut sensors = Vec::new();
    for record in bytes.chunks_exact(8) {
        match record[0] {
            1 => strips.push((record[1], u16::from_le_bytes([record[2], record[3]]) as usize)),
            2 => oleds.push(esp_soc::devices::OledConfig { id: record[1], sda: record[2], scl: record[3], address: record[4], width: record[5], height: record[6] }),
            3 => sensors.push(esp_soc::devices::SensorConfig{id:record[1],sda:record[2],scl:record[3],address:record[4],model:record[5],shunt_milliohms:u16::from_le_bytes([record[6],record[7]])}),
            _ => return 1,
        }
    }
    match e.m.configure_circuit(&strips, &oleds, &sensors) { Ok(()) => 0, Err(message) => { log(&message); 1 } }
}

/// Configure MFRC522 readers before boot: model1,id,SCLK,MOSI,MISO,CS,reset(or255),zero.
/// # Safety
/// `e` must be exclusive and `data` readable for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_configure_rfid(e:*mut Emu,data:*const u8,len:usize)->u32{
    if e.is_null()||len>128||len%8!=0||(len!=0&&data.is_null()){return 1;}
    let e=unsafe{&mut *e};if e.booted{return 1;}
    let bytes=if len==0{&[]}else{unsafe{std::slice::from_raw_parts(data,len)}};
    let mut configs=Vec::new();
    for r in bytes.chunks_exact(8){if r[0]!=1||r[7]!=0{return 1;}configs.push(esp_soc::devices::rfid::RfidConfig{id:r[1],sclk:r[2],mosi:r[3],miso:r[4],cs:r[5],reset:r[6]});}
    match e.m.configure_rfid(&configs){Ok(())=>0,Err(error)=>{log(&error);1}}
}
/// Present/replace a single card with4/7/10 UID bytes; zero length removes it.
/// # Safety
/// `e` must be exclusive and `data` readable for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_rfid_card(e:*mut Emu,id:u32,data:*const u8,len:usize)->u32{
    if e.is_null()||![0,4,7,10].contains(&len)||(len!=0&&data.is_null()){return 1;}
    let uid=if len==0{&[]}else{unsafe{std::slice::from_raw_parts(data,len)}};
    unsafe{&mut *e}.m.rfid_card(id,uid)
}

/// Configure physical inputs before boot with 20-byte records:
/// kind, id, row count, column count, eight row/first-side pins, eight column/second-side pins.
/// Unused pin slots must be 255. Kind 1 is HC-SR04 trigger/echo, kind 2 keypad,
/// kind 3 encoder A/B. Kinds 1 and 3 require exactly one pin on each side.
/// # Safety
/// `e` must be exclusively borrowed; `data` must be readable for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_configure_inputs(e:*mut Emu,data:*const u8,len:usize)->u32 {
    if e.is_null() || len>16*20 || len%20!=0 || (len!=0&&data.is_null()) {return 1;}
    let e=unsafe{&mut *e};if e.booted{return 1;}
    let bytes=if len==0 {&[]} else {unsafe{std::slice::from_raw_parts(data,len)}};
    let mut configs=Vec::new();
    for r in bytes.chunks_exact(20) {
        let rows=r[2] as usize;let columns=r[3] as usize;
        if !(1..=8).contains(&rows)||!(1..=8).contains(&columns)||r[4+rows..12].iter().chain(&r[12+columns..20]).any(|&p|p!=255) {return 1;}
        use esp_soc::devices::inputs::InputConfig;
        configs.push(match r[0] {
            1 if rows==1&&columns==1=>InputConfig::Ultrasonic{id:r[1],trigger:r[4],echo:r[12]},
            2=>InputConfig::Keypad{id:r[1],rows:r[4..4+rows].to_vec(),columns:r[12..12+columns].to_vec()},
            3 if rows==1&&columns==1=>InputConfig::Encoder{id:r[1],a:r[4],b:r[12]},
            _=>return 1,
        });
    }
    match e.m.configure_inputs(&configs) {Ok(())=>0,Err(error)=>{log(&error);1}}
}
/// Set an HC-SR04 target distance in millimetres; 0 means no echo, maximum 4000.
/// # Safety
/// `e` must be null or an exclusively borrowed live emulator.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_distance_mm(e:*mut Emu,id:u32,value:u32)->u32 {
    if e.is_null()||value>4000{return 1;}unsafe{&mut *e}.m.input_action(1,id,value as i32,0)
}
/// Queue a physical keypad press (150 ms held, 50 ms released), at most 32 queued keys.
/// # Safety
/// `e` must be null or an exclusively borrowed live emulator.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_keypad_press(e:*mut Emu,id:u32,row:u32,column:u32)->u32 {
    if e.is_null()||row>=8||column>=8{return 1;}unsafe{&mut *e}.m.input_action(2,id,row as i32,column as i32)
}
/// Queue signed encoder quarter steps, 2 ms apart, at most 1024 pending steps.
/// # Safety
/// `e` must be null or an exclusively borrowed live emulator.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_encoder_steps(e:*mut Emu,id:u32,steps:i32)->u32 {
    if e.is_null(){return 1;}unsafe{&mut *e}.m.input_action(3,id,steps,0)
}

/// Set an external sensor's physical input: field 0 Celsius, 1 percent RH,
/// 2 pascals, 3 lux, 4..6 acceleration XYZ in m/s², 7..9 gyro XYZ rad/s,
/// 10 bus volts, 11 shunt millivolts, 12 current milliamps, 14 Unix seconds,
/// 15 shunt milliohms. Field 13 is read-only power in milliwatts.
/// Returns 1 for invalid identity, field or range.
/// # Safety
/// Non-null `e` must be an exclusively borrowed live emulator.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_sensor_set(e:*mut Emu,id:u32,field:u32,value:f64)->u32 {
    if e.is_null(){return 1;}unsafe{&mut *e}.m.sensor_set(id,field,value)
}

/// Completed sensor conversion generation, 0 before the first result, MAX if invalid.
/// # Safety
/// Non-null `e` must be an exclusively borrowed live emulator.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_sensor_generation(e:*mut Emu,id:u32)->u32 {
    if e.is_null(){return u32::MAX;}unsafe{&mut *e}.m.sensor_generation(id)
}
/// Latched, compensated sensor result; NaN if invalid, skipped or not yet measured.
/// # Safety
/// Non-null `e` must be an exclusively borrowed live emulator.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_sensor_value(e:*mut Emu,id:u32,field:u32)->f64 {
    if e.is_null(){return f64::NAN;}unsafe{&mut *e}.m.sensor_value(id,field)
}

/// Enable relay transport after configuring the virtual AP. Returns 0 on success, 1 if unsupported.
/// # Safety
/// Non-null `e` must be a live exclusively borrowed emulator.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_net_enable(e: *mut Emu, enabled: u32) -> u32 {
    if e.is_null() || enabled > 1 { return 1; }
    let e = unsafe { &mut *e };
    e.network_frames.clear();
    u32::from(!e.m.network_enable(enabled == 1))
}

/// Queue one host-to-guest Ethernet II frame. Returns 0 on success, 1 if unavailable/full, 2 if invalid.
/// # Safety
/// `e` must be live/exclusive; non-null `data` must be readable for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_net_rx(e: *mut Emu, data: *const u8, len: usize) -> u32 {
    if e.is_null() || data.is_null() || !(14..=1518).contains(&len) { return 2; }
    let e = unsafe { &mut *e };
    u32::from(!e.m.network_receive(unsafe { bytes(data, len) }))
}

/// Drain up to 64 guest Ethernet frames; ptr/len remain valid until the next take or enable call.
/// # Safety
/// Non-null `e` must be a live exclusively borrowed emulator.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_net_tx_take(e: *mut Emu) -> u32 {
    if e.is_null() { return 0; }
    let e = unsafe { &mut *e };
    e.network_frames = e.m.take_network_frames();
    e.network_frames.len() as u32
}

/// # Safety
/// Non-null `e` must be live with shared access; copy returned bytes before the next take/enable.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_net_tx_ptr(e: *const Emu, index: usize) -> *const u8 {
    if e.is_null() { return std::ptr::null(); }
    unsafe { &*e }.network_frames.get(index).map_or(std::ptr::null(), |f| f.as_ptr())
}

/// # Safety
/// Non-null `e` must be live with shared access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_net_tx_len(e: *const Emu, index: usize) -> usize {
    if e.is_null() { return 0; }
    unsafe { &*e }.network_frames.get(index).map_or(0, Vec::len)
}

/// PWM frequency in Hz, or zero when no modeled LEDC output drives this pin.
///
/// # Safety
/// A non-null `e` must point to a live emulator with shared access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_pwm_frequency(e: *const Emu, pin: u32) -> f64 {
    if e.is_null() { return 0.0; }
    unsafe { &*e }.m.pwm_output(pin).map_or(0.0, |p| p.0)
}

/// PWM high-time fraction scaled to 0..65535; u32::MAX means inactive or invalid.
///
/// # Safety
/// A non-null `e` must point to a live emulator with shared access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_pwm_duty(e: *const Emu, pin: u32) -> u32 {
    if e.is_null() { return u32::MAX; }
    unsafe { &*e }.m.pwm_output(pin).map_or(u32::MAX, |p| p.1)
}

/// Read one GPIO's output enable (bit 0), output latch (bit 1), effective input
/// level (bit 2), pull-up (bit 3) and pull-down (bit 4). Returns u32::MAX for a pin outside the chip's register range.
/// This is a level snapshot; use timestamped GPIO observations for pulse protocols.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has shared access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_gpio_state(e: *const Emu, pin: u32) -> u32 {
    // SAFETY: The caller provides a live emulator for this read.
    unsafe { &*e }.m.gpio_state(pin)
}

/// Run for `cycles` more emulated cycles. Returns 0 while the machine can go on; otherwise a stop
/// code: 2 unimplemented instruction, 3 breakpoint/ebreak, 4 exception limit, 5 semihosting call.
/// A chip reset (esp_restart, watchdog) reboots through the ROM and keeps going, like the CLI.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_run(e: *mut Emu, cycles: u32, unix_ms: f64) -> u32 {
    // SAFETY: The caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    if !e.booted { return 9; }
    #[cfg(target_arch = "wasm32")] esp_soc::host::set_unix_time_ms(unix_ms as u64);
    let _ = unix_ms;
    e.m.run_slice(cycles)
}

/// Offer the browser one complete receipt-priced, side-effect-free S3 SRAM scheduling quantum.
/// A nonzero return value is a stable module id; zero means the normal interpreter must run. The
/// generated module shares this module's exported memory and writes only an internal handoff
/// record; architectural state changes only after `esp32sim_jit_commit` validates it.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub unsafe extern "C" fn esp32sim_jit_prepare(e: *mut Emu, requested: u32, unix_ms: f64) -> u32 {
    let e = unsafe { &mut *e };
    if !e.booted {
        return 0;
    }
    esp_soc::host::set_unix_time_ms(unix_ms as u64);
    let Emu { m, jit, .. } = e;
    let Some(machine) = m.as_any_mut().downcast_mut::<esp32s3::Machine>() else {
        return 0;
    };
    prepare_browser_jit(machine, jit, requested).unwrap_or(0)
}

#[cfg(target_arch = "wasm32")]
fn prepare_browser_jit(
    machine: &mut esp32s3::Machine,
    jit: &mut BrowserJit,
    requested: u32,
) -> Option<u32> {
    jit.ticket = None;
    let cpu = &machine.cores[0];
    let limit = machine.browser_external_block_budget(requested)?;
    for compare in cpu.ccompare {
        let distance = compare.wrapping_sub(cpu.ccount);
        if distance != 0 && distance < limit {
            return None;
        }
    }

    let (start_pc, mut pc) = (cpu.pc, cpu.pc);
    let mut code = Vec::with_capacity(limit as usize * 3);
    let mut last_pc = start_pc;
    let mut instruction_count = 0u32;
    while instruction_count < limit {
        let bytes = machine.bus.fetch(pc).ok()?;
        let instruction = decode(pc, bytes);
        if !matches!(
            instruction.op,
            Op::L32i | Op::L32iN | Op::MoviN | Op::Memw | Op::Sub | Op::Saltu
        ) || instruction.len == 0
            || window_overflow_possible(cpu, supported_max_ar(&instruction))
        {
            break;
        }
        let next_pc = pc.wrapping_add(u32::from(instruction.len));
        if cpu.lcount != 0 && next_pc == cpu.lend {
            break;
        }
        code.extend_from_slice(&bytes[..instruction.len as usize]);
        last_pc = pc;
        pc = next_pc;
        instruction_count += 1;
    }
    if instruction_count != limit {
        return None;
    }

    let mut page_indices = Vec::new();
    let last_byte = pc.wrapping_sub(1);
    let page_size = 1u32 << xtensa_lx7::bus::VPAGE_SHIFT;
    let mut page_address = start_pc;
    loop {
        let index = machine.bus.code_page(page_address);
        if page_indices.last() != Some(&index) {
            page_indices.push(index);
        }
        if page_address / page_size == last_byte / page_size {
            break;
        }
        page_address = (page_address / page_size + 1) * page_size;
    }
    let versions = machine.bus.page_versions();
    let code_pages = page_indices
        .into_iter()
        .map(|index| (index, versions.get(index as usize).copied().unwrap_or(0)))
        .collect();

    let state_offset = u32::try_from(jit.state.as_ptr() as usize).ok()?;
    let dram_len = (esp32s3::bus::DRAM_HIGH - esp32s3::bus::DRAM_LOW) as usize;
    let dram_storage_offset = machine.bus.sram.len().checked_sub(dram_len)?;
    // SAFETY: `dram_storage_offset` was derived by subtracting `dram_len` from this allocation.
    let dram_ptr = unsafe { machine.bus.sram.as_ptr().add(dram_storage_offset) };
    let dram_offset = u32::try_from(dram_ptr as usize).ok()?;
    let module_index = if let Some(index) = jit
        .modules
        .iter()
        .position(|cached| cached.pc == start_pc && cached.code == code)
    {
        index
    } else {
        if jit.modules.len() >= JIT_MODULE_LIMIT {
            return None;
        }
        let compiled = compile_shared_sram_block(
            start_pc,
            &code,
            state_offset,
            dram_offset,
            esp32s3::bus::DRAM_LOW,
            dram_len,
        )
        .ok()?;
        let receipt_cycles = compiled.cycle_cost;
        jit.modules.push(CachedJitModule {
            pc: start_pc,
            code: code.clone(),
            module: compiled.bytes,
            receipt_cycles,
        });
        jit.modules.len() - 1
    };

    let receipt_cycles = jit.modules[module_index].receipt_cycles;
    write_jit_state(jit, cpu, start_pc);
    let module_id = module_index as u32 + 1;
    jit.ticket = Some(JitTicket {
        module_id,
        pc: start_pc,
        next_pc: pc,
        last_pc,
        ccount: cpu.ccount,
        insns: cpu.insn_count,
        bus_cycles: machine.bus.cycles,
        instruction_count,
        receipt_cycles,
        code_pages,
    });
    Some(module_id)
}

#[cfg(target_arch = "wasm32")]
fn window_overflow_possible(cpu: &xtensa_lx7::Cpu, max_ar: u8) -> bool {
    use xtensa_lx7::state::ps;
    if max_ar < 4 || cpu.ps & ps::WOE == 0 || cpu.ps & ps::EXCM != 0 {
        return false;
    }
    (1..=u32::from(max_ar / 4)).any(|frame| {
        cpu.windowstart & (1 << ((cpu.windowbase + frame) & 15)) != 0
    })
}

#[cfg(target_arch = "wasm32")]
fn supported_max_ar(instruction: &xtensa_lx7::Insn) -> u8 {
    match instruction.op {
        Op::L32i | Op::L32iN => instruction.s.max(instruction.t),
        Op::MoviN => instruction.s,
        Op::Sub | Op::Saltu => instruction.r.max(instruction.s).max(instruction.t),
        Op::Memw => 0,
        _ => unreachable!("called only after the supported-opcode check"),
    }
}

#[cfg(target_arch = "wasm32")]
fn write_jit_state(jit: &mut BrowserJit, cpu: &xtensa_lx7::Cpu, pc: u32) {
    for register in 0..REGISTER_COUNT {
        store_jit_u32(&mut jit.state[..], register * 4, cpu.get_ar(register as u8));
    }
    store_jit_u32(&mut jit.state[..], esp32sim_wasm_jit::PC_OFFSET, pc);
    jit.state[esp32sim_wasm_jit::CYCLE_OFFSET..esp32sim_wasm_jit::CYCLE_OFFSET + 8]
        .copy_from_slice(&0u64.to_le_bytes());
}

#[cfg(target_arch = "wasm32")]
fn store_jit_u32(state: &mut [u8], offset: usize, value: u32) {
    state[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

#[cfg(target_arch = "wasm32")]
fn load_jit_u32(state: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(state[offset..offset + 4].try_into().unwrap())
}

#[cfg(target_arch = "wasm32")]
fn load_jit_u64(state: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(state[offset..offset + 8].try_into().unwrap())
}

/// Commit the prepared sidecar result. Returns 1 when committed and 0 when validation failed.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub unsafe extern "C" fn esp32sim_jit_commit(e: *mut Emu) -> u32 {
    let e = unsafe { &mut *e };
    let Some(ticket) = e.jit.ticket.take() else {
        return 0;
    };
    let Some(machine) = e.m.as_any_mut().downcast_mut::<esp32s3::Machine>() else {
        return 0;
    };
    let cpu = &machine.cores[0];
    let versions = machine.bus.page_versions();
    let unchanged = cpu.pc == ticket.pc
        && cpu.ccount == ticket.ccount
        && cpu.insn_count == ticket.insns
        && machine.bus.cycles == ticket.bus_cycles
        && ticket.code_pages.iter().all(|&(index, version)| {
            versions.get(index as usize).copied().unwrap_or(0) == version
        })
        && load_jit_u32(&e.jit.state[..], esp32sim_wasm_jit::PC_OFFSET) == ticket.next_pc
        && load_jit_u64(&e.jit.state[..], esp32sim_wasm_jit::CYCLE_OFFSET) == ticket.receipt_cycles
        && machine
            .browser_external_block_budget(ticket.instruction_count)
            .is_some_and(|budget| budget >= ticket.instruction_count);
    if !unchanged {
        return 0;
    }

    let cpu = &mut machine.cores[0];
    for register in 0..REGISTER_COUNT {
        cpu.set_ar(
            register as u8,
            load_jit_u32(&e.jit.state[..], register * 4),
        );
    }
    cpu.pc = ticket.next_pc;
    cpu.insn_count += u64::from(ticket.instruction_count);
    cpu.advance_ccount(ticket.instruction_count);
    machine.bus.note_pc(ticket.last_pc);
    if matches!(
        machine.finish_browser_external_quantum(),
        Some(Stop::SwReset)
    ) {
        let cause = machine.bus.reset_cause();
        let note = format!(
            "[emu] chip reset at t={:.3}s: cause {:#x} ({})",
            machine.seconds(),
            cause,
            esp_periph::reset_cause_name(cause)
        );
        log(&note);
        if let Some(web) = &machine.web {
            web.send_text(&format!(
                "{{\"t\":\"emu\",\"msg\":\"{}\"}}",
                json_escape(&note)
            ));
        }
        machine.reboot();
    }
    1
}

/// Discard a prepared sidecar result after the generated module trapped.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub unsafe extern "C" fn esp32sim_jit_abort(e: *mut Emu) {
    unsafe { &mut *e }.jit.ticket = None;
}

/// Pointer to the currently prepared sidecar module, or null without a ticket.
///
/// # Safety
/// `e` must point to a live emulator and no mutable access may overlap this call.
#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub unsafe extern "C" fn esp32sim_jit_module_ptr(e: *mut Emu) -> *const u8 {
    let e = unsafe { &*e };
    let Some(ticket) = &e.jit.ticket else {
        return std::ptr::null();
    };
    e.jit.modules[(ticket.module_id - 1) as usize].module.as_ptr()
}

/// Length of the currently prepared sidecar module.
///
/// # Safety
/// `e` must point to a live emulator and no mutable access may overlap this call.
#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub unsafe extern "C" fn esp32sim_jit_module_len(e: *mut Emu) -> usize {
    let e = unsafe { &*e };
    let Some(ticket) = &e.jit.ticket else {
        return 0;
    };
    e.jit.modules[(ticket.module_id - 1) as usize].module.len()
}

/// The emulated CPU clock, so the driver paces the right chip: 240 MHz on the S3, 160 on the C3.
///
/// # Safety
/// `e` must point to a live emulator, and no mutable access may overlap this call.
#[no_mangle] pub unsafe extern "C" fn esp32sim_cpu_hz(e: *mut Emu) -> f64 {
    // SAFETY: The caller provides shared access to a live emulator without overlapping mutation.
    unsafe { &*e }.m.cpu_hz()
}
/// Return the current emulated cycle count.
///
/// # Safety
/// `e` must point to a live emulator, and no mutable access may overlap this call.
#[no_mangle] pub unsafe extern "C" fn esp32sim_cycles(e: *mut Emu) -> f64 {
    // SAFETY: The caller provides shared access to a live emulator without overlapping mutation.
    unsafe { &*e }.m.cycles()
}
/// Return the current emulated instruction count.
///
/// # Safety
/// `e` must point to a live emulator, and no mutable access may overlap this call.
#[no_mangle] pub unsafe extern "C" fn esp32sim_insns(e: *mut Emu) -> f64 {
    // SAFETY: The caller provides shared access to a live emulator without overlapping mutation.
    unsafe { &*e }.m.insns()
}

/// Drain what the machine sent since the last call; then index it with the accessors below.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_out_take(e: *mut Emu) -> u32 {
    // SAFETY: The caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    e.out = e.m.web().map(|w| w.take_outbox()).unwrap_or_default();
    e.out.len() as u32
}
/// Return the kind of one message from the last output drain, or 0 for an invalid index.
///
/// # Safety
/// `e` must point to a live emulator, and no mutable access may overlap this call.
#[no_mangle] pub unsafe extern "C" fn esp32sim_out_kind(e: *mut Emu, i: u32) -> u32 {
    // SAFETY: The caller provides shared access to a live emulator without overlapping mutation.
    unsafe { &*e }.out.get(i as usize).map(|m| m.0 as u32).unwrap_or(0)
}
/// Return a message's data pointer, or null for an invalid index.
///
/// # Safety
/// `e` must point to a live emulator, and no mutable access may overlap this call. A non-null
/// result remains valid until the next `esp32sim_out_take` or `esp32sim_delete` call for `e`.
#[no_mangle] pub unsafe extern "C" fn esp32sim_out_ptr(e: *mut Emu, i: u32) -> *const u8 {
    // SAFETY: The caller provides shared access to a live emulator without overlapping mutation.
    unsafe { &*e }.out.get(i as usize).map(|m| m.1.as_ptr()).unwrap_or(std::ptr::null())
}
/// Return a message's data length, or 0 for an invalid index.
///
/// # Safety
/// `e` must point to a live emulator, and no mutable access may overlap this call.
#[no_mangle] pub unsafe extern "C" fn esp32sim_out_len(e: *mut Emu, i: u32) -> usize {
    // SAFETY: The caller provides shared access to a live emulator without overlapping mutation.
    unsafe { &*e }.out.get(i as usize).map(|m| m.1.len()).unwrap_or(0)
}

/// Page input in the WebSocket JSON protocol.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access. For nonzero `len`,
/// `ptr` must be non-null and readable for `len` bytes throughout this call.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_in_text(e: *mut Emu, ptr: *const u8, len: usize) {
    // SAFETY: The caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    // SAFETY: The caller provides a readable input buffer for this call.
    let input = unsafe { text(ptr, len) };
    if let Some(w) = e.m.web() { w.push_incoming(input.to_string()); }
}
/// Page input in the WebSocket binary protocol.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access. For nonzero `len`,
/// `ptr` must be non-null and readable for `len` bytes throughout this call.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_in_bin(e: *mut Emu, ptr: *const u8, len: usize) {
    // SAFETY: The caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    // SAFETY: The caller provides a readable input buffer for this call.
    let input = unsafe { bytes(ptr, len) };
    if let Some(w) = e.m.web() { w.push_incoming_bin(input.to_vec()); }
}

/// Enable or disable the scheduler-integrated block JIT. The interpreter remains available.
///
/// # Safety
/// `e` must be a live exclusively borrowed emulator.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_set_jit(e: *mut Emu, enabled: u32) {
    // SAFETY: The ABI caller guarantees a live exclusive handle.
    unsafe { &mut *e }.m.set_jit(enabled != 0);
}

/// Guest instructions retired by compiled blocks, including interpreter helpers.
///
/// # Safety
/// `e` must be a live exclusively borrowed emulator.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_block_jit_insns(e: *mut Emu) -> f64 {
    // SAFETY: The ABI caller guarantees a live exclusive handle.
    let e = unsafe { &mut *e };
    e.m.as_any_mut().downcast_mut::<esp32s3::Machine>()
        .map(|m| m.cores.iter().map(|c| c.blocks.jit_instructions).sum::<u64>() as f64)
        .unwrap_or(0.0)
}

#[cfg(all(target_arch = "wasm32", feature = "jit-tests"))]
mod jit_tests;

/// Emit the optional statistical block profile through host_log.
///
/// # Safety
/// `e` must be a live exclusively borrowed emulator.
#[cfg(all(target_arch = "wasm32", feature = "jit-profile"))]
#[no_mangle]
pub unsafe extern "C" fn esp32sim_profile_report(e: *mut Emu) {
    // SAFETY: the ABI caller guarantees a live exclusive handle.
    if let Some(m) = unsafe { &mut *e }.m.as_any_mut().downcast_mut::<esp32s3::Machine>() {
        for (i, core) in m.cores.iter().enumerate() {
            log(&format!("core={i}\n{}", core.blocks.profile.report()));
            if let Some(r) = core.blocks.region_report() { log(&format!("core={i} {r}")); }
        }
    }
}

/// Runs the generated-code differential suite in a real WASM runtime.
#[cfg(all(target_arch = "wasm32", feature = "jit-tests"))]
#[no_mangle]
pub extern "C" fn esp32sim_test_block_jit() -> u32 {
    std::panic::set_hook(Box::new(|info| log(&format!("[jit test] {info}"))));
    xtensa_lx7::jit::tests::run_tests() + jit_tests::run()
}

// ---------------------------------------------------------------- a network of C6 motes
//
// The single-machine ABI above runs one emulator; this one runs several on a shared medium
// (`esp32c6::net`), so the page can boot a whole 802.15.4 network with no simulator behind it.
// The stepping and the medium stay in Rust — the caller only says how far to run and reads what
// came out — because that is where `run_until_cycle` and `radio_receive` already make the timing
// exact, and where a native test can hold it to that (`esp32c6/tests/net.rs`).

/// A network plus the buffers its accessors hand out.
pub struct Net { net: esp32c6::net::Network, console: Vec<u8> }

/// A new network. `slice_ns` is how far every node runs before the medium looks again (0: the
/// default 100 µs); shorter is more exact and slower.
#[no_mangle] pub extern "C" fn esp32sim_net_new(slice_ns: f64) -> *mut Net {
    std::panic::set_hook(Box::new(|info| log(&format!("[emu] panic: {}", info))));
    let mut net = esp32c6::net::Network::new();
    if slice_ns > 0.0 { net.slice_ns = slice_ns as u64; }
    Box::into_raw(Box::new(Net { net, console: Vec::new() }))
}

/// Destroy a network returned by `esp32sim_net_new`. A null pointer is ignored.
///
/// # Safety
/// A non-null `n` must be the live pointer returned by `esp32sim_net_new`, with exclusive access,
/// and must not be used again.
#[no_mangle] pub unsafe extern "C" fn esp32sim_net_delete(n: *mut Net) {
    // SAFETY: The caller returns the live allocation with unique ownership.
    if !n.is_null() { drop(unsafe { Box::from_raw(n) }); }
}

/// Add a node and return its index. `mac` is six bytes — two nodes must not share one, since
/// Contiki takes its link-layer address from the efuses and drops a frame that looks like its
/// own. `start_ns` staggers the power-on: identical images booted together stay in lockstep and
/// collide forever. `board` may be empty for a bare module.
///
/// # Safety
/// `n` must point to a live network with exclusive access; `mac` must be readable for 6 bytes and
/// `board` for `board_len` bytes throughout this call.
#[no_mangle] pub unsafe extern "C" fn esp32sim_net_add(n: *mut Net, mac: *const u8, flash_mb: u32, start_ns: f64, x: f64, y: f64, board: *const u8, board_len: usize) -> u32 {
    // SAFETY: The caller provides exclusive access to a live network.
    let n = unsafe { &mut *n };
    // SAFETY: The caller provides a readable six-byte MAC for this call.
    let mac_bytes = unsafe { bytes(mac, 6) };
    let mut m = [0u8; 6];
    m.copy_from_slice(&mac_bytes[..6.min(mac_bytes.len())]);
    // SAFETY: The caller provides a readable board name for this call.
    let board = unsafe { text(board, board_len) };
    let flash = (flash_mb.max(1) as usize) << 20;
    n.net.add(m, flash, start_ns.max(0.0) as u64, x, y, board) as u32
}

/// Load an image into one node: the same `kind` numbering as `esp32sim_load`.
///
/// # Safety
/// `n` must point to a live network with exclusive access; for nonzero `len`, `ptr` must be
/// readable for `len` bytes throughout this call.
#[no_mangle] pub unsafe extern "C" fn esp32sim_net_load(n: *mut Net, node: u32, kind: u32, ptr: *const u8, len: usize) -> u32 {
    // SAFETY: The caller provides exclusive access to a live network.
    let n = unsafe { &mut *n };
    // SAFETY: The caller provides a readable input buffer for this call.
    let data = unsafe { bytes(ptr, len) };
    let Some(node) = n.net.nodes.get_mut(node as usize) else { return 1 };
    let m = &mut node.m;
    let r = match kind {
        0 => m.load_rom(data),
        1 => m.write_flash(0x0, data), 2 => m.write_flash(0x8000, data), 3 => m.write_flash(0x10000, data),
        4 => m.add_symbols(data),
        5 => m.write_flash(0x0, data),
        _ => Err(format!("unknown load kind {}", kind)),
    };
    match r { Ok(()) => 0, Err(msg) => { log(&format!("[emu] net load kind {}: {}", kind, msg)); 1 } }
}

/// Stub a function on one node, by symbol name or `0x`-prefixed address.
///
/// # Safety
/// `n` must point to a live network with exclusive access; `name` must be readable for `len`
/// bytes throughout this call.
#[no_mangle] pub unsafe extern "C" fn esp32sim_net_stub(n: *mut Net, node: u32, name: *const u8, len: usize, value: u32) -> u32 {
    // SAFETY: The caller provides exclusive access to a live network.
    let n = unsafe { &mut *n };
    // SAFETY: The caller provides a readable symbol name for this call.
    let name = unsafe { text(name, len) };
    let Some(node) = n.net.nodes.get_mut(node as usize) else { return 1 };
    let addr = node.m.sym_addr(name).or_else(|| u32::from_str_radix(name.trim_start_matches("0x"), 16).ok());
    match addr { Some(a) => { node.m.stubs.insert(a, value); 0 } None => { log(&format!("[emu] net stub: unknown symbol {}", name)); 1 } }
}

/// Boot every node from its reset vector.
///
/// # Safety
/// `n` must point to a live network to which the caller has exclusive access.
#[no_mangle] pub unsafe extern "C" fn esp32sim_net_boot(n: *mut Net) -> u32 {
    // SAFETY: The caller provides exclusive access to a live network.
    let n = unsafe { &mut *n };
    if n.net.nodes.is_empty() { log("[emu] net: no nodes"); return 1; }
    n.net.boot(); 0
}

/// Advance the whole network to `until_ns` of network time.
///
/// # Safety
/// `n` must point to a live network to which the caller has exclusive access.
#[no_mangle] pub unsafe extern "C" fn esp32sim_net_run(n: *mut Net, until_ns: f64) {
    // SAFETY: The caller provides exclusive access to a live network.
    let n = unsafe { &mut *n };
    n.net.run_until(until_ns.max(0.0) as u64);
}

/// Network time in nanoseconds.
///
/// # Safety
/// `n` must point to a live network, and no mutable access may overlap this call.
#[no_mangle] pub unsafe extern "C" fn esp32sim_net_now_ns(n: *mut Net) -> f64 {
    // SAFETY: The caller provides shared access without overlapping mutation.
    unsafe { &*n }.net.now_ns as f64
}

/// Collect one node's console bytes into the network's buffer and return how many there are;
/// `esp32sim_net_console_ptr` then reads them, until the next call.
///
/// # Safety
/// `n` must point to a live network to which the caller has exclusive access.
#[no_mangle] pub unsafe extern "C" fn esp32sim_net_console_take(n: *mut Net, node: u32) -> usize {
    // SAFETY: The caller provides exclusive access to a live network.
    let n = unsafe { &mut *n };
    n.console = n.net.take_console(node as usize);
    n.console.len()
}

/// The bytes from the last `esp32sim_net_console_take`.
///
/// # Safety
/// `n` must point to a live network, and no mutable access may overlap this call.
#[no_mangle] pub unsafe extern "C" fn esp32sim_net_console_ptr(n: *mut Net) -> *const u8 {
    // SAFETY: The caller provides shared access without overlapping mutation.
    unsafe { &*n }.console.as_ptr()
}

/// One counter of one node: 0 frames sent, 1 frames taken, 2 frames refused (a collision or a
/// radio not listening), 3 the node's own clock in ns, 4 its WS2812 as 0xRRGGBB, 5 that LED's
/// change count, 6 nonzero once the node has halted.
///
/// # Safety
/// `n` must point to a live network, and no mutable access may overlap this call.
#[no_mangle] pub unsafe extern "C" fn esp32sim_net_stat(n: *mut Net, node: u32, which: u32) -> f64 {
    // SAFETY: The caller provides shared access without overlapping mutation.
    let n = unsafe { &*n };
    let i = node as usize;
    let Some(nd) = n.net.nodes.get(i) else { return 0.0 };
    match which {
        0 => nd.tx as f64, 1 => nd.rx as f64, 2 => nd.rx_dropped as f64,
        3 => nd.now_ns() as f64,
        4 => n.net.led(i).0 as f64, 5 => n.net.led(i).1 as f64,
        6 => nd.halted as u32 as f64,
        _ => 0.0,
    }
}

#[cfg(test)]
mod gpio_tests {
    use super::*;

    #[test]
    fn each_browser_slice_releases_previous_serial_history() {
        let mut m = esp32c3::machine([0; 6], 4 << 20);
        m.console.all = vec![b'x'; 1 << 20];
        assert_eq!(MachineApi::run_slice(&mut m, 0), 0);
        assert!(m.console.all.is_empty());
    }

    #[test]
    fn io_mux_pulls_reach_gpio_registers_and_browser_snapshots() {
        macro_rules! check {
            ($machine:expr, $mux:expr) => {{
                let mut m = $machine;
                m.bus.periph.write32($mux + 20, 1 << 7);
                assert_eq!(MachineApi::gpio_state(&m, 4), 16);
                m.bus.periph.write32($mux + 20, 1 << 8);
                assert_eq!(MachineApi::gpio_state(&m, 4), 12);
                m.bus.periph.gpio.write(0x24, 16);
                assert_eq!(MachineApi::gpio_state(&m, 4), 9);
                m.bus.periph.gpio.write(0x8, 16);
                assert_eq!(MachineApi::gpio_state(&m, 4), 15);
                m.bus.gpio_set_input(4, false);
                assert_eq!(MachineApi::gpio_state(&m, 4), 11);
                m.bus.reboot([0; 6]);
                assert_eq!(MachineApi::gpio_state(&m, 4), 0);
            }};
        }
        check!(esp32s3::machine([0; 6]), 0x60009000);
        check!(esp32c3::machine([0; 6], 4 << 20), 0x60009000);
        check!(esp32c6::machine([0; 6], 4 << 20), 0x60090000);
    }

    #[test]
    fn gpio_snapshots_preserve_direction_and_levels_for_each_chip() {
        macro_rules! check {
            ($machine:expr, $count:expr) => {{
                let mut m = $machine;
                m.bus.periph.gpio.write(0x24, 1 << 4);
                m.bus.periph.gpio.write(0x8, 1 << 4);
                m.bus.gpio_set_input(4, false);
                assert_eq!(MachineApi::gpio_state(&m, 4), 3);
                m.bus.periph.gpio.write(0xc, 1 << 4);
                assert_eq!(MachineApi::gpio_state(&m, 4), 1);
                m.bus.periph.gpio.write(0x28, 1 << 4);
                m.bus.gpio_set_input(4, true);
                assert_eq!(MachineApi::gpio_state(&m, 4), 4);
                assert_eq!(MachineApi::gpio_state(&m, $count), u32::MAX);
                assert_eq!(MachineApi::gpio_state(&m, u32::MAX), u32::MAX);
            }};
        }
        check!(esp32s3::machine([0; 6]), 49);
        check!(esp32c3::machine([0; 6], 4 << 20), 22);
        check!(esp32c6::machine([0; 6], 4 << 20), 31);
    }

    #[test]
    fn pwm_routes_real_registers_and_rejects_invalid_pins() {
        macro_rules! check {
            ($machine:expr, $count:expr, $base:expr, $signal:expr, $shift:expr, $clken:expr, $reset:expr) => {{
                let mut m = $machine;
                let p = &mut m.bus.periph;
                p.write32($clken, 1 << 11);
                p.write32($reset, 0);
                // C6 selects and gates the LEDC source clock in PCR.
                if $shift == 1 { p.write32(0x60096034, 1); p.write32(0x60096038, 7 << 20); }
                p.write32($base + 0xd0, 3);
                p.write32($base + 0xa0, 8 | (40000 << (4 + $shift)) | (1 << (25 + $shift)));
                p.write32($base + 8, 128 << 4);
                p.write32($base + 12, 1 << 31);
                p.write32($base, 4 | (1 << 4));
                p.gpio.enable = 1 << 4;
                p.gpio.func_out_sel[4] = $signal;
                p.tick(240000);
                assert_eq!(MachineApi::pwm_output(&m, 4), Some((1000.0, 32768)));
                assert_eq!(MachineApi::pwm_output(&m, $count), None);
                assert_eq!(MachineApi::pwm_output(&m, u32::MAX), None);
                if $shift == 1 { m.bus.periph.write32(0x60096034, 3); }
                else { m.bus.periph.write32($reset, 1 << 11); }
                assert_eq!(MachineApi::pwm_output(&m, 4), None);
            }};
        }
        check!(esp32s3::machine([0; 6]), 49, 0x60019000, 73, 0, 0x600c0018, 0x600c0020);
        check!(esp32c3::machine([0; 6], 4 << 20), 22, 0x60019000, 45, 0, 0x600c0010, 0x600c0018);
        check!(esp32c6::machine([0; 6], 4 << 20), 31, 0x60007000, 0, 1, 0x60096034, 0x60096034);
        unsafe {
            assert_eq!(esp32sim_pwm_frequency(std::ptr::null(), 0), 0.0);
            assert_eq!(esp32sim_pwm_duty(std::ptr::null(), 0), u32::MAX);
        }
    }
    #[test]
    fn ethernet_transport_is_bounded_and_rejects_unmodeled_radios() {
        let mut s3 = esp32s3::machine([0; 6]);
        assert!(!MachineApi::network_enable(&mut s3, true));
        s3.bus.periph.wifi.ap = Some(esp32s3::wifi::VirtualAp::new(esp32s3::wifi::ApConfig {
            ssid: "fixture".into(), bssid: [2; 6], channel: 6, psk: None,
        }, false));
        assert!(MachineApi::network_enable(&mut s3, true));
        assert!(!MachineApi::network_receive(&mut s3, &[0; 13]));
        assert!(!MachineApi::network_receive(&mut s3, &[0; 1519]));
        for _ in 0..64 { assert!(MachineApi::network_receive(&mut s3, &[0; 14])); }
        assert!(!MachineApi::network_receive(&mut s3, &[0; 14]));
        s3.bus.periph.wifi.eth_tx.push(vec![1; 14]);
        assert_eq!(MachineApi::take_network_frames(&mut s3), vec![vec![1; 14]]);
        assert!(MachineApi::take_network_frames(&mut s3).is_empty());
        assert!(MachineApi::network_enable(&mut s3, false));
        assert!(s3.bus.periph.wifi.eth_rx.is_empty());
        assert!(!MachineApi::network_receive(&mut s3, &[0; 14]));
        let mut c3 = esp32c3::machine([0;6],4<<20);
        assert!(!MachineApi::network_enable(&mut c3,true));
        c3.bus.periph.wifi.ap = Some(esp32c3::wifi::VirtualAp::new(esp32c3::wifi::ApConfig {
            ssid: "fixture".into(), bssid:[2,0,0,0,0,1], channel:6, psk:None,
        }, false));
        assert!(MachineApi::network_enable(&mut c3,true));
        for _ in 0..64 { assert!(MachineApi::network_receive(&mut c3,&[0;14])); }
        assert!(!MachineApi::network_receive(&mut c3,&[0;14]));
        c3.reboot();
        assert_eq!(c3.bus.periph.wifi.ap.as_ref().unwrap().cfg.ssid, "fixture");
        assert!(c3.bus.periph.wifi.eth_rx.is_empty());
        assert!(MachineApi::network_enable(&mut c3,true));
        assert!(!MachineApi::network_enable(&mut esp32c6::machine([0;6],4<<20),true));
        unsafe {
            assert_eq!(esp32sim_net_rx(std::ptr::null_mut(),std::ptr::null(),1519),2);
            assert_eq!(esp32sim_net_enable(std::ptr::null_mut(),1),1);
            assert!(esp32sim_net_tx_ptr(std::ptr::null(),0).is_null());
            assert_eq!(esp32sim_net_tx_len(std::ptr::null(),0),0);
        }
    }

}

/// Last completed physical conversion: field 0 generation, field 1 raw counts.
/// Host input changes do not advance the generation. Invalid arguments return UINT32_MAX.
/// # Safety
/// `e` must be null or a live emulator pointer returned by esp32sim_new.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_adc_info(e:*const Emu,pin:u32,field:u32)->u32 {
    if e.is_null(){return u32::MAX;}
    unsafe { &*e }.m.adc_info(pin,field)
}

/// Set a 12-bit sample for an ADC-capable GPIO. Returns zero on success.
/// # Safety
/// `e` must be a live emulator pointer returned by esp32sim_new.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_set_adc(e: *mut Emu, pin: u32, value: u32) -> u32 {
    if e.is_null() { return 1; }
    unsafe { &mut *e }.m.set_adc(pin, value)
}

#[cfg(test)]
mod adc_tests {
    use super::*;
    #[test]
    fn host_samples_survive_firmware_reboot_on_all_chips() {
        macro_rules! check { ($m:expr, $pin:expr) => {{
            let mut m = $m;
            assert_eq!(MachineApi::set_adc(&mut m, $pin, 3072), 0);
            assert_eq!(MachineApi::set_adc(&mut m, $pin, 4096), 1);
            assert_eq!(MachineApi::adc_info(&m,$pin,0),0);
            assert_eq!(MachineApi::adc_info(&m,99,0),u32::MAX);
            assert_eq!(MachineApi::adc_info(&m,$pin,2),u32::MAX);
            if $pin==1 { esp_periph::Device::write(&mut m.bus.periph.adc,0x0c,(1<<19)|(1<<17)); }
            else {esp_periph::Device::write(&mut m.bus.periph.adc,0x20,(1<<31)|(1<<29));}
            esp_periph::Device::tick(&mut m.bus.periph.adc,14);
            assert_eq!(MachineApi::adc_info(&m,$pin,0),1);
            assert_eq!(MachineApi::adc_info(&m,$pin,1),3072);
            esp_soc::SocBus::reboot(&mut m.bus, [0; 6]);
            assert_eq!(m.bus.periph.adc.inputs[0][0], 3072);
            assert_eq!(MachineApi::adc_info(&m,$pin,0),1);
            assert_eq!(MachineApi::adc_info(&m,$pin,1),3072);
        }} }
        check!(esp32s3::machine([0; 6]), 1);
        check!(esp32c3::machine([0; 6], 4 << 20), 0);
        check!(esp32c6::machine([0; 6], 4 << 20), 0);
        assert_eq!(unsafe { esp32sim_set_adc(std::ptr::null_mut(), 0, 0) }, 1);
        assert_eq!(unsafe{esp32sim_adc_info(std::ptr::null(),0,0)},u32::MAX);
    }
}

#[cfg(test)]
mod wifi_config_tests {
    use super::*;
    #[test]
    fn literal_credentials_validate_and_state_tracks_ap_protocol() {
        unsafe {
            for board in ["none", "esp32c3", "esp32c6"] {
                let emu = esp32sim_new(board.as_ptr(),board.len(),4,0);
                assert!(!emu.is_null());
                let ssid=b"fixture,ssid=one"; let password=b"pass,word=two";
                assert_eq!(esp32sim_wifi_configure(emu,ssid.as_ptr(),ssid.len(),password.as_ptr(),password.len(),6),0);
                {
                    let ap=wifi_mac(&mut *emu).unwrap().ap.as_mut().unwrap();
                    assert_eq!(ap.cfg.ssid,"fixture,ssid=one");
                    assert_eq!(ap.cfg.psk.as_deref(),Some("pass,word=two"));
                }
                assert_eq!(esp32sim_wifi_state(emu),0);
                wifi_mac(&mut *emu).unwrap().ap.as_mut().unwrap().state=esp32s3::wifi::StaState::Authenticated;
                assert_eq!(esp32sim_wifi_state(emu),1);
                wifi_mac(&mut *emu).unwrap().ap.as_mut().unwrap().state=esp32s3::wifi::StaState::Associated;
                assert_eq!(esp32sim_wifi_state(emu),1);
                wifi_mac(&mut *emu).unwrap().ap.as_mut().unwrap().wpa.msg=4;
                assert_eq!(esp32sim_wifi_state(emu),2);
                assert_eq!(esp32sim_wifi_configure(emu,[0u8;33].as_ptr(),33,password.as_ptr(),password.len(),6),2);
                assert_eq!(esp32sim_wifi_configure(emu,ssid.as_ptr(),ssid.len(),password.as_ptr(),7,6),2);
                assert_eq!(esp32sim_wifi_configure(emu,ssid.as_ptr(),ssid.len(),std::ptr::null(),0,0),2);
                let key="01".repeat(32);
                assert_eq!(esp32sim_wifi_configure(emu,ssid.as_ptr(),ssid.len(),key.as_ptr(),64,6),0);
                assert_eq!(wifi_mac(&mut *emu).unwrap().ap.as_ref().unwrap().wpa.pmk,[1;32]);
                assert_eq!(esp32sim_wifi_configure(emu,ssid.as_ptr(),ssid.len(),std::ptr::null(),0,6),0);
                wifi_mac(&mut *emu).unwrap().ap.as_mut().unwrap().state=esp32s3::wifi::StaState::Associated;
                assert_eq!(esp32sim_wifi_state(emu),2);
                esp32sim_delete(emu);
            }
            assert_eq!(esp32sim_wifi_state(std::ptr::null_mut()),u32::MAX);
        }
    }
}

/// Attach host PCM to an ADC GPIO or an I2S RX port and its physical GPIO wiring.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_audio_configure(e: *mut Emu, id: u32, kind: u32, port: u32, data_gpio: u32, bclk_gpio: u32, ws_gpio: u32, sample_rate: u32, channels: u32) -> u32 {
    if e.is_null() || id >= 16 || kind > 1 { return 2; }
    let Some(input) = esp_periph::pcm::PcmInput::new(sample_rate, channels, [data_gpio, bclk_gpio, ws_gpio]) else { return 2; };
    let e = unsafe { &mut *e };
    if kind == 0 && (port != 0 || bclk_gpio != 0 || ws_gpio != 0) { return 2; }
    if kind == 1 && (data_gpio == bclk_gpio || data_gpio == ws_gpio || bclk_gpio == ws_gpio) { return 2; }
    if [data_gpio, bclk_gpio, ws_gpio].iter().any(|pin| e.m.gpio_state(*pin) == u32::MAX) { return 1; }
    let targets: Vec<_> = if kind == 1 && port == u32::MAX {
        (0..2).map(|port| (kind, port, id)).filter(|target| e.m.audio_input(*target).is_some()).collect()
    } else { vec![(kind, port, if kind == 0 { data_gpio } else { id })] };
    if targets.is_empty() || targets.iter().any(|target| e.m.audio_input(*target).is_none()) { return 1; }
    for (other, previous) in e.audio_targets.iter().enumerate() {
        if other == id as usize { continue; }
        for target in &targets {
            for old in previous {
                if old.0 == target.0 && old.1 == target.1 {
                    if let Some(Some(old_input)) = e.m.audio_input(*old) {
                        if old_input.pins == input.pins { return 2; }
                    }
                }
            }
        }
    }
    for old in &e.audio_targets[id as usize] {
        if let Some(input) = e.m.audio_input(*old) { *input = None; }
    }
    for target in &targets { *e.m.audio_input(*target).unwrap() = Some(input.clone()); }
    e.audio_targets[id as usize] = targets;
    0
}

#[no_mangle]
pub unsafe extern "C" fn esp32sim_audio_push(e: *mut Emu, id: u32, ptr: *const u8, len: usize) -> u32 {
    if e.is_null() || id >= 16 || len > 768000 || (len != 0 && ptr.is_null()) { return 2; }
    let e = unsafe { &mut *e };
    let targets = &e.audio_targets[id as usize];
    if targets.is_empty() { return 1; }
    let bytes = if len == 0 { &[] } else { unsafe { std::slice::from_raw_parts(ptr, len) } };
    for target in targets {
        let Some(Some(input)) = e.m.audio_input(*target) else { return 1; };
        if !input.push(bytes) { return 2; }
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn esp32sim_audio_reset(e: *mut Emu, id: u32) -> u32 {
    if e.is_null() || id >= 16 { return 2; }
    let e = unsafe { &mut *e };
    let targets = &e.audio_targets[id as usize];
    if targets.is_empty() { return 1; }
    for target in targets {
        let Some(Some(input)) = e.m.audio_input(*target) else { return 1; };
        input.reset();
    }
    0
}

/// 0 rate, 1 bits/sample, 2 channels, 3 running, 4 observed port or u32::MAX. I2S values come from guest registers.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_audio_info(e: *mut Emu, id: u32, field: u32) -> u32 {
    if e.is_null() || id >= 16 { return 0; }
    let e = unsafe { &mut *e };
    for target in &e.audio_targets[id as usize] {
        if e.m.audio_info(*target, 3) != 0 { return e.m.audio_info(*target, field); }
    }
    if field == 4 { u32::MAX } else { 0 }
}

#[cfg(test)]
mod audio_tests {
    use super::*;
    #[test]
    fn external_audio_survives_controller_reset_on_every_chip() {
        macro_rules! check { ($m:expr, $pin:expr) => {{
            let mut m = $m;
            for target in [(0, 0, $pin), (1, 0, 0)] {
                let input = MachineApi::audio_input(&mut m, target).unwrap();
                *input = esp_periph::pcm::PcmInput::new(8000, 1, [$pin, 2, 3]);
                assert!(input.as_mut().unwrap().push(&[0xd2, 4, 0xd7, 0xf6]));
                assert_eq!(input.as_mut().unwrap().advance(10000, 80_000_000), [1234; 2]);
            }
            esp_soc::SocBus::reboot(&mut m.bus, [0; 6]);
            for target in [(0, 0, $pin), (1, 0, 0)] {
                let input = MachineApi::audio_input(&mut m, target).unwrap().as_mut().unwrap();
                assert_eq!(input.advance(0, 80_000_000), [1234; 2]);
                assert_eq!(input.advance(10000, 80_000_000), [-2345; 2]);
            }
        }} }
        check!(esp32s3::machine([0; 6]), 1);
        check!(esp32c3::machine([0; 6], 4 << 20), 0);
        check!(esp32c6::machine([0; 6], 4 << 20), 0);
        unsafe {
            let e = esp32sim_new(b"esp32c3".as_ptr(), 7, 4, 0);
            assert_eq!(esp32sim_audio_configure(e, 0, 0, 0, 0, 0, 0, 16000, 1), 0);
            assert_eq!(esp32sim_audio_configure(e, 1, 0, 0, 0, 0, 0, 16000, 1), 2);
            assert_eq!(esp32sim_audio_configure(e, 1, 1, 1, 4, 5, 6, 16000, 1), 1);
            assert_eq!(esp32sim_audio_configure(e, 1, 1, 0, 4, 4, 6, 16000, 1), 2);
            assert_eq!(esp32sim_audio_configure(e, 16, 0, 0, 0, 0, 0, 16000, 1), 2);
            assert_eq!(esp32sim_audio_push(e, 0, [0u8].as_ptr(), 1), 2);
            assert_eq!(esp32sim_audio_reset(e, 0), 0);
            assert_eq!(esp32sim_audio_info(e, 0, 0), 16000);
            esp32sim_delete(e);
        }
    }
}

/// Configure an external 8N1 NMEA GPS on a physical MCU RX pin. Zero succeeds.
/// # Safety
/// A non-null emulator pointer must be live and exclusively borrowed.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_gps_configure(e:*mut Emu,id:u32,pin:u32,baud:u32)->u32 {
    if e.is_null(){return 1;} unsafe{&mut *e}.m.gps_configure(id,pin,baud)
}
/// Set/clear a GPS fix. Speed is metres/second, altitude metres, UTC milliseconds.
/// # Safety
/// A non-null emulator pointer must be live and exclusively borrowed.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_gps_fix(e:*mut Emu,id:u32,valid:u32,lat:f64,lng:f64,altitude:f64,speed:f64,unix_ms:f64)->u32 {
    if e.is_null() || valid>1{return 1;}
    unsafe{&mut *e}.m.gps_fix(id,if valid==1{Some(esp_soc::devices::gps::Fix{lat,lng,altitude,speed})}else{None},unix_ms)
}

#[cfg(test)]
mod gps_tests {
    use super::*;
    #[test]
    fn physical_rx_follows_chip_gpio_matrix_and_rejects_miswiring() {
        macro_rules! check {($machine:expr,$signal:expr,$selector:expr,$clock:expr)=>{{
            let mut m=$machine;
            m.bus.periph.gpio.func_in_sel[$signal]=$selector|4;
            m.bus.periph.uart[1].write(0x14,(5<<20)|2083);
            m.bus.periph.uart[1].write(0x78,(3<<20)|(1<<12));
            $clock(&mut m);
            m.bus.uart_pin_input(7,9600,b'x');
            assert_eq!(m.bus.periph.uart[1].rx_pending(),0);
            m.bus.uart_pin_input(4,9600,b'y');
            assert_eq!(m.bus.periph.uart[1].read(0),b'y' as u32);
            m.bus.periph.gpio.func_in_sel[$signal]=4;
            m.bus.uart_pin_input(4,9600,b'z');
            assert_eq!(m.bus.periph.uart[1].rx_pending(),0);
        }};}
        check!(esp32s3::machine([0;6]),15,0x80,|_: &mut esp32s3::Machine|{});
        check!(esp32c3::machine([0;6],4096),9,0x40,|_: &mut esp32c3::Machine|{});
        check!(esp32c6::machine([0;6],4096),9,0x80,|m: &mut esp32c6::Machine|{xtensa_lx7::Bus::write32(&mut m.bus,0x60096010,(3<<20)|(1<<12)).unwrap();});
    }
    #[test]
    fn gps_abi_validates_slots_pins_values_and_keeps_devices_on_reboot() {
        let mut m=esp32c3::machine([0;6],4096);
        assert_ne!(MachineApi::gps_configure(&mut m,4,4,9600),0);
        assert_ne!(MachineApi::gps_configure(&mut m,0,22,9600),0);
        assert_ne!(MachineApi::gps_configure(&mut m,0,4,0),0);
        assert_eq!(MachineApi::gps_configure(&mut m,0,4,9600),0);
        assert_eq!(MachineApi::gps_fix(&mut m,0,None,1704067200000.0),0);
        m.reboot();
        assert_eq!(MachineApi::gps_fix(&mut m,0,None,1704067200000.0),0);
    }
}

#[no_mangle]
pub unsafe extern "C" fn esp32sim_camera_configure(e: *mut Emu, ptr: *const u8, len: usize) -> u32 {
    if e.is_null() || ptr.is_null() || len != 20 { return 2; }
    let Some(config) = esp_soc::devices::camera::CameraConfig::parse(unsafe { std::slice::from_raw_parts(ptr,len) }) else { return 2; };
    let Some(machine) = (unsafe { &mut *e }).m.as_any_mut().downcast_mut::<esp32s3::Machine>() else { return 1; };
    if machine.bus.board.name() == "none" { machine.bus.board = Box::new(esp_soc::devices::CircuitBoard::new(&[],&[]).unwrap()); }
    if !machine.bus.board.configure_camera(config) { return 1; }
    for i2c in &mut machine.bus.periph.i2c { i2c.clear_devices(); }
    machine.bus.attach_board_devices();
    0
}
fn camera(e: &mut Emu, id: u32) -> Option<std::sync::Arc<std::sync::Mutex<esp_soc::devices::camera::Camera>>> {
    let machine = e.m.as_any_mut().downcast_mut::<esp32s3::Machine>()?;
    let camera = machine.bus.board.camera()?;
    let matches = camera.lock().unwrap().config.id as u32 == id;
    matches.then_some(camera)
}
#[no_mangle]
pub unsafe extern "C" fn esp32sim_camera_push(e: *mut Emu, id: u32, width: u32, height: u32, format: u32, ptr: *const u8, len: usize) -> u32 {
    if e.is_null() || ptr.is_null() || len > 5_760_000 { return 2; }
    let Some(camera) = camera(unsafe { &mut *e }, id) else { return 1; };
    let accepted = camera.lock().unwrap().push(width,height,format,unsafe { std::slice::from_raw_parts(ptr,len) });
    if accepted { 0 } else { 2 }
}
#[no_mangle]
pub unsafe extern "C" fn esp32sim_camera_reset(e: *mut Emu, id: u32) -> u32 {
    let Some(e) = (unsafe { e.as_mut() }) else { return 2; };
    let Some(camera) = camera(e,id) else { return 1; };
    camera.lock().unwrap().reset_frame();
    0
}
#[no_mangle]
pub unsafe extern "C" fn esp32sim_camera_info(e: *mut Emu, id: u32, field: u32) -> u32 {
    let Some(e) = (unsafe { e.as_mut() }) else { return 0; };
    let Some(camera) = camera(e,id) else { return 0; };
    let value = camera.lock().unwrap().info(field); value
}

#[cfg(test)]
mod sensor_tests {
    use super::*;
    #[test]
    fn sensor_configuration_validates_chip_pins_fields_and_keeps_camera_and_reboot_state() {
        macro_rules! check {
            ($machine:expr,$invalid:expr) => {{
                let mut m = $machine;
                let a = esp_soc::devices::SensorConfig {
                    id: 0,
                    sda: 4,
                    scl: 5,
                    address: 0x76,
                    model: 1,
                    shunt_milliohms: 0,
                };
                let b = esp_soc::devices::SensorConfig {
                    id: 1,
                    sda: 6,
                    scl: 7,
                    ..a
                };
                MachineApi::configure_circuit(&mut m, &[], &[], &[a, b]).unwrap();
                assert_eq!(MachineApi::sensor_set(&mut m, 0, 0, -10.), 0);
                assert_eq!(MachineApi::sensor_set(&mut m, 1, 0, 35.), 0);
                assert_eq!(MachineApi::sensor_set(&mut m, 2, 0, 35.), 1);
                assert_eq!(MachineApi::sensor_set(&mut m, 0, 3, 35.), 1);
                assert_eq!(MachineApi::sensor_set(&mut m, 0, 0, f64::INFINITY), 1);
                assert!(MachineApi::configure_circuit(
                    &mut m,
                    &[],
                    &[],
                    &[a, esp_soc::devices::SensorConfig { id: 1, ..a }]
                )
                .is_err());
                assert!(MachineApi::configure_circuit(
                    &mut m,
                    &[],
                    &[],
                    &[esp_soc::devices::SensorConfig { sda: $invalid, ..a }]
                )
                .is_err());
                m.bus
                    .board()
                    .configure_camera(esp_soc::devices::camera::CameraConfig {
                        sensor: 0x26,
                        id: 0,
                        pins: std::array::from_fn(|i| i as u8),
                        fps: 10,
                    });
                let camera = m.bus.board_ref().camera().unwrap();
                MachineApi::configure_circuit(&mut m, &[], &[], &[a, b]).unwrap();
                assert!(std::sync::Arc::ptr_eq(
                    &camera,
                    &m.bus.board_ref().camera().unwrap()
                ));
                m.bus.reboot([0; 6]);
                assert_eq!(MachineApi::sensor_set(&mut m, 1, 0, 31.), 0);
                let motion = esp_soc::devices::SensorConfig {
                    model: 4,
                    address: 0x68,
                    ..a
                };
                let power = esp_soc::devices::SensorConfig {
                    id: 2,
                    model: 5,
                    address: 0x40,
                    shunt_milliohms: 100,
                    ..a
                };
                let rtc = esp_soc::devices::SensorConfig {
                    model: 6,
                    address: 0x68,
                    ..b
                };
                MachineApi::configure_circuit(&mut m, &[], &[], &[motion, power, rtc]).unwrap();
                assert_eq!(MachineApi::sensor_set(&mut m, 0, 4, 9.80665), 0);
                assert_eq!(MachineApi::sensor_set(&mut m, 0, 4, 157.), 1);
                assert_eq!(MachineApi::sensor_set(&mut m, 2, 12, 3200.), 0);
                assert_eq!(MachineApi::sensor_set(&mut m, 2, 15, 101.), 1);
                assert_eq!(MachineApi::sensor_set(&mut m, 1, 14, 1704067200.), 0);
                assert_eq!(MachineApi::sensor_set(&mut m, 1, 14, 1704067200.5), 1);
                assert!(MachineApi::configure_circuit(
                    &mut m,
                    &[],
                    &[],
                    &[esp_soc::devices::SensorConfig {
                        shunt_milliohms: 1,
                        ..motion
                    }]
                )
                .is_err());
            }};
        }
        check!(esp32s3::machine([0; 6]), 49);
        check!(esp32c3::machine([0; 6], 4 << 20), 22);
        check!(esp32c6::machine([0; 6], 4 << 20), 31);
    }
}

#[no_mangle]
pub unsafe extern "C" fn esp32sim_servo_configure(e: *mut Emu, id: u32, pin: u32, min_us: u32, max_us: u32, min_angle: i32, max_angle: i32) -> u32 {
    let Some(e) = (unsafe {e.as_mut()}) else {return 2;};
    let Some(servo) = esp_soc::devices::servo::Servo::new(pin,min_us,max_us,min_angle,max_angle) else {return 2;};
    if id >= 16 {return 2;}
    if e.m.gpio_state(pin) == u32::MAX {return 1;}
    if let Some(previous) = e.servos[id as usize].as_mut().filter(|previous| previous.source == esp_soc::devices::servo::Source::Gpio(pin)) { previous.configure(servo); }
    else { e.servos[id as usize] = Some(servo); }
    0
}
#[no_mangle]
pub unsafe extern "C" fn esp32sim_servo_info(e: *mut Emu, id: u32, field: u32) -> i32 {
    let Some(e) = (unsafe {e.as_mut()}) else {return 0;};
    let Some(Some(servo)) = e.servos.get_mut(id as usize) else {return 0;};
    let output=match servo.source {esp_soc::devices::servo::Source::Gpio(pin)=>e.m.pwm_output(pin),esp_soc::devices::servo::Source::Pca9685{device,channel}=>e.m.pwm_expander_output(device,channel)};
    servo.observe(output,field)
}

#[cfg(test)]
mod servo_tests {
    use super::*;
    #[test]
    fn servo_configuration_is_bounded_and_failed_updates_preserve_position() {
        unsafe {
            for board in ["none","esp32c3","esp32c6"] {
                let e=esp32sim_new(board.as_ptr(),board.len(),4,0);
                assert!(!e.is_null());
                assert_eq!(esp32sim_servo_configure(e,0,4,1000,2000,-90_000,90_000),0);
                let servo=(*e).servos[0].as_mut().unwrap();
                servo.observe(Some((50.0,6554)),2);
                assert_eq!(esp32sim_servo_info(e,0,2),90_000);
                assert_eq!(esp32sim_servo_configure(e,0,4,2000,1000,0,180_000),2);
                assert_eq!(esp32sim_servo_configure(e,0,100,1000,2000,0,180_000),1);
                assert_eq!(esp32sim_servo_configure(e,16,4,1000,2000,0,180_000),2);
                assert_eq!(esp32sim_servo_info(e,0,2),90_000);
                assert_eq!(esp32sim_servo_info(e,0,0),0);
                assert_eq!(esp32sim_servo_configure(e,0,4,1000,2000,0,180_000),0);
                assert_eq!(esp32sim_servo_info(e,0,2),90_000);
                assert_eq!(esp32sim_servo_configure(e,1,4,1000,2000,-90_000,90_000),0);
                assert_eq!(esp32sim_servo_info(e,1,2),0);
                esp32sim_delete(e);
            }
            assert_eq!(esp32sim_servo_configure(std::ptr::null_mut(),0,4,1000,2000,0,180_000),2);
        }
    }
}

/// Configure SPI displays before boot with 24-byte records: id, model, SCLK,
/// MOSI, CS, DC, RESET, flags (BGR=1, inverted=2), then LE u16 width/height/x/y, backlight GPIO, active-low flag, two reserved zeros, then LE u16 address width/height (both zero select controller defaults).
/// Absent CS or RESET uses 255. Returns 1 for invalid wiring or lifecycle.
/// # Safety
/// Non-null pointers must cover an exclusively borrowed emulator and `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_spi_displays(e:*mut Emu,data:*const u8,len:usize)->u32 {
    if e.is_null() || len>16*24 || len%24!=0 || (len!=0 && data.is_null()) { return 1; }
    let e=unsafe{&mut *e};
    if e.booted { return 1; }
    let bytes=if len==0 { &[] } else { unsafe{std::slice::from_raw_parts(data,len)} };
    let mut configs=Vec::new();
    for r in bytes.chunks_exact(24) {
        if r[7]&!3!=0 || r[17]>1 || r[18]!=0 || r[19]!=0 { return 1; }
        let word=|i|u16::from_le_bytes([r[i],r[i+1]]);
        configs.push(esp_soc::devices::spi_display::SpiDisplayConfig { id:r[0],model:r[1],sclk:r[2],mosi:r[3],cs:(r[4]!=255).then_some(r[4]),dc:r[5],reset:(r[6]!=255).then_some(r[6]),bgr:r[7]&1!=0,inverted:r[7]&2!=0,width:word(8),height:word(10),x:word(12),y:word(14),backlight:(r[16]!=255).then_some(r[16]),backlight_active_low:r[17]!=0,address_width:word(20),address_height:word(22) });
    }
    match e.m.configure_spi_displays(&configs) { Ok(())=>0,Err(message)=>{log(&message);1} }
}

#[no_mangle]
pub unsafe extern "C" fn esp32sim_configure_touches(e:*mut Emu,ptr:*const u8,len:usize)->u32 {
    if e.is_null() || (ptr.is_null() && len!=0) || len>16*12 || len%12!=0 {return 2;}
    let bytes=if len==0 {&[]}else{unsafe{std::slice::from_raw_parts(ptr,len)}};
    let Some(configs)=bytes.chunks_exact(12).map(esp_soc::devices::touch::TouchConfig::parse).collect::<Option<Vec<_>>>() else {return 2;};
    match unsafe{&mut *e}.m.configure_touches(&configs) {Ok(())=>0,Err(message)=>{log(&message);1}}
}
#[no_mangle]
pub unsafe extern "C" fn esp32sim_touch_input(e:*mut Emu,id:u32,x:u32,y:u32,down:u32)->u32 {
    let Some(e)=(unsafe{e.as_mut()}) else{return 2;};
    if x>4095 || y>4095 || down>1 {return 2;}
    e.m.touch_input(id,Some((x as u16,y as u16,down!=0)))
}
#[no_mangle]
pub unsafe extern "C" fn esp32sim_touch_reset(e:*mut Emu,id:u32)->u32 {
    let Some(e)=(unsafe{e.as_mut()}) else{return 2;};e.m.touch_input(id,None)
}

#[no_mangle]
pub unsafe extern "C" fn esp32sim_touch_report_hz(e:*mut Emu,id:u32,hz:u32)->u32 {
    let Some(e)=(unsafe{e.as_mut()}) else{return 1;};e.m.touch_rate(id,hz)
}

#[cfg(test)]
mod touch_tests {
    use super::*;
    #[test]
    fn touch_setup_preserves_other_devices_and_reboot_state_and_releases_removed_irq() {
        macro_rules! check {
            ($machine:expr,$invalid:expr)=>{{
                let mut m=$machine;
                let sensor=esp_soc::devices::SensorConfig{id:0,sda:4,scl:5,address:0x76,model:1,shunt_milliohms:0};
                MachineApi::configure_circuit(&mut m,&[],&[],&[sensor]).unwrap();
                m.bus.board().configure_camera(esp_soc::devices::camera::CameraConfig{sensor:0x26,id:0,pins:std::array::from_fn(|i|i as u8),fps:10});
                let camera=m.bus.board_ref().camera().unwrap();
                let c=esp_soc::devices::touch::TouchConfig{model:5,id:0,sda:4,scl:5,address:0x63,irq:6,reset:7,report_hz:20,width:320,height:240};
                MachineApi::configure_touches(&mut m,&[c]).unwrap();
                assert!(std::sync::Arc::ptr_eq(&camera,&m.bus.board_ref().camera().unwrap()));
                assert_eq!(MachineApi::sensor_set(&mut m,0,0,21.),0);
                assert!(MachineApi::configure_touches(&mut m,&[c,esp_soc::devices::touch::TouchConfig{id:1,..c}]).is_err());
                assert!(MachineApi::configure_touches(&mut m,&[esp_soc::devices::touch::TouchConfig{sda:$invalid,..c}]).is_err());
                assert_eq!(MachineApi::touch_input(&mut m,0,Some((20,30,true))),0);
                assert_eq!(m.bus.gpio_input()&(1<<6),0);
                let device=m.bus.board().touch_device(0).unwrap();
                m.bus.reboot([0;6]);
                assert!(std::sync::Arc::ptr_eq(&device,&m.bus.board().touch_device(0).unwrap()));
                assert_eq!(device.lock().unwrap().config.report_hz,20);
                MachineApi::configure_touches(&mut m,&[]).unwrap();
                assert_ne!(m.bus.gpio_input()&(1<<6),0);
            }};
        }
        check!(esp32s3::machine([0;6]),49);
        check!(esp32c3::machine([0;6],4<<20),22);
        check!(esp32c6::machine([0;6],4<<20),31);
    }
}

#[cfg(test)]
mod physical_input_tests {
    use super::*;
    use xtensa_lx7::Bus;
    use esp_soc::devices::inputs::InputConfig;
    #[test]
    fn input_abi_rejects_malformed_and_duplicate_records_before_mutation() {
        unsafe {
            let e=esp32sim_new(b"esp32c3".as_ptr(),7,4,0);assert!(!e.is_null());
            let mut record=[255u8;20];record[..5].copy_from_slice(&[1,0,1,1,4]);record[12]=5;
            assert_eq!(esp32sim_configure_inputs(e,record.as_ptr(),19),1);
            record[5]=6;assert_eq!(esp32sim_configure_inputs(e,record.as_ptr(),20),1);record[5]=255;
            assert_eq!(esp32sim_configure_inputs(e,record.as_ptr(),20),0);
            let duplicate=[record,record].concat();assert_eq!(esp32sim_configure_inputs(e,duplicate.as_ptr(),40),1);
            assert_eq!(esp32sim_distance_mm(e,0,4000),0);assert_eq!(esp32sim_distance_mm(e,0,4001),1);
            assert_eq!(esp32sim_distance_mm(e,1,100),1);
            assert_eq!(esp32sim_keypad_press(e,0,0,0),1);
            (*e).booted=true;assert_eq!(esp32sim_configure_inputs(e,record.as_ptr(),20),1);
            esp32sim_delete(e);
        }
    }
    #[test]
    fn project_input_waves_follow_actual_gpio_drive_and_chip_clock() {
        macro_rules! check {($machine:expr,$gpio:expr,$mux:expr,$invalid:expr)=>{{
            let mut m=$machine;
            MachineApi::configure_circuit(&mut m,&[],&[],&[]).unwrap();
            let configs=[InputConfig::Keypad{id:0,rows:vec![1],columns:vec![3]},InputConfig::Encoder{id:1,a:5,b:6},InputConfig::Ultrasonic{id:2,trigger:8,echo:9}];
            MachineApi::configure_inputs(&mut m,&configs).unwrap();
            assert!(MachineApi::configure_inputs(&mut m,&[InputConfig::Encoder{id:0,a:$invalid,b:0}]).is_err());
            m.bus.write32($mux+4+4,1<<12|1<<9|1<<8).unwrap();
            m.bus.write32($gpio+0x24,1<<3).unwrap();
            assert_eq!(MachineApi::input_action(&mut m,2,0,0,0),0);
            assert_eq!(m.bus.gpio_input()&(1<<1),0);
            m.bus.write32($gpio+0x28,1<<3).unwrap();
            assert_ne!(m.bus.gpio_input()&(1<<1),0,"floating row uses its physical pullup");
            assert_eq!(MachineApi::input_action(&mut m,2,0,1,0),1);
            assert_eq!(MachineApi::input_action(&mut m,3,1,1025,0),1);
            assert_eq!(MachineApi::input_action(&mut m,1,2,1000,0),0);
            let hz=MachineApi::cpu_hz(&m) as u32;
            m.bus.write32($gpio+0x24,1<<8).unwrap();m.bus.write32($gpio+8,1<<8).unwrap();
            m.bus.tick(hz/100_000);m.bus.flush_ticks();
            m.bus.write32($gpio+0xc,1<<8).unwrap();
            m.bus.tick(hz/10_000);m.bus.flush_ticks();
            assert_ne!(m.bus.gpio_input()&(1<<9),0,"echo rises after the trigger");
            m.bus.tick((hz as u64*5800/1_000_000) as u32);m.bus.flush_ticks();
            assert_eq!(m.bus.gpio_input()&(1<<9),0,"echo is exactly58us/cm");
            assert_eq!(MachineApi::input_action(&mut m,3,1,1,0),0);
            m.bus.tick(hz/500);m.bus.flush_ticks();
            assert_eq!(m.bus.gpio_input()&(1<<5),0);assert_ne!(m.bus.gpio_input()&(1<<6),0);
        }}}
        check!(esp32s3::machine([0;6]),0x60004000,0x60009000,22);
        check!(esp32c3::machine([0;6],4<<20),0x60004000,0x60009000,22);
        check!(esp32c6::machine([0;6],4<<20),0x60091000,0x60090000,31);
    }
}

#[no_mangle]
pub unsafe extern "C" fn esp32sim_configure_resistive_touches(e:*mut Emu,ptr:*const u8,len:usize)->u32 {
    if e.is_null() || (ptr.is_null() && len!=0) || len>16*24 || len%24!=0 {return 2;}
    let bytes=if len==0 {&[]}else{unsafe{std::slice::from_raw_parts(ptr,len)}};
    let Some(configs)=bytes.chunks_exact(24).map(esp_soc::devices::resistive_touch::ResistiveConfig::parse).collect::<Option<Vec<_>>>() else {return 2;};
    match unsafe{&mut *e}.m.configure_resistive_touches(&configs) {Ok(())=>0,Err(message)=>{log(&message);1}}
}
#[no_mangle]
pub unsafe extern "C" fn esp32sim_touch_calibrate(e:*mut Emu,id:u32,field:u32,value:u32)->u32 {
    let Some(e)=(unsafe{e.as_mut()}) else{return 2;};e.m.touch_calibrate(id,field,value)
}

#[cfg(test)]
mod resistive_tests {
    use super::*;
    #[test]
    fn resistive_bounds_identity_and_configuration_preserve_capacitive_devices() {
        macro_rules! check {
            ($machine:expr,$invalid:expr)=>{{
                let mut m=$machine;
                MachineApi::configure_circuit(&mut m,&[],&[],&[]).unwrap();
                let cap=esp_soc::devices::touch::TouchConfig{model:5,id:1,sda:9,scl:10,address:0x63,irq:11,reset:12,report_hz:20,width:320,height:240};
                MachineApi::configure_touches(&mut m,&[cap]).unwrap();
                let original=m.bus.board().touch_device(1).unwrap();
                let c=esp_soc::devices::resistive_touch::ResistiveConfig{model:1,id:0,pins:[4,5,6,7],irq:8,width:320,height:240,calibration:[0,4095,0,4095,1000,1800]};
                MachineApi::configure_resistive_touches(&mut m,&[c]).unwrap();
                assert!(std::sync::Arc::ptr_eq(&original,&m.bus.board().touch_device(1).unwrap()));
                assert!(MachineApi::configure_resistive_touches(&mut m,&[esp_soc::devices::resistive_touch::ResistiveConfig{id:1,..c}]).is_err());
                assert!(MachineApi::configure_resistive_touches(&mut m,&[esp_soc::devices::resistive_touch::ResistiveConfig{pins:[$invalid,5,6,7],..c}]).is_err());
                assert!(MachineApi::configure_touches(&mut m,&[esp_soc::devices::touch::TouchConfig{id:0,..cap}]).is_err());
                assert_eq!(MachineApi::touch_calibrate(&mut m,0,0,100),0);
                assert_eq!(MachineApi::touch_calibrate(&mut m,0,1,100),2);
                assert_eq!(MachineApi::touch_calibrate(&mut m,0,4,4096),2);
                assert_eq!(MachineApi::touch_calibrate(&mut m,1,4,100),1);
                assert_eq!(MachineApi::touch_input(&mut m,0,Some((20,30,true))),0);
                assert_eq!(MachineApi::touch_input(&mut m,1,Some((40,50,true))),0);
                assert_eq!(m.bus.gpio_input()&((1<<8)|(1<<11)),0);
                MachineApi::configure_resistive_touches(&mut m,&[]).unwrap();
                assert_ne!(m.bus.gpio_input()&(1<<8),0);
                assert_eq!(m.bus.gpio_input()&(1<<11),0);
            }};
        }
        check!(esp32s3::machine([0;6]),49);
        check!(esp32c3::machine([0;6],4<<20),22);
        check!(esp32c6::machine([0;6],4<<20),31);
    }
}

/// Configure at most 16 GPIO sensors before boot. Twelve-byte records:
/// [model, id, pin, 0, ROM x8], models 1 DHT11, 2 DHT22, 3 DS18B20.
/// DS ROMs require family 0x28 and valid Dallas CRC; DHT ROM bytes must be zero.
/// # Safety
/// Live exclusively borrowed `e`; `data` readable for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_configure_pin_sensors(e:*mut Emu,data:*const u8,len:usize)->u32 {
    if e.is_null()||len>16*12||len%12!=0||(len>0&&data.is_null()){return 1;}
    let e=unsafe{&mut *e};if e.booted{return 1;}
    let bytes=if len==0{&[]}else{unsafe{std::slice::from_raw_parts(data,len)}};
    let mut configs=Vec::new();
    for r in bytes.chunks_exact(12){if r[3]!=0{return 1;}configs.push(esp_soc::devices::pin_sensor::Config{model:r[0],id:r[1],pin:r[2],rom:r[4..12].try_into().unwrap()});}
    u32::from(e.m.configure_pin_sensors(&configs).is_err())
}
/// Physical input field 0 Celsius or 1 humidity percent. Returns 1 on invalid input.
/// # Safety
/// Non-null `e` must be live and exclusively borrowed.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_pin_sensor_set(e:*mut Emu,id:u32,field:u32,value:f64)->u32 {
    if e.is_null(){return 1;}unsafe{&mut *e}.m.pin_sensor_set(id,field,value)
}
/// Completed wire measurement count, MAX for an invalid identity.
/// # Safety
/// Non-null `e` must be live and exclusively borrowed.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_pin_sensor_generation(e:*mut Emu,id:u32)->u32 {
    if e.is_null(){return u32::MAX;}unsafe{&mut *e}.m.pin_sensor_generation(id)
}
/// Last completed physical sample, NaN before measurement or for invalid input.
/// # Safety
/// Non-null `e` must be live and exclusively borrowed.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_pin_sensor_value(e:*mut Emu,id:u32,field:u32)->f64 {
    if e.is_null(){return f64::NAN;}unsafe{&mut *e}.m.pin_sensor_value(id,field)
}

#[cfg(test)]
mod pin_sensor_tests {
    use super::*;
    use xtensa_lx7::Bus;
    #[test]
    fn chip_gpio_registers_observe_open_drain_response_and_released_master_low() {
        macro_rules! check {($machine:expr,$gpio:expr)=>{{
            let mut m=$machine;
            MachineApi::configure_circuit(&mut m,&[],&[],&[]).unwrap();
            MachineApi::configure_pin_sensors(&mut m,&[esp_soc::devices::pin_sensor::Config{model:2,id:0,pin:4,rom:[0;8]}]).unwrap();
            let hz=MachineApi::cpu_hz(&m) as u32;
            m.bus.write32($gpio+0x24,16).unwrap();
            m.bus.tick(hz/500);
            m.bus.write32($gpio+0x28,16).unwrap();
            assert_ne!(m.bus.read32($gpio+0x3c).unwrap()&16,0);
            m.bus.tick((hz/1_000_000)*30);
            assert_eq!(m.bus.read32($gpio+0x3c).unwrap()&16,0);
            m.bus.tick((hz/1_000_000)*80);
            assert_ne!(m.bus.read32($gpio+0x3c).unwrap()&16,0);
            m.bus.tick(hz/100);
            m.bus.write32($gpio+0x24,16).unwrap();
            assert_eq!(m.bus.read32($gpio+0x3c).unwrap()&16,0);
            assert_eq!(MachineApi::pin_sensor_generation(&m,0),1);
            assert_eq!(MachineApi::pin_sensor_value(&m,0,0),25.);
        }};}
        check!(esp32s3::machine([0;6]),0x60004000);
        check!(esp32c3::machine([0;6],4<<20),0x60004000);
        check!(esp32c6::machine([0;6],4<<20),0x60091000);
    }
}

#[cfg(test)]
mod rfid_tests {
    use super::*;
    #[test]
    fn rfid_configuration_and_card_input_enforce_physical_and_buffer_bounds() {
        unsafe {
            for chip in ["none","esp32c3","esp32c6"] {
                let e=esp32sim_new(chip.as_ptr(),chip.len(),4,0);assert!(!e.is_null());
                let record=[1,0,1,2,3,4,5,0];
                assert_eq!(esp32sim_configure_rfid(e,record.as_ptr(),7),1);
                assert_eq!(esp32sim_configure_rfid(e,record.as_ptr(),8),0);
                let mut second=record;second[1]=1;let collision=[record,second].concat();
                assert_eq!(esp32sim_configure_rfid(e,collision.as_ptr(),collision.len()),1);
                second[5]=49;assert_eq!(esp32sim_configure_rfid(e,second.as_ptr(),8),1);
                assert_eq!(esp32sim_rfid_card(e,0,b"1234".as_ptr(),4),0);
                assert_eq!(esp32sim_rfid_card(e,0,b"1234".as_ptr(),3),1);
                assert_eq!(esp32sim_rfid_card(e,1,b"1234".as_ptr(),4),1);
                assert_eq!(esp32sim_rfid_card(e,0,std::ptr::null(),0),0);
                (*e).booted=true;assert_eq!(esp32sim_configure_rfid(e,record.as_ptr(),8),1);
                esp32sim_delete(e);
            }
        }
    }
}

#[cfg(test)]
mod input_reboot_tests {
    use super::*;
    use xtensa_lx7::Bus;
    #[test]
    fn chip_reset_releases_keypad_master_drive_before_any_new_gpio_write() {
        macro_rules! check {($machine:expr,$gpio:expr)=>{{
            let mut m=$machine;
            MachineApi::configure_circuit(&mut m,&[],&[],&[]).unwrap();
            MachineApi::configure_inputs(&mut m,&[esp_soc::devices::inputs::InputConfig::Keypad{id:0,rows:vec![1],columns:vec![3]}]).unwrap();
            m.bus.write32($gpio+0x24,1<<3).unwrap();
            assert_eq!(MachineApi::input_action(&mut m,2,0,0,0),0);
            assert_eq!(m.bus.gpio_input()&(1<<1),0);
            let cycle=m.bus.cycles();m.reboot();
            assert_eq!(m.bus.cycles(),cycle);
            assert_ne!(m.bus.gpio_input()&(1<<1),0,"released MCU column must no longer pull the pressed row LOW");
            m.bus.write32($gpio+0x24,1<<3).unwrap();
            assert_eq!(m.bus.gpio_input()&(1<<1),0,"physical key remains pressed across MCU reset");
        }};}
        check!(esp32s3::machine([0;6]),0x60004000);
        check!(esp32c3::machine([0;6],4<<20),0x60004000);
        check!(esp32c6::machine([0;6],4<<20),0x60091000);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn esp32sim_configure_gestures(e:*mut Emu,data:*const u8,len:usize)->u32{
    if e.is_null()||len>128||len%8!=0||(len>0&&data.is_null()){return 1;}
    let e=unsafe{&mut *e};if e.booted{return 1;}
    let bytes=if len==0{&[][..]}else{unsafe{std::slice::from_raw_parts(data,len)}};
    let mut configs=Vec::new();
    for r in bytes.chunks_exact(8){if r[0]!=1||r[5..].iter().any(|v|*v!=0){return 1;}configs.push(esp_soc::devices::gesture::GestureConfig{id:r[1],sda:r[2],scl:r[3],irq:r[4]});}
    match e.m.configure_gestures(&configs){Ok(())=>0,Err(error)=>{log(&error);1}}
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn esp32sim_gesture(e:*mut Emu,id:u32,direction:u32)->u32{
    if e.is_null(){return 1;}unsafe{&mut *e}.m.gesture(id,direction)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn esp32sim_proximity(e:*mut Emu,id:u32,value:f64)->u32{
    if e.is_null(){return 1;}unsafe{&mut *e}.m.proximity(id,value)
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn esp32sim_proximity_reading(e:*const Emu,id:u32)->u32{
    if e.is_null(){return u32::MAX;}unsafe{&*e}.m.proximity_reading(id)
}
#[cfg(test)]
mod gesture_tests {
    use super::*;
    #[test]
    fn gesture_configuration_validates_routes_and_bounded_commands_on_all_chips() {
        unsafe {
            for chip in ["none","esp32c3","esp32c6"] {
                let e=esp32sim_new(chip.as_ptr(),chip.len(),4,0);
                let record=[1,0,4,5,6,0,0,0];
                assert_eq!(esp32sim_configure_gestures(e,record.as_ptr(),7),1);
                assert_eq!(esp32sim_configure_gestures(e,record.as_ptr(),8),0);
                let mut other=record;other[1]=1;
                let duplicate=[record,other].concat();
                assert_eq!(esp32sim_configure_gestures(e,duplicate.as_ptr(),16),1);
                other[2]=49;assert_eq!(esp32sim_configure_gestures(e,other.as_ptr(),8),1);
                assert_eq!(esp32sim_gesture(e,0,1),0);
                assert_eq!(esp32sim_proximity(e,0,80.),0);
                assert_eq!(esp32sim_proximity(e,1,80.),1);
                assert_eq!(esp32sim_proximity(e,0,f64::NAN),1);
                assert_eq!(esp32sim_proximity(e,0,256.),1);
                assert_eq!(esp32sim_gesture(e,1,1),1);
                assert_eq!(esp32sim_gesture(e,0,5),1);
                (*e).booted=true;
                assert_eq!(esp32sim_configure_gestures(e,record.as_ptr(),8),1);
                esp32sim_delete(e);
            }
        }
    }
}

/// Character LCD records: id, SDA, SCL, address, columns, rows, reserved, reserved.
/// # Safety
/// Pointers must cover an exclusively borrowed emulator and `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_lcds(e:*mut Emu,ptr:*const u8,len:usize)->u32 {
    if e.is_null() || (ptr.is_null()&&len!=0) || len>16*8 || len%8!=0{return 1;}
    let e=unsafe{&mut *e};if e.booted{return 1;}
    let bytes=if len==0{&[]}else{unsafe{std::slice::from_raw_parts(ptr,len)}};
    let mut configs=Vec::new();
    for r in bytes.chunks_exact(8){if r[6]!=0||r[7]!=0{return 1;}configs.push(esp_soc::devices::lcd::LcdConfig{id:r[0],sda:r[1],scl:r[2],address:r[3],columns:r[4],rows:r[5]});}
    match e.m.configure_lcds(&configs){Ok(())=>0,Err(message)=>{log(&message);1}}
}

/// LED display records: id, controller, layout, pin A, pin B, address, digits, flags (TM1637 center colon = 1).
/// # Safety
/// Pointers must cover an exclusively borrowed emulator and `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_led_displays(e:*mut Emu,ptr:*const u8,len:usize)->u32 {
    if e.is_null() || (ptr.is_null() && len!=0) || len>16*8 || len%8!=0 {return 1;}
    let e=unsafe{&mut *e};if e.booted{return 1;}
    let bytes=if len==0 {&[]}else{unsafe{std::slice::from_raw_parts(ptr,len)}};
    let mut configs=Vec::new();
    for r in bytes.chunks_exact(8) {
        if r[7]>1{return 1;}
        configs.push(esp_soc::devices::led_display::LedDisplayConfig{id:r[0],controller:r[1],layout:r[2],a:r[3],b:r[4],address:r[5],digits:r[6],colon:r[7]!=0});
    }
    match e.m.configure_led_displays(&configs){Ok(())=>0,Err(message)=>{log(&message);1}}
}

#[cfg(test)]
mod led_display_tests {
    use super::*;
    #[test]
    fn led_display_abi_is_bounded_and_only_configures_before_boot() {
        unsafe {
            for chip in ["none","esp32c3","esp32c6"] {
                let e=esp32sim_new(chip.as_ptr(),chip.len(),4,0);assert!(!e.is_null());
                let record=[0,1,3,4,5,0x70,4,0];
                assert_eq!(esp32sim_led_displays(e,record.as_ptr(),7),1);
                assert_eq!(esp32sim_led_displays(e,record.as_ptr(),8),0);
                let bad=[0,1,3,60,5,0x70,4,0];assert_eq!(esp32sim_led_displays(e,bad.as_ptr(),8),1);
                let duplicate=[record,record].concat();assert_eq!(esp32sim_led_displays(e,duplicate.as_ptr(),16),1);
                let invalid=[0,2,1,4,5,0,255,0];assert_eq!(esp32sim_led_displays(e,invalid.as_ptr(),8),1);
                (*e).booted=true;assert_eq!(esp32sim_led_displays(e,record.as_ptr(),8),1);
                esp32sim_delete(e);
            }
        }
    }
}

#[cfg(test)]
mod spi_geometry_tests {
    use super::*;
    #[test]
    fn geometry_records_validate_lengths_pairs_and_visible_bounds_on_all_chips() {
        unsafe {for board in ["none","esp32c3","esp32c6"] {
            let e=esp32sim_new(board.as_ptr(),board.len(),4,0);
            assert_eq!(esp32sim_configure_circuit(e,std::ptr::null(),0),0);
            let mut record=[0u8;24];record[..8].copy_from_slice(&[0,3,4,5,6,7,255,0]);record[16]=255;
            for (offset,value) in [(8,128u16),(10,160),(20,128),(22,160)] {record[offset..offset+2].copy_from_slice(&value.to_le_bytes());}
            assert_eq!(esp32sim_spi_displays(e,record.as_ptr(),24),0);
            assert_eq!(esp32sim_spi_displays(e,record.as_ptr(),20),1);
            record[22]=0;assert_eq!(esp32sim_spi_displays(e,record.as_ptr(),24),1);record[22]=160;
            record[12]=2;assert_eq!(esp32sim_spi_displays(e,record.as_ptr(),24),1);record[12]=0;
            record[20]=133;assert_eq!(esp32sim_spi_displays(e,record.as_ptr(),24),1);record[20]=0;record[22]=0;
            assert_eq!(esp32sim_spi_displays(e,record.as_ptr(),24),0);
            (*e).booted=true;assert_eq!(esp32sim_spi_displays(e,record.as_ptr(),24),1);
            esp32sim_delete(e);
        }}
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn esp32sim_configure_load_cells(e:*mut Emu,data:*const u8,len:usize)->u32 {
    if e.is_null()||len>512||len%32!=0||(len>0&&data.is_null()){return 1;}
    let e=unsafe{&mut *e};if e.booted{return 1;}
    let bytes=if len==0{&[][..]}else{unsafe{std::slice::from_raw_parts(data,len)}};
    let mut configs=Vec::new();
    for r in bytes.chunks_exact(32){
        if r[0]!=1||r[5..8].iter().chain(r[28..32].iter()).any(|v|*v!=0){return 1;}
        configs.push(esp_soc::devices::hx711::LoadCellConfig{id:r[1],dout:r[2],sck:r[3],rate:r[4],capacity:f64::from_le_bytes(r[8..16].try_into().unwrap()),sensitivity:f64::from_le_bytes(r[16..24].try_into().unwrap()),offset:i32::from_le_bytes(r[24..28].try_into().unwrap())});
    }
    match e.m.configure_load_cells(&configs){Ok(())=>0,Err(error)=>{log(&error);1}}
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn esp32sim_load_cell_weight(e:*mut Emu,id:u32,value:f64)->u32 {if e.is_null(){return 1;}unsafe{&mut *e}.m.load_cell_weight(id,value)}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn esp32sim_load_cell_calibrate(e:*mut Emu,id:u32,capacity:f64,sensitivity:f64,offset:i32)->u32 {if e.is_null(){return 1;}unsafe{&mut *e}.m.load_cell_calibrate(id,capacity,sensitivity,offset)}

#[cfg(test)]
mod load_cell_tests {
    use super::*;
    #[test]
    fn load_cell_abi_bounds_calibration_and_preserves_independent_physical_instances() {
        unsafe {for chip in ["none","esp32c3","esp32c6"] {
            let e=esp32sim_new(chip.as_ptr(),chip.len(),4,0);assert!(!e.is_null());
            let mut record=[0u8;32];record[..5].copy_from_slice(&[1,0,4,5,10]);
            record[8..16].copy_from_slice(&5000f64.to_le_bytes());record[16..24].copy_from_slice(&2f64.to_le_bytes());
            assert_eq!(esp32sim_configure_load_cells(e,record.as_ptr(),31),1);
            assert_eq!(esp32sim_configure_load_cells(e,record.as_ptr(),32),0);
            let mut second=record;second[1]=1;second[2]=6;
            let both=[record,second].concat();assert_eq!(esp32sim_configure_load_cells(e,both.as_ptr(),64),0);
            assert_eq!(esp32sim_load_cell_weight(e,0,500.),0);assert_eq!(esp32sim_load_cell_weight(e,1,-100.),0);
            assert_eq!(esp32sim_load_cell_weight(e,2,1.),1);assert_eq!(esp32sim_load_cell_weight(e,0,f64::NAN),1);
            assert_eq!(esp32sim_load_cell_calibrate(e,0,0.,2.,0),1);assert_eq!(esp32sim_load_cell_calibrate(e,1,5000.,2.,123),0);
            second[2]=49;assert_eq!(esp32sim_configure_load_cells(e,second.as_ptr(),32),1);
            (*e).booted=true;assert_eq!(esp32sim_configure_load_cells(e,record.as_ptr(),32),1);
            esp32sim_delete(e);
        }}
    }
}

#[no_mangle]
pub unsafe extern "C" fn esp32sim_radar_configure(e: *mut Emu, id: u32, tx: u32, rx: u32) -> u32 {
    if e.is_null() {
        return 1;
    }
    unsafe { &mut *e }.m.radar_configure(id, tx, rx)
}
#[no_mangle]
pub unsafe extern "C" fn esp32sim_radar_set(e: *mut Emu, id: u32, field: u32, value: f64) -> u32 {
    if e.is_null() {
        return 1;
    }
    unsafe { &mut *e }.m.radar_set(id, field, value)
}
#[cfg(test)]
mod radar_tests {
    use super::*;
    #[test]
    fn physical_tx_routes_and_baud_follow_each_chip() {
        macro_rules! check {
            ($machine:expr,$signal:expr,$clock:expr) => {{
                let mut m = $machine;
                m.bus.periph.gpio.func_out_sel[5] = $signal;
                m.bus.periph.gpio.enable |= 1 << 5;
                m.bus.periph.uart[1].write(0x14, (5 << 20) | 2083);
                m.bus.periph.uart[1].write(0x78, (3 << 20) | (1 << 12));
                $clock(&mut m);
                assert!(m.bus.uart_tx_route(1, 5, 9600));
                assert!(!m.bus.uart_tx_route(0, 5, 9600));
                assert!(!m.bus.uart_tx_route(1, 4, 9600));
                assert!(!m.bus.uart_tx_route(1, 5, 19200));
                m.bus.periph.gpio.enable = 0;
                assert!(!m.bus.uart_tx_route(1, 5, 9600));
            }};
        }
        check!(esp32s3::machine([0; 6]), 15, |_: &mut esp32s3::Machine| {});
        check!(
            esp32c3::machine([0; 6], 4096),
            9,
            |_: &mut esp32c3::Machine| {}
        );
        check!(
            esp32c6::machine([0; 6], 4096),
            9,
            |m: &mut esp32c6::Machine| {
                xtensa_lx7::Bus::write32(&mut m.bus, 0x60096010, (3 << 20) | (1 << 12)).unwrap();
            }
        );
    }
    #[test]
    fn bounded_radar_abi_rejects_conflicts_and_preserves_targets_on_guest_reboot() {
        let mut s3 = esp32s3::machine([0;6]);
        assert_ne!(MachineApi::radar_configure(&mut s3,0,22,5),0);
        let mut m = esp32c3::machine([0; 6], 4096);
        assert_eq!(MachineApi::radar_configure(&mut m, 0, 4, 5), 0);
        assert_ne!(MachineApi::radar_configure(&mut m, 4, 6, 7), 0);
        assert_ne!(MachineApi::radar_configure(&mut m, 1, 22, 7), 0);
        assert_ne!(MachineApi::radar_configure(&mut m, 1, 5, 6), 0);
        assert_ne!(MachineApi::gps_configure(&mut m, 0, 4, 9600), 0);
        assert_eq!(MachineApi::radar_set(&mut m, 0, 0, 1.), 0);
        assert_ne!(MachineApi::radar_set(&mut m, 0, 0, 2.), 0);
        m.bus.reboot([0; 6]);
        assert_eq!(MachineApi::radar_set(&mut m, 0, 2, 244.), 0);
    }
}

#[no_mangle]
pub unsafe extern "C" fn esp32sim_radar_generation(e:*const Emu,id:u32)->u32 {if e.is_null(){return u32::MAX;} unsafe{&*e}.m.radar_generation(id)}
#[no_mangle]
pub unsafe extern "C" fn esp32sim_radar_value(e:*const Emu,id:u32,field:u32)->f64 {if e.is_null(){return f64::NAN;} unsafe{&*e}.m.radar_value(id,field)}

#[no_mangle]
pub unsafe extern "C" fn esp32sim_pzem_configure(e:*mut Emu,id:u32,tx:u32,rx:u32,range:u32,address:u32)->u32 {
    if e.is_null() || unsafe{&*e}.booted {return 1;} unsafe{&mut *e}.m.pzem_configure(id,tx,rx,range,address)
}
#[no_mangle]
pub unsafe extern "C" fn esp32sim_pzem_set(e:*mut Emu,id:u32,field:u32,value:f64)->u32 {if e.is_null(){return 1;} unsafe{&mut *e}.m.pzem_set(id,field,value)}
#[no_mangle]
pub unsafe extern "C" fn esp32sim_pzem_generation(e:*const Emu,id:u32)->u32 {if e.is_null(){return u32::MAX;} unsafe{&*e}.m.pzem_generation(id)}
#[no_mangle]
pub unsafe extern "C" fn esp32sim_pzem_value(e:*const Emu,id:u32,field:u32)->f64 {if e.is_null(){return f64::NAN;} unsafe{&*e}.m.pzem_value(id,field)}

#[cfg(test)]
mod pzem_tests {
    use super::*;
    #[test]
    fn meter_abi_rejects_invalid_chip_pins_variant_address_and_cross_uart_conflicts() {
        unsafe {for chip in ["none","esp32c3","esp32c6"] {
            let e=esp32sim_new(chip.as_ptr(),chip.len(),4,0);assert!(!e.is_null());
            assert_eq!(esp32sim_pzem_configure(e,0,4,5,100,1),0);
            assert_eq!(esp32sim_pzem_configure(e,1,6,7,10,1),0);
            assert_eq!(esp32sim_pzem_configure(e,4,8,9,10,1),1);
            assert_eq!(esp32sim_pzem_configure(e,2,49,8,10,1),1);
            assert_eq!(esp32sim_pzem_configure(e,2,8,9,20,1),1);
            assert_eq!(esp32sim_pzem_configure(e,2,8,9,10,248),1);
            assert_eq!(esp32sim_pzem_configure(e,2,5,9,10,1),1);
            assert_eq!(esp32sim_radar_configure(e,0,9,4),1);
            assert_eq!(esp32sim_gps_configure(e,0,5,9600),1);
            assert_eq!(esp32sim_radar_configure(e,0,10,11),0);
            assert_eq!(esp32sim_pzem_configure(e,2,12,11,10,1),1);
            assert_eq!(esp32sim_gps_configure(e,0,13,9600),0);
            assert_eq!(esp32sim_pzem_configure(e,2,13,14,10,1),1);
            assert_eq!(esp32sim_pzem_set(e,0,1,99.),0);assert_eq!(esp32sim_pzem_set(e,1,1,99.),1);
            assert_eq!(esp32sim_pzem_generation(e,0),0);assert!(esp32sim_pzem_value(e,0,0).is_nan());
            assert_eq!(esp32sim_pzem_set(e,0,0,f64::NAN),1);
            (*e).booted=true;assert_eq!(esp32sim_pzem_configure(e,0,4,5,100,1),1);
            esp32sim_delete(e);
        }}
    }
}

#[cfg(test)]
mod lcd_tests {
    use super::*;
    #[test]
    fn lcd_abi_rejects_invalid_geometry_pin_and_booted_mutation() {
        unsafe {for chip in ["none","esp32c3","esp32c6"] {
            let e=esp32sim_new(chip.as_ptr(),chip.len(),4,0);assert!(!e.is_null());
            let record=[0,4,5,0x27,20,4,0,0];
            assert_eq!(esp32sim_lcds(e,record.as_ptr(),7),1);
            assert_eq!(esp32sim_lcds(e,record.as_ptr(),8),0);
            for bad in [[0,4,5,0x27,40,4,0,0],[0,60,5,0x27,20,4,0,0],[0,4,5,0x27,20,4,1,0]] {assert_eq!(esp32sim_lcds(e,bad.as_ptr(),8),1);}
            (*e).booted=true;assert_eq!(esp32sim_lcds(e,record.as_ptr(),8),1);esp32sim_delete(e);
        }}
    }
}
#[no_mangle]
pub unsafe extern "C" fn esp32sim_pwm_expanders(e:*mut Emu,ptr:*const u8,len:usize)->u32{
    if e.is_null() || (ptr.is_null()&&len!=0) || len>4*16 || len%16!=0 {return 1;}
    let e=unsafe{&mut *e};if e.booted{return 1;}
    let bytes=if len==0 {&[]} else {unsafe{std::slice::from_raw_parts(ptr,len)}};
    let mut configs=Vec::new();for r in bytes.chunks_exact(16){if r[5..8]!=[0;3]{return 1;}configs.push(esp_soc::devices::pca9685::Config{id:r[0],sda:r[1],scl:r[2],address:r[3],oe:r[4],oscillator_hz:u32::from_le_bytes(r[8..12].try_into().unwrap()),external_hz:u32::from_le_bytes(r[12..16].try_into().unwrap())});}
    match e.m.configure_pwm_expanders(&configs){Ok(())=>0,Err(message)=>{log(&message);1}}
}
#[no_mangle]
pub unsafe extern "C" fn esp32sim_pwm_expander_clock(e:*mut Emu,id:u32,hz:u32)->u32{if e.is_null()||id>=4{return 1;}u32::from(!unsafe{&mut *e}.m.pwm_expander_clock(id as u8,hz))}
#[no_mangle]
pub unsafe extern "C" fn esp32sim_pwm_expander_info(e:*const Emu,id:u32,channel:u32,field:u32)->f64{
    if e.is_null()||id>=4||channel>=16{return f64::NAN;}let e=unsafe{&*e};if !e.m.has_pwm_expander(id as u8){return f64::NAN;}
    let Some((hz,duty))=e.m.pwm_expander_output(id as u8,channel as u8)else{return 0.;};
    match field {0=>hz,1=>duty as f64/65535.,2=>duty as f64/65535.*1_000_000./hz,_=>f64::NAN}
}
#[no_mangle]
pub unsafe extern "C" fn esp32sim_servo_configure_pca(e:*mut Emu,id:u32,device:u32,channel:u32,min_us:u32,max_us:u32,min_angle:i32,max_angle:i32)->u32{
    if e.is_null() || id>=16 || device>=4 || channel>=16{return 1;}let e=unsafe{&mut *e};if !e.m.has_pwm_expander(device as u8){return 1;}
    let source=esp_soc::devices::servo::Source::Pca9685{device:device as u8,channel:channel as u8};
    let Some(servo)=esp_soc::devices::servo::Servo::from_source(source,min_us,max_us,min_angle,max_angle)else{return 2;};
    if let Some(previous)=e.servos[id as usize].as_mut().filter(|s|s.source==source){previous.configure(servo);}else{e.servos[id as usize]=Some(servo);}0
}

#[cfg(test)]
mod pwm_expander_tests {
    use super::*;
    #[test]
    fn pca_configuration_calibration_servo_sources_and_collisions_remain_bounded() {
        unsafe {for chip in ["none","esp32c3","esp32c6"] {
            let e=esp32sim_new(chip.as_ptr(),chip.len(),4,0);assert!(!e.is_null());
            let mut record=[0u8;16];record[..5].copy_from_slice(&[0,4,5,0x40,6]);record[8..12].copy_from_slice(&25_000_000u32.to_le_bytes());
            assert_eq!(esp32sim_pwm_expanders(e,record.as_ptr(),15),1);assert_eq!(esp32sim_pwm_expanders(e,record.as_ptr(),16),0);
            assert_eq!(esp32sim_servo_configure_pca(e,0,0,0,1000,2000,0,180000),0);
            assert_eq!(esp32sim_servo_configure_pca(e,1,0,0,500,2500,-90000,90000),0);
            assert_eq!(esp32sim_servo_configure_pca(e,0,1,0,1000,2000,0,180000),1);
            assert_eq!(esp32sim_servo_configure_pca(e,0,0,16,1000,2000,0,180000),1);
            assert_eq!(esp32sim_servo_configure_pca(e,0,0,0,2000,1000,0,180000),2);
            assert_eq!(esp32sim_pwm_expander_clock(e,0,27_000_000),0);assert_eq!(esp32sim_pwm_expander_clock(e,0,0),1);
            assert_eq!(esp32sim_pwm_expander_info(e,0,0,0),0.);assert!(esp32sim_pwm_expander_info(e,1,0,0).is_nan());
            let duplicate=[record,record].concat();assert_eq!(esp32sim_pwm_expanders(e,duplicate.as_ptr(),32),1);
            let ht=[0,1,1,4,5,0x70,4,0];assert_eq!(esp32sim_led_displays(e,ht.as_ptr(),8),1);
            assert_eq!(esp32sim_pwm_expanders(e,std::ptr::null(),0),0);assert_eq!(esp32sim_led_displays(e,ht.as_ptr(),8),0);assert_eq!(esp32sim_pwm_expanders(e,record.as_ptr(),16),1);
            let mut other=record;other[2]=49;assert_eq!(esp32sim_pwm_expanders(e,other.as_ptr(),16),1);
            (*e).booted=true;assert_eq!(esp32sim_pwm_expanders(e,record.as_ptr(),16),1);esp32sim_delete(e);
        }}
    }
}

#[no_mangle]
pub unsafe extern "C" fn esp32sim_steppers(e:*mut Emu,ptr:*const u8,len:usize)->u32 {
    if e.is_null() || len>128 || len%8!=0 || (len>0 && ptr.is_null()) {return 1;}
    let e=unsafe{&mut *e};if e.booted{return 1;}
    let bytes=if len==0{&[]}else{unsafe{std::slice::from_raw_parts(ptr,len)}};
    let mut configs=Vec::new();for r in bytes.chunks_exact(8){if r[4]>1 || r[5]>8 || r[6]!=0 || r[7]!=0{return 1;}configs.push(esp_soc::devices::stepper::StepperConfig{id:r[0],step:r[1],dir:r[2],enable:r[3],enable_active_low:r[4]!=0,microsteps:1<<r[5]});}
    match e.m.configure_steppers(&configs){Ok(())=>0,Err(message)=>{log(&message);1}}
}
#[no_mangle]
pub unsafe extern "C" fn esp32sim_stepper_position(e:*mut Emu,id:u32)->f64 {
    if e.is_null(){f64::NAN}else{unsafe{&*e}.m.stepper_position(id)}
}

#[cfg(test)]
mod stepper_tests {
    use super::*;
    #[test]
    fn stepper_abi_bounds_routes_ratios_and_mutations() {
        unsafe {for chip in ["none","esp32c3","esp32c6"] {
            let e=esp32sim_new(chip.as_ptr(),chip.len(),4,0);assert!(!e.is_null());
            let record=[0,4,5,6,1,2,0,0];
            assert_eq!(esp32sim_steppers(e,record.as_ptr(),7),1);
            assert_eq!(esp32sim_steppers(e,record.as_ptr(),8),0);
            assert_eq!(esp32sim_stepper_position(e,0),0.);assert!(esp32sim_stepper_position(e,16).is_nan());
            for bad in [[0,4,4,6,1,2,0,0],[0,60,5,6,1,2,0,0],[0,4,5,6,1,9,0,0],[0,4,5,6,2,0,0,0]] {assert_eq!(esp32sim_steppers(e,bad.as_ptr(),8),1);}
            let duplicate=[record,record].concat();assert_eq!(esp32sim_steppers(e,duplicate.as_ptr(),16),1);
            (*e).booted=true;assert_eq!(esp32sim_steppers(e,record.as_ptr(),8),1);esp32sim_delete(e);
        }}
    }
}
