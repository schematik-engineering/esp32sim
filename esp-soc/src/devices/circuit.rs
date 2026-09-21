use crate::board::BoardModel;
use super::{Ws2812Chain, OledConfig, Ssd1306, Ssd1306I2c};
use std::sync::{Arc, Mutex};

/// Devices supplied by a project, indexed by their physical data pin.
pub struct CircuitBoard {
    pwm_expanders: Vec<Arc<Mutex<super::pca9685::Pca9685>>>,
    pub camera: Option<Arc<Mutex<super::camera::Camera>>>,
    sensor_clock: Arc<std::sync::atomic::AtomicU64>,
    sensors: Vec<Arc<Mutex<super::Sensor>>>,
    steppers: Vec<super::stepper::Stepper>,
    inputs: Option<super::inputs::InputDevices>,
    pin_sensors: Option<super::pin_sensor::PinSensors>,
    rfid: Vec<super::rfid::Rfid>,
    thermocouples: Vec<super::thermocouple::Thermocouple>,
    load_cells: Vec<super::hx711::LoadCell>,
    gestures: Vec<Arc<Mutex<super::gesture::GestureSensor>>>,
    touches: Vec<Arc<Mutex<super::touch::TouchController>>>,
    resistive_touches: Vec<Arc<Mutex<super::resistive_touch::ResistiveTouch>>>,
    strips: Vec<(u8, Ws2812Chain)>,
    gpio_hz: u32,
    max_displays: Vec<super::max7219::Max7219>,
    led_displays: Vec<Arc<Mutex<super::led_display::LedDisplay>>>,
    spi_displays: Vec<super::spi_display::SpiDisplay>,
    oleds: Vec<Arc<Mutex<Ssd1306>>>,
    parallel_lcds: Vec<super::lcd::ParallelLcd>,
    lcds: Vec<Arc<Mutex<super::lcd::Lcd>>>,
}
impl CircuitBoard {
    fn lcd_route(&self,route:(u8,u8,u8))->bool {self.lcds.iter().any(|d|{let c=d.lock().unwrap().config;(c.sda,c.scl,c.address)==route})}
    fn pwm_route(&self,route:(u8,u8,u8))->bool {self.pwm_expanders.iter().any(|p|{let c=p.lock().unwrap().config;(c.sda,c.scl)==(route.0,route.1) && [c.address,0x70].contains(&route.2)})}
    fn other_i2c_route(&self,route:(u8,u8,u8))->bool {
        self.led_route(route) || self.lcd_route(route) || self.sensors.iter().any(|p|{let c=p.lock().unwrap().config;(c.sda,c.scl,c.address)==route})
        || self.oleds.iter().any(|p|{let c=p.lock().unwrap().config;(c.sda,c.scl,c.address)==route})
        || self.touches.iter().any(|p|{let c=p.lock().unwrap().config;(c.sda,c.scl,c.address)==route})
        || self.resistive_touches.iter().any(|p|{let c=p.lock().unwrap().config;c.model==2 && (c.pins[0],c.pins[1],c.pins[2])==route})
        || self.gestures.iter().any(|p|{let c=p.lock().unwrap().config;(c.sda,c.scl,0x39)==route})
        || self.camera.as_ref().is_some_and(|p|{let c=p.lock().unwrap().config;(c.pins[0],c.pins[1],if c.sensor==0x26 {0x30}else{0x3c})==route})
    }
    fn led_route(&self,route:(u8,u8,u8))->bool {self.led_displays.iter().any(|d|{let c=d.lock().unwrap().config;c.controller==1 && (c.a,c.b,c.address)==route})}
    fn pin_sensor_uses(&self,pin:u8)->bool {self.pin_sensors.as_ref().is_some_and(|s|s.levels().iter().any(|(p,_)|*p==pin))}

    pub fn new(strips: &[(u8, usize)], oleds: &[OledConfig]) -> Result<Self, String> {
        if strips.len() > 16 { return Err("at most 16 LED strips are supported".into()); }
        let mut seen = 0u64;
        let mut devices = Vec::new();
        for &(pin, count) in strips {
            if pin >= 49 || count == 0 || count > 300 || seen & (1u64 << pin) != 0 {
                return Err("invalid or duplicate LED strip wiring".into());
            }
            seen |= 1u64 << pin;
            devices.push((pin, Ws2812Chain::new(count)));
        }
        if oleds.len() > 16 { return Err("at most 16 OLED displays are supported".into()); }
        let mut displays = Vec::new();
        for (index, config) in oleds.iter().enumerate() {
            if oleds[..index].iter().any(|other| other.id == config.id || (other.sda, other.scl, other.address) == (config.sda, config.scl, config.address)) {
                return Err("duplicate OLED identity or bus address".into());
            }
            displays.push(Arc::new(Mutex::new(Ssd1306::new(*config)?)));
        }
        Ok(Self { pwm_expanders:Vec::new(), camera: None, sensor_clock: Arc::new(std::sync::atomic::AtomicU64::new(0)), sensors: Vec::new(), inputs: None, steppers: Vec::new(), pin_sensors: None, rfid: Vec::new(), thermocouples: Vec::new(), gestures: Vec::new(), load_cells: Vec::new(), touches: Vec::new(), resistive_touches: Vec::new(), spi_displays: Vec::new(), led_displays: Vec::new(), max_displays: Vec::new(), strips: devices, gpio_hz: 0, oleds: displays, parallel_lcds: Vec::new(), lcds: Vec::new() })
    }
    pub fn configure_gpio_clock(&mut self, hz: u32) { self.gpio_hz = hz; }
    pub fn configure_sensors(&mut self, configs:&[super::SensorConfig], hz:u32)->Result<(),String> {
        if configs.len()>16 || hz==0 {return Err("invalid sensor count or clock".into());}
        for (i,c) in configs.iter().enumerate() {
            let route=(c.sda,c.scl,c.address);
            if self.led_route(route) || self.lcd_route(route) || self.pwm_route(route) {return Err("I2C address overlaps an LED display".into());}
            if !c.valid() || configs[..i].iter().any(|p|p.id==c.id || (p.sda,p.scl,p.address)==route)
                || self.oleds.iter().any(|p|{let p=p.lock().unwrap().config;(p.sda,p.scl,p.address)==route}) {
                return Err("invalid sensor model, identity or bus address".into());
            }
        }
        self.sensors=configs.iter().map(|c|Arc::new(Mutex::new(super::Sensor::new(*c,self.sensor_clock.clone(),hz)))).collect();
        Ok(())
    }

}
impl BoardModel for CircuitBoard {
    fn configure_steppers(&mut self,configs:&[super::stepper::StepperConfig])->Result<(),String> {
        if configs.len()>16 || configs.iter().enumerate().any(|(i,c)|!c.valid() || configs[..i].iter().any(|p|p.id==c.id)) {return Err("invalid or duplicate step/direction driver identity".into());}
        self.steppers=configs.iter().map(|c|super::stepper::Stepper::new(*c)).collect::<Result<_,_>>()?;Ok(())
    }
    fn stepper_position(&self,id:u8)->f64 {self.steppers.iter().find(|d|d.config.id==id).map_or(f64::NAN,|d|d.position())}


    fn configure_parallel_lcds(&mut self,configs:&[super::lcd::ParallelLcdConfig],hz:u64)->Result<(),String>{
        if configs.len()+self.lcds.len()>16 || configs.iter().enumerate().any(|(i,c)|
            !c.valid() || self.lcds.iter().any(|p|p.lock().unwrap().config.id==c.id) || configs[..i].iter().any(|p|c.conflicts(p))) {
            return Err("invalid or overlapping parallel LCD identity or wiring".into());
        }
        self.parallel_lcds=configs.iter().map(|c|super::lcd::ParallelLcd::new(*c,self.sensor_clock.clone(),hz)).collect::<Result<_,_>>()?;
        Ok(())
    }
    fn configure_lcds(&mut self,configs:&[super::lcd::LcdConfig],hz:u64)->Result<(),String>{
        if configs.len()+self.parallel_lcds.len()>16 {return Err("at most16 character LCDs are supported".into());}
        let mut devices=Vec::new();
        for (i,c) in configs.iter().enumerate(){
            let route=(c.sda,c.scl,c.address);
            if !c.valid() || self.parallel_lcds.iter().any(|p|p.config.id==c.id) || configs[..i].iter().any(|p|p.id==c.id || (p.sda,p.scl,p.address)==route)
                || self.led_route(route) || self.pwm_route(route)
                || self.sensors.iter().any(|p|{let p=p.lock().unwrap().config;(p.sda,p.scl,p.address)==route})
                || self.oleds.iter().any(|p|{let p=p.lock().unwrap().config;(p.sda,p.scl,p.address)==route})
                || self.touches.iter().any(|p|{let p=p.lock().unwrap().config;(p.sda,p.scl,p.address)==route})
                || self.gestures.iter().any(|p|{let p=p.lock().unwrap().config;(p.sda,p.scl,0x39)==route})
                || self.resistive_touches.iter().any(|p|{let p=p.lock().unwrap().config;p.model==2 && (p.pins[0],p.pins[1],p.pins[2])==route})
                || self.camera.as_ref().is_some_and(|p|{let p=p.lock().unwrap().config;(p.pins[0],p.pins[1],if p.sensor==0x26{0x30}else{0x3c})==route}) {return Err("invalid or overlapping character LCD identity or I2C route".into());}
            devices.push(Arc::new(Mutex::new(super::lcd::Lcd::new(*c,self.sensor_clock.clone(),hz)?)));
        }
        self.lcds=devices;Ok(())
    }
    fn configure_pwm_expanders(&mut self,configs:&[super::pca9685::Config],hz:u64)->Result<(),String>{
        if configs.len()>4{return Err("at most four PWM expanders are supported".into());}
        let mut devices=Vec::new();
        for (i,c) in configs.iter().enumerate(){
            if configs[..i].iter().any(|p|p.id==c.id || (p.sda,p.scl,p.address)==(c.sda,c.scl,c.address))
                || self.other_i2c_route((c.sda,c.scl,c.address)) || self.other_i2c_route((c.sda,c.scl,0x70)) {return Err("PWM expander identity or I2C address collision".into());}
            devices.push(Arc::new(Mutex::new(super::pca9685::Pca9685::new(*c,hz)?)));
        }
        self.pwm_expanders=devices;Ok(())
    }
    fn has_pwm_expander(&self,id:u8)->bool{self.pwm_expanders.iter().any(|p|p.lock().unwrap().config.id==id)}
    fn pwm_expander_clock(&mut self,id:u8,hz:u32)->bool{self.pwm_expanders.iter().find(|p|p.lock().unwrap().config.id==id).is_some_and(|p|p.lock().unwrap().set_clock(hz))}
    fn pwm_expander_output(&self,id:u8,channel:u8)->Option<(f64,u32)>{self.pwm_expanders.iter().find(|p|p.lock().unwrap().config.id==id).and_then(|p|p.lock().unwrap().pwm(channel))}
    fn configure_led_displays(&mut self,configs:&[super::led_display::LedDisplayConfig],hz:u64)->Result<(),String> {
        if configs.len()>16 {return Err("at most 16 LED displays are supported".into());}
        let mut displays=Vec::new();let mut max_displays=Vec::new();
        for (i,c) in configs.iter().enumerate() {
            if configs[..i].iter().any(|p|p.id==c.id || (p.controller==1 && c.controller==1 && (p.a,p.b,p.address)==(c.a,c.b,c.address)) || (p.controller==3 && c.controller==3 && p.address==c.address) || ((p.controller==2 || c.controller==2) && p.pins().iter().any(|pin|c.pins().contains(pin)))) {return Err("duplicate LED display identity or wiring".into());}
            let route=(c.a,c.b,c.address);
            if c.controller==1 && (self.lcd_route(route) || self.pwm_route(route) || self.sensors.iter().any(|p|{let p=p.lock().unwrap().config;(p.sda,p.scl,p.address)==route})
                || self.oleds.iter().any(|p|{let p=p.lock().unwrap().config;(p.sda,p.scl,p.address)==route})
                || self.touches.iter().any(|p|{let p=p.lock().unwrap().config;(p.sda,p.scl,p.address)==route})
                || self.resistive_touches.iter().any(|p|{let p=p.lock().unwrap().config;p.model==2 && (p.pins[0],p.pins[1],p.pins[2])==route})) {return Err("LED display I2C address collision".into());}
            if c.controller==2 && [c.a,c.b].iter().any(|pin|self.pin_sensor_uses(*pin)||self.inputs.as_ref().is_some_and(|inputs|inputs.owns_input(*pin))) {return Err("TM1637 GPIO overlaps an input".into());}
            if c.controller==3 {
                if self.thermocouples.iter().any(|d|d.config.cs==c.address) || self.spi_displays.iter().any(|p|p.config.cs==Some(c.address)) || self.rfid.iter().any(|p|p.config.cs==c.address) || self.resistive_touches.iter().any(|p|{let p=p.lock().unwrap().config;p.model==1 && p.pins[3]==c.address}) {return Err("MAX7219 chip select overlaps another SPI device".into());}
                max_displays.push(super::max7219::Max7219::new(*c)?);
            } else {displays.push(Arc::new(Mutex::new(super::led_display::LedDisplay::new(*c,hz)?)));}
        }
        self.led_displays=displays;self.max_displays=max_displays;Ok(())
    }

    fn configure_pin_sensors(&mut self,configs:&[super::pin_sensor::Config],hz:u64)->Result<(),String>{
        if configs.iter().any(|c|self.thermocouples.iter().any(|d|d.config.miso==c.pin)||self.load_cells.iter().any(|l|[l.config.dout,l.config.sck].contains(&c.pin))||self.gestures.iter().any(|g|{let g=g.lock().unwrap().config;[g.sda,g.scl,g.irq].contains(&c.pin)})||self.resistive_touches.iter().any(|t|t.lock().unwrap().config.gpio_pins().contains(&c.pin))||self.inputs.as_ref().is_some_and(|i|i.owns_input(c.pin))||self.touches.iter().any(|t|{let c2=t.lock().unwrap().config;[c2.sda,c2.scl,c2.irq,c2.reset].contains(&c.pin)})) {return Err("GPIO sensor pin overlaps another input protocol".into());}
        self.pin_sensors=Some(super::pin_sensor::PinSensors::new(configs,hz)?);Ok(())
    }
    fn pin_sensor_set(&mut self,id:u8,field:u32,value:f64)->bool{self.pin_sensors.as_mut().is_some_and(|s|s.set(id,field,value))}
    fn pin_sensor_generation(&self,id:u8)->u32{self.pin_sensors.as_ref().map_or(u32::MAX,|s|s.generation(id))}
    fn pin_sensor_value(&self,id:u8,field:u32)->f64{self.pin_sensors.as_ref().map_or(f64::NAN,|s|s.value(id,field))}

    fn advance_to(&mut self, cycle:u64) {for device in &mut self.thermocouples {device.advance(cycle);}for (_,strip) in &mut self.strips {strip.advance_gpio(cycle,self.gpio_hz);}for p in &self.pwm_expanders {p.lock().unwrap().advance(cycle);}for display in &self.led_displays {display.lock().unwrap().advance(cycle);}for cell in &mut self.load_cells {cell.advance(cycle);}for sensor in &self.gestures {sensor.lock().unwrap().advance(cycle);}for reader in &mut self.rfid {reader.advance_to(cycle);} if let Some(s)=&mut self.pin_sensors{s.advance(cycle);} if let Some(inputs)=&mut self.inputs {inputs.advance_to(cycle);} self.sensor_clock.store(cycle,std::sync::atomic::Ordering::Relaxed); for lcd in &self.lcds {lcd.lock().unwrap().advance();} for lcd in &mut self.parallel_lcds {lcd.lcd.advance();} for t in &self.touches {t.lock().unwrap().advance(cycle);} for t in &self.resistive_touches {t.lock().unwrap().advance(cycle);} }
    fn configure_load_cells(&mut self,configs:&[super::hx711::LoadCellConfig],hz:u64)->Result<(),String>{
        if configs.len()>16||hz==0{return Err("invalid load cell count or clock".into());}
        for (i,c) in configs.iter().enumerate(){
            if !c.valid()||configs[..i].iter().any(|p|p.id==c.id||p.dout==c.dout||p.dout==c.sck||p.sck==c.dout)
                || [c.dout,c.sck].iter().any(|pin|self.pin_sensor_uses(*pin)||self.inputs.as_ref().is_some_and(|p|p.owns_input(*pin))) {
                return Err("invalid load cell identity, calibration or physical GPIO wiring".into());
            }
        }
        let now=self.sensor_clock.load(std::sync::atomic::Ordering::Relaxed);
        self.load_cells=configs.iter().map(|c|super::hx711::LoadCell::new(*c,hz,now)).collect();Ok(())
    }
    fn load_cell_weight(&mut self,id:u8,value:f64)->bool{self.load_cells.iter_mut().find(|c|c.config.id==id).is_some_and(|c|c.weight(value))}
    fn load_cell_calibrate(&mut self,id:u8,capacity:f64,sensitivity:f64,offset:i32)->bool{self.load_cells.iter_mut().find(|c|c.config.id==id).is_some_and(|c|c.calibrate(capacity,sensitivity,offset))}
    fn configure_gestures(&mut self,configs:&[super::gesture::GestureConfig],hz:u64)->Result<(),String>{
        if configs.len()>16||hz==0{return Err("invalid gesture sensor count or clock".into());}
        for (i,c) in configs.iter().enumerate(){
            if !c.valid()||self.lcd_route((c.sda,c.scl,0x39))||configs[..i].iter().any(|p|p.id==c.id||(p.sda,p.scl)==(c.sda,c.scl))
                || [c.sda,c.scl,c.irq].iter().any(|p|self.pin_sensor_uses(*p))
                || self.sensors.iter().any(|p|{let p=p.lock().unwrap().config;(p.sda,p.scl,p.address)==(c.sda,c.scl,0x39)})
                || self.touches.iter().any(|p|{let p=p.lock().unwrap().config;(p.sda,p.scl,p.address)==(c.sda,c.scl,0x39)}) {
                return Err("invalid gesture identity or physical I2C wiring".into());
            }
        }
        self.gestures=configs.iter().map(|c|Arc::new(Mutex::new(super::gesture::GestureSensor::new(*c,hz)))).collect();Ok(())
    }
    fn gesture(&mut self,id:u8,direction:u8)->bool{
        self.gestures.iter().find(|s|s.lock().unwrap().config.id==id).is_some_and(|s|s.lock().unwrap().gesture(direction))
    }
    fn proximity_reading(&self,id:u8)->u32{self.gestures.iter().find(|s|s.lock().unwrap().config.id==id).map_or(u32::MAX,|s|s.lock().unwrap().proximity_reading())}
    fn proximity(&mut self,id:u8,value:f64)->bool{
        self.gestures.iter().find(|s|s.lock().unwrap().config.id==id).is_some_and(|s|s.lock().unwrap().proximity(value))
    }
    fn configure_thermocouples(&mut self,configs:&[super::thermocouple::Config],hz:u64)->Result<(),String>{
        if configs.len()>16 || hz==0 {return Err("invalid thermocouple count or clock".into());}
        for (i,c) in configs.iter().enumerate(){
            if !c.valid() || configs[..i].iter().any(|p|p.id==c.id || p.cs==c.cs)
                || self.rfid.iter().any(|d|d.config.cs==c.cs)
                || self.max_displays.iter().any(|d|d.config.address==c.cs)
                || self.spi_displays.iter().any(|d|d.config.cs==Some(c.cs))
                || self.resistive_touches.iter().any(|d|{let c2=d.lock().unwrap().config;c2.model==1 && c2.pins[3]==c.cs})
                || self.pin_sensor_uses(c.miso) || self.inputs.as_ref().is_some_and(|d|d.owns_input(c.miso)) {
                return Err("invalid thermocouple identity or conflicting SPI wiring".into());
            }
        }
        let now=self.sensor_clock.load(std::sync::atomic::Ordering::Relaxed);
        self.thermocouples=configs.iter().map(|c|super::thermocouple::Thermocouple::new(*c,hz,now)).collect();Ok(())
    }
    fn thermocouple_set(&mut self,id:u8,field:u32,value:f64)->bool{self.thermocouples.iter_mut().find(|d|d.config.id==id).is_some_and(|d|d.set(field,value))}
    fn thermocouple_generation(&self,id:u8)->u32{self.thermocouples.iter().find(|d|d.config.id==id).map_or(u32::MAX,|d|d.generation())}
    fn thermocouple_value(&self,id:u8,field:u32)->f64{self.thermocouples.iter().find(|d|d.config.id==id).map_or(f64::NAN,|d|d.value(field))}
    fn configure_rfid(&mut self,configs:&[super::rfid::RfidConfig],hz:u64)->Result<(),String>{
        if configs.len()>16||hz==0{return Err("invalid RFID count or clock".into());}
        for (i,c) in configs.iter().enumerate(){
            if !c.valid()||self.thermocouples.iter().any(|d|d.config.cs==c.cs)||self.max_displays.iter().any(|p|p.config.address==c.cs)||configs[..i].iter().any(|p|p.id==c.id||p.cs==c.cs)||self.spi_displays.iter().any(|p|p.config.cs==Some(c.cs))||self.resistive_touches.iter().any(|t|{let t=t.lock().unwrap();t.config.model==1&&t.config.pins[3]==c.cs}){return Err("invalid RFID wiring or duplicate chip select".into());}
        }
        self.rfid=configs.iter().map(|c|super::rfid::Rfid::new(*c,hz)).collect();Ok(())
    }
    fn rfid_card(&mut self,id:u8,uid:&[u8])->bool{self.rfid.iter_mut().find(|r|r.config.id==id).is_some_and(|r|r.card(uid))}
    fn configure_inputs(&mut self, configs:&[super::inputs::InputConfig], hz:u64)->Result<(),String> {
        if configs.iter().flat_map(|c|c.pins()).any(|pin|self.pin_sensor_uses(pin)||self.thermocouples.iter().any(|d|d.config.miso==pin)||self.load_cells.iter().any(|l|[l.config.dout,l.config.sck].contains(&pin))){return Err("input pin overlaps a GPIO sensor".into());}
        self.inputs=Some(super::inputs::InputDevices::new(configs,hz)?); Ok(())
    }
    fn distance_mm(&mut self,id:u8,value:u32)->bool {self.inputs.as_mut().is_some_and(|inputs|inputs.distance_mm(id,value))}
    fn keypad_press(&mut self,id:u8,row:usize,column:usize)->bool {self.inputs.as_mut().is_some_and(|inputs|inputs.keypad_press(id,row,column))}
    fn encoder_steps(&mut self,id:u8,steps:i32)->bool {self.inputs.as_mut().is_some_and(|inputs|inputs.encoder_steps(id,steps))}
    fn gpio_waveform(&mut self, cycle:u64, gpio:&esp_periph::gpio::Gpio, signal:u32) {
        for (pin,strip) in &mut self.strips {
            let route=gpio.func_out_sel[*pin as usize];
            let enabled=gpio.enable & (1u64<<*pin)!=0 && route & (signal*2-1)==signal
                && route & (signal*8)==0 && (gpio.io_mux[*pin as usize]>>12)&7==1;
            let high=(gpio.out & (1u64<<*pin)!=0) ^ (route & (signal*2)!=0);
            strip.gpio_drive(cycle,self.gpio_hz,enabled,high);
        }
    }
    fn gpio_drive(&mut self,cycle:u64,enabled:u64,output:u64) {for d in &mut self.thermocouples {d.advance(cycle);d.drive(enabled,output);}for lcd in &mut self.parallel_lcds {lcd.drive(cycle,enabled,output);}for stepper in &mut self.steppers {stepper.drive(enabled,output);}for p in &self.pwm_expanders {p.lock().unwrap().drive(enabled,output);}for cell in &mut self.load_cells {cell.gpio_drive(cycle,enabled,output);}for display in &self.led_displays {display.lock().unwrap().drive(enabled,output);}if let Some(s)=&mut self.pin_sensors{s.gpio_drive(cycle,enabled,output);}if let Some(inputs)=&mut self.inputs {inputs.gpio_drive(cycle,enabled,output);}}
    fn released_inputs(&self)->Vec<u8> {
        let driven=self.input_levels();
        self.inputs.as_ref().map_or_else(Vec::new,|i|i.released_inputs()).into_iter()
            .chain(self.pin_sensors.iter().flat_map(|s|s.levels()).filter_map(|(pin,high)|high.then_some(pin)))
            .chain(self.led_displays.iter().filter_map(|d|{let d=d.lock().unwrap();(d.config.controller==2 && d.ack_pin().is_none()).then_some(d.config.b)}))
            .chain(self.thermocouples.iter().filter(|d|d.level().is_none()).map(|d|d.config.miso))
            .filter(|pin|!driven.iter().any(|(p,_)|p==pin)).collect()
    }
    fn next_deadline(&self)->Option<u64> {self.inputs.as_ref().and_then(|i|i.next_deadline()).into_iter().chain(self.pin_sensors.as_ref().and_then(|s|s.next_deadline())).chain(self.rfid.iter().filter_map(|r|r.next_deadline())).chain(self.thermocouples.iter().filter_map(|d|d.next_deadline())).chain(self.load_cells.iter().filter_map(|c|c.next_deadline())).min()}
    fn take_edges(&mut self)->Vec<crate::board::BoardEdge> {self.inputs.as_mut().map_or_else(Vec::new,|inputs|inputs.take_edges())}
    fn sensor_generation(&mut self,id:u8)->u32 {
        self.sensors.iter().find_map(|s|{let mut s=s.lock().unwrap();(s.config.id==id).then(||s.generation())}).unwrap_or(u32::MAX)
    }
    fn sensor_value(&mut self,id:u8,field:u32)->f64 {
        self.sensors.iter().find_map(|s|{let mut s=s.lock().unwrap();(s.config.id==id).then(||s.value(field))}).unwrap_or(f64::NAN)
    }
    fn sensor_set(&mut self,id:u8,field:u32,value:f64)->bool {
        self.sensors.iter().find_map(|s|{let mut s=s.lock().unwrap();(s.config.id==id).then(||s.set(field,value))}).unwrap_or(false)
    }
    fn gpio_changes(&mut self, changes: &[(u8,bool)]) {
        for display in &mut self.max_displays {for &(pin,level) in changes {display.gpio(pin,level);}}
        for reader in &mut self.rfid {for &(pin,level) in changes {reader.gpio(pin,level);}}
        for display in &mut self.spi_displays { for &(pin, level) in changes { display.gpio(pin, level); } }
        for t in &self.touches { let mut t=t.lock().unwrap(); for &(pin,level) in changes { t.gpio(pin,level); } }
        for t in &self.resistive_touches {let mut t=t.lock().unwrap();for &(pin,level) in changes {t.gpio(pin,level);}}
        if let Some(camera) = &self.camera { let mut camera = camera.lock().unwrap(); for &(pin,level) in changes { camera.gpio(pin,level); } }
    }
    fn camera(&self) -> Option<Arc<Mutex<super::camera::Camera>>> { self.camera.clone() }
    fn configure_camera(&mut self, config: super::camera::CameraConfig) -> bool { self.camera = Some(Arc::new(Mutex::new(super::camera::Camera::new(config)))); true }
    fn camera_frame(&mut self) -> Option<(u32,u32,Arc<Vec<u8>>)> { self.camera.as_ref()?.lock().unwrap().frame() }
    fn configure_touches(&mut self, configs:&[super::touch::TouchConfig], hz:u64)->Result<(),String> {
        if configs.len()+self.resistive_touches.len()>16 || hz==0 {return Err("invalid touch count or clock".into());}
        if configs.iter().any(|c|[c.sda,c.scl,c.irq,c.reset].iter().any(|p|self.pin_sensor_uses(*p))){return Err("touch pin overlaps a GPIO sensor".into());}
        for (i,c) in configs.iter().enumerate() {
            let route=(c.sda,c.scl,c.address);
            if self.led_route(route) || self.lcd_route(route) || self.pwm_route(route) {return Err("I2C address overlaps an LED display".into());}
            if !c.valid() || self.resistive_touches.iter().any(|t|t.lock().unwrap().config.id==c.id) || configs[..i].iter().any(|p|p.id==c.id || (p.sda,p.scl,p.address)==route)
                || self.oleds.iter().any(|p|{let p=p.lock().unwrap().config;(p.sda,p.scl,p.address)==route})
                || self.sensors.iter().any(|p|{let p=p.lock().unwrap().config;(p.sda,p.scl,p.address)==route})
                || self.camera.as_ref().is_some_and(|p|{let p=p.lock().unwrap().config;(p.pins[0],p.pins[1],if p.sensor==0x26 {0x30}else{0x3c})==route}) {
                return Err("invalid touch model, identity or bus address".into());
            }
        }
        self.touches=configs.iter().map(|c|Arc::new(Mutex::new(super::touch::TouchController::new(*c,hz)))).collect();
        Ok(())
    }
    fn touch_device(&mut self,id:u8)->Option<Arc<Mutex<super::touch::TouchController>>> {
        self.touches.iter().find(|t|t.lock().unwrap().config.id==id).cloned()
    }
    fn configure_resistive_touches(&mut self,configs:&[super::resistive_touch::ResistiveConfig],hz:u64)->Result<(),String> {
        if configs.len()+self.touches.len()>16 || hz==0 {return Err("invalid resistive touch count or clock".into());}
        if configs.iter().flat_map(|c|c.gpio_pins()).any(|pin|self.pin_sensor_uses(pin)) {return Err("touch pin overlaps a GPIO sensor".into());}
        for (i,c) in configs.iter().enumerate() {
            if !c.valid() || self.touches.iter().any(|t|t.lock().unwrap().config.id==c.id)
                || configs[..i].iter().any(|p|p.id==c.id || p.model==c.model && p.pins==c.pins) {return Err("invalid resistive touch identity or wiring".into());}
            let route=(c.pins[0],c.pins[1],c.pins[2]);
            if c.model==2 && (self.led_route(route) || self.lcd_route(route) || self.pwm_route(route)) {return Err("I2C address overlaps an LED display".into());}
            if c.model==2 && (self.sensors.iter().any(|p|{let p=p.lock().unwrap().config;(p.sda,p.scl,p.address)==route})
                || self.oleds.iter().any(|p|{let p=p.lock().unwrap().config;(p.sda,p.scl,p.address)==route})
                || self.touches.iter().any(|p|{let p=p.lock().unwrap().config;(p.sda,p.scl,p.address)==route})
                || self.camera.as_ref().is_some_and(|p|{let p=p.lock().unwrap().config;(p.pins[0],p.pins[1],if p.sensor==0x26 {0x30}else{0x3c})==route})) {return Err("conflicting I2C touch route".into());}
            if c.model==1 && (self.thermocouples.iter().any(|d|d.config.cs==c.pins[3]) || self.max_displays.iter().any(|d|d.config.address==c.pins[3]) || self.rfid.iter().any(|r|r.config.cs==c.pins[3])) {return Err("conflicting SPI RFID/touch chip select".into());}
            if c.model==1 && self.spi_displays.iter().any(|p|p.config.sclk==c.pins[0] && p.config.mosi==c.pins[1] && (p.config.cs.is_none() || p.config.cs==Some(c.pins[3]))) {return Err("conflicting SPI display/touch chip select".into());}
        }
        self.resistive_touches=configs.iter().map(|c|Arc::new(Mutex::new(super::resistive_touch::ResistiveTouch::new(*c,hz)))).collect();Ok(())
    }
    fn resistive_touch_device(&mut self,id:u8)->Option<Arc<Mutex<super::resistive_touch::ResistiveTouch>>> {
        self.resistive_touches.iter().find(|t|t.lock().unwrap().config.id==id).cloned()
    }
    fn input_levels(&self)->Vec<(u8,bool)> {
        let mut levels=std::collections::BTreeMap::new();
        for d in &self.thermocouples {if let Some((pin,high))=d.level(){*levels.entry(pin).or_insert(true)&=high;}}
        for cell in &self.load_cells {let(pin,high)=cell.level();*levels.entry(pin).or_insert(true)&=high;}
        for g in &self.gestures {let g=g.lock().unwrap();if g.config.irq!=255 {*levels.entry(g.config.irq).or_insert(true) &= g.irq_high();}}
        for t in &self.touches {let t=t.lock().unwrap();if t.config.irq!=255 {let level=levels.entry(t.config.irq).or_insert(true);*level &= t.irq_high;}}
        if let Some(inputs)=&self.inputs {for (pin,level) in inputs.input_levels() {let previous=levels.entry(pin).or_insert(true);*previous &= level;}}
        for t in &self.resistive_touches {let t=t.lock().unwrap();if t.config.irq!=255 {*levels.entry(t.config.irq).or_insert(true) &= t.irq_high();}}
        if let Some(s)=&self.pin_sensors {for(pin,high)in s.levels(){if !high{levels.insert(pin,false);}}}
        for d in &self.led_displays {if let Some(pin)=d.lock().unwrap().ack_pin() {levels.insert(pin,false);}}
        levels.into_iter().collect()
    }
    fn name(&self) -> &'static str { "project" }
    fn rmt_frame(&mut self, pin: u8, bits: &[bool]) {
        if let Some((_, strip)) = self.strips.iter_mut().find(|(p, _)| *p == pin) {
            strip.from_bits(bits);
        }
    }
    fn parallel_output(&mut self, pins:&[(u8,u8)], samples:&[u16], clock_hz:u32) {
        for &(pin,lane) in pins {
            if let Some((_,strip)) = self.strips.iter_mut().find(|(p,_)| *p==pin) {
                strip.from_samples(samples,lane,clock_hz);
            }
        }
    }
    fn i2c_devices(&mut self) -> Vec<(u8, u8, Box<dyn esp_periph::i2c::I2cDevice>)> {
        let mut devices: Vec<(u8, u8, Box<dyn esp_periph::i2c::I2cDevice>)> = Vec::new();
        if let Some(camera) = &self.camera {
            let address = if camera.lock().unwrap().config.sensor == 0x26 { 0x30 } else { 0x3c };
            for bus in 0..2 { devices.push((bus,address,Box::new(super::camera::CameraI2c(camera.clone())))); }
        }
        for state in &self.pwm_expanders {let address=state.lock().unwrap().config.address;for bus in 0..2 {devices.push((bus,address,Box::new(super::pca9685::PcaI2c::new(state.clone()))));}}
        for state in &self.gestures {for bus in 0..2 {devices.push((bus,0x39,Box::new(super::gesture::GestureI2c::new(state.clone()))));}}
        for state in &self.touches {
            let address=state.lock().unwrap().config.address;
            for bus in 0..2 {devices.push((bus,address,Box::new(super::touch::TouchI2c::new(state.clone()))));}
        }
        for state in &self.resistive_touches {let c=state.lock().unwrap().config;if c.model==2 {for bus in 0..2 {devices.push((bus,c.pins[2],Box::new(super::resistive_touch::Tsc2007::new(state.clone()))));}}}
        for state in &self.sensors {
            let address=state.lock().unwrap().config.address;
            for bus in 0..2 {devices.push((bus,address,Box::new(super::SensorI2c::new(state.clone()))));}
        }
        for state in &self.led_displays {let c=state.lock().unwrap().config;if c.controller==1 {for bus in 0..2 {devices.push((bus,c.address,Box::new(super::led_display::Ht16k33::new(state.clone()))));}}}
        for state in &self.oleds {
            let address = state.lock().unwrap().config.address;
            for bus in 0..2 { devices.push((bus, address, Box::new(Ssd1306I2c::new(state.clone())))); }
        }
        for lcd in &self.lcds {let address=lcd.lock().unwrap().config.address;for bus in 0..2 {devices.push((bus,address,Box::new(super::lcd::LcdI2c(lcd.clone()))));}}
        devices
    }
    fn display_backlight_pins(&self) -> Vec<u8> { self.spi_displays.iter().filter_map(|display|display.config.backlight).collect() }
    fn display_backlight_duty(&mut self, pin:u8, duty:u32) { for display in &mut self.spi_displays { display.backlight_duty(pin,duty); } }
    fn configure_spi_displays(&mut self, configs: &[super::spi_display::SpiDisplayConfig]) -> Result<(), String> {
        if configs.len() > 16 { return Err("at most 16 SPI displays are supported".into()); }
        let mut displays = Vec::new();
        for (i, config) in configs.iter().enumerate() {
            if config.id>=128 || configs[..i].iter().any(|other| other.id == config.id || (other.sclk,other.mosi,other.cs) == (config.sclk,config.mosi,config.cs)) {
                return Err("duplicate SPI display identity or chip select".into());
            }
            if config.cs.is_some_and(|cs|self.thermocouples.iter().any(|d|d.config.cs==cs) || self.max_displays.iter().any(|d|d.config.address==cs) || self.rfid.iter().any(|r|r.config.cs==cs)) {return Err("conflicting SPI RFID/display chip select".into());}
            displays.push(super::spi_display::SpiDisplay::new(*config)?);
        }
        self.spi_displays = displays;
        Ok(())
    }
    fn spi_transfer_pins(&mut self, _host: u8, pins: crate::SpiPins, tx: &[u8], rx_len: usize) -> Vec<u8> {
        for display in &mut self.spi_displays { display.transfer(pins.sclk, pins.mosi, pins.cs, tx); }
        for display in &mut self.max_displays {display.transfer(pins,tx);}
        let mut rx=vec![0xff;rx_len];
        for d in &mut self.thermocouples {if let Some(data)=d.spi(pins,tx,rx_len){for(out,byte)in rx.iter_mut().zip(data){*out &= byte;}}}
        for t in &self.resistive_touches {if let Some(data)=t.lock().unwrap().spi(pins,tx,rx_len) {for (out,byte) in rx.iter_mut().zip(data) {*out &= byte;}}}
        for reader in &mut self.rfid {if let Some(bytes)=reader.spi(pins,tx,rx_len){for (out,byte) in rx.iter_mut().zip(bytes){*out &= byte;}}}
        rx
    }
    fn project_displays(&self) -> Vec<(u8, u16, u16, Vec<u8>, u64, bool)> {
        self.oleds.iter().map(|state| {
            let state = state.lock().unwrap();
            (state.config.id, state.config.width as u16, state.config.height as u16, state.frame(), state.version, false)
        }).chain(self.parallel_lcds.iter().map(|d|{let(w,h)=d.lcd.config.dimensions();(64+d.config.id,w,h,d.lcd.frame(),d.lcd.generation,false)})).chain(self.lcds.iter().map(|d|{let d=d.lock().unwrap();let(w,h)=d.config.dimensions();(64+d.config.id,w,h,d.frame(),d.generation,false)})).chain(self.spi_displays.iter().map(|display| (display.config.id, display.config.width, display.config.height, display.frame(), display.generation, true))).chain(self.led_displays.iter().map(|d|{let d=d.lock().unwrap();let(w,h)=d.config.dimensions();(128+d.config.id,w,h,d.frame(),d.generation,true)})).chain(self.max_displays.iter().map(|d|{let(w,h)=d.config.dimensions();(128+d.config.id,w,h,d.frame(),d.generation,true)})).collect()
    }
    fn strip_frames(&self) -> Vec<(u8, &[[u8; 3]], u64)> {
        self.strips.iter().map(|(pin, strip)| (*pin, strip.leds.as_slice(), strip.updates)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn led_displays_preserve_existing_devices_and_route_same_address_by_wires() {
        let mut board=CircuitBoard::new(&[(10,2)],&[OledConfig{id:0,sda:4,scl:5,address:0x3c,width:128,height:64,controller:crate::devices::OledController::Ssd1306,column_offset:0}]).unwrap();
        let c=super::super::led_display::LedDisplayConfig{id:0,controller:1,layout:3,a:4,b:5,address:0x70,digits:4,colon:false};
        board.configure_led_displays(&[c,super::super::led_display::LedDisplayConfig{id:1,a:6,b:7,..c}],1000).unwrap();
        let mut devices=board.i2c_devices();
        let (_,_,device)=devices.iter_mut().find(|(bus,address,d)|*bus==0 && *address==0x70 && d.pins()==Some((4,5))).unwrap();
        for byte in [0x21,0x81,0xef] {device.start(false);device.write(byte);}
        device.start(false);device.write(0);device.write(1);
        let frames=board.project_displays();assert_eq!(frames.len(),3);
        assert_eq!(frames[0].0,0);assert_eq!(frames[1].0,128);assert_eq!(frames[2].0,129);
        assert!(frames[1].3.iter().any(|b|*b!=0));assert!(frames[2].3.iter().all(|b|*b==0));
        assert_eq!(board.strip_frames().len(),1);
        assert!(board.configure_led_displays(&[c,c],1000).is_err());assert_eq!(board.project_displays().len(),3);
    }
    #[test]
    fn gpio_ws2812_routes_ignore_peripherals_input_pins_and_other_mux_functions() {
        for signal in [128,256] {
            for (route,mux,enable,inverted,valid) in [
                (signal,1<<12,1<<4,false,true),
                (signal | signal*2,1<<12,1<<4,true,true),
                (71,1<<12,1<<4,false,false),
                (signal,0,1<<4,false,false),
                (signal,1<<12,0,false,false),
                (signal | signal*8,1<<12,1<<4,false,false),
            ] {
                let mut board=CircuitBoard::new(&[(4,1),(5,1)],&[]).unwrap();board.configure_gpio_clock(160_000_000);
                let mut gpio=esp_periph::gpio::Gpio::new();gpio.func_out_sel[4]=route;gpio.io_mux[4]=mux;gpio.enable=enable;
                let mut at=0;
                for byte in [0x12u8,0x34,0x56] {for i in (0..8).rev() {
                    let one=byte & (1<<i)!=0;
                    gpio.out=if inverted {0}else{1<<4};board.gpio_waveform(at,&gpio,signal);
                    at+=if one {128}else{64};gpio.out=if inverted {1<<4}else{0};board.gpio_waveform(at,&gpio,signal);
                    at+=if one {72}else{136};
                }}
                board.advance_to(at+8000);
                assert_eq!(board.strips[0].1.updates,u64::from(valid));
                assert_eq!(board.strips[1].1.updates,0,"wrong pin cannot get another strip's frame");
                if valid {assert_eq!(board.strips[0].1.leds,vec![[0x34,0x12,0x56]]);}
            }
        }
    }

    #[test]
    fn strips_keep_their_pin_identity_and_wire_color_order() {
        let mut board = CircuitBoard::new(&[(4, 2), (5, 1)], &[]).unwrap();
        let bits: Vec<_> = [0u8, 255, 0, 255, 0, 0].iter()
            .flat_map(|byte| (0..8).map(move |bit| byte & (0x80 >> bit) != 0)).collect();
        board.rmt_frame(4, &bits);
        let frames = board.strip_frames();
        assert_eq!(frames[0], (4, &[[255, 0, 0], [0, 255, 0]][..], 1));
        assert_eq!(frames[1], (5, &[[0, 0, 0]][..], 0));
        assert!(CircuitBoard::new(&[(4, 1), (4, 2)], &[]).is_err());
        assert!(CircuitBoard::new(&[(49, 1)], &[]).is_err());
        assert!(CircuitBoard::new(&[(4, 301)], &[]).is_err());
    }
}

#[cfg(test)]
mod pin_sensor_wiring_tests {
    use super::*;
    #[test]
    fn overlapping_input_protocols_are_rejected_in_both_configuration_orders() {
        let sensor=super::super::pin_sensor::Config{model:2,id:0,pin:4,rom:[0;8]};
        let input=super::super::inputs::InputConfig::Ultrasonic{id:0,trigger:4,echo:5};
        let mut board=CircuitBoard::new(&[],&[]).unwrap();
        board.configure_pin_sensors(&[sensor],1_000_000).unwrap();
        assert!(board.configure_inputs(&[input.clone()],1_000_000).is_err());
        let mut board=CircuitBoard::new(&[],&[]).unwrap();
        board.configure_inputs(&[input],1_000_000).unwrap();
        assert!(board.configure_pin_sensors(&[sensor],1_000_000).is_err());
    }
}

#[cfg(test)]
mod rfid_wiring_tests {
    use super::*;
    #[test]
    fn rfid_and_resistive_touch_share_spi_but_reject_one_chip_select_in_both_orders() {
        let touch=super::super::resistive_touch::ResistiveConfig{model:1,id:0,pins:[4,5,6,7],irq:8,width:320,height:240,calibration:[0,4095,0,4095,1000,1800]};
        let reader=super::super::rfid::RfidConfig{id:0,sclk:4,mosi:5,miso:6,cs:7,reset:255};
        let mut board=CircuitBoard::new(&[],&[]).unwrap();
        board.configure_resistive_touches(&[touch],240_000_000).unwrap();
        assert!(board.configure_rfid(&[reader],240_000_000).is_err());
        board.configure_rfid(&[super::super::rfid::RfidConfig{cs:9,..reader}],240_000_000).unwrap();
        let pins=crate::SpiPins{sclk:1<<4,mosi:1<<5,miso:Some(6),cs:0};
        board.gpio_changes(&[(9,false)]);
        assert_eq!(board.spi_transfer_pins(2,pins,&[0xee,0],2),[0,0x92]);
        let mut board=CircuitBoard::new(&[],&[]).unwrap();
        board.configure_rfid(&[reader],240_000_000).unwrap();
        assert!(board.configure_resistive_touches(&[touch],240_000_000).is_err());
        board.configure_resistive_touches(&[super::super::resistive_touch::ResistiveConfig{pins:[4,5,6,9],..touch}],240_000_000).unwrap();
    }
}

#[cfg(test)]
mod lcd_wiring_tests {
    use super::*;
    #[test]
    fn parallel_lcd_and_sensor_ids_have_independent_namespaces() {
        let lcd=super::super::lcd::ParallelLcdConfig{id:0,rs:0,enable:1,data:[2,3,4,5],rw:255,columns:16,rows:2};
        let sensor=super::super::SensorConfig{id:0,sda:6,scl:7,address:0x76,model:1,shunt_milliohms:0};
        for lcd_first in [false,true] {
            let mut board=CircuitBoard::new(&[],&[]).unwrap();
            if lcd_first {board.configure_parallel_lcds(&[lcd],1_000_000).unwrap();}
            board.configure_sensors(&[sensor],1_000_000).unwrap();
            if !lcd_first {board.configure_parallel_lcds(&[lcd],1_000_000).unwrap();}
            assert_eq!(board.project_displays()[0].0,64);
            assert_eq!(board.sensors[0].lock().unwrap().config.id,0);
            assert!(board.i2c_devices().iter().any(|(_,address,device)|*address==0x76 && device.pins()==Some((6,7))));
        }
    }
    #[test]
    fn lcd_identity_routes_and_frame_pages_are_bounded() {
        let c=super::super::lcd::LcdConfig{id:0,sda:4,scl:5,address:0x27,columns:20,rows:4};
        let mut board=CircuitBoard::new(&[],&[]).unwrap();
        board.configure_lcds(&[c,super::super::lcd::LcdConfig{id:1,sda:6,scl:7,..c}],1_000_000).unwrap();
        let frames=board.project_displays();assert_eq!(frames.len(),2);
        assert_eq!((frames[0].0,frames[0].1,frames[0].2,frames[0].3.len()),(64,120,36,600));
        let devices=board.i2c_devices();assert_eq!(devices.len(),4);
        assert!(devices.iter().any(|(bus,a,d)|*bus==1 && *a==0x27 && d.pins()==Some((6,7))));
        assert!(board.configure_lcds(&[c,super::super::lcd::LcdConfig{id:1,..c}],1_000_000).is_err());
        assert_eq!(board.project_displays().len(),2);
        let mut board=CircuitBoard::new(&[],&[OledConfig{id:0,sda:4,scl:5,address:0x3c,width:128,height:64,controller:crate::devices::OledController::Ssd1306,column_offset:0}]).unwrap();
        assert!(board.configure_lcds(&[super::super::lcd::LcdConfig{address:0x3c,..c}],1_000_000).is_err());
    }
}

#[cfg(test)]
mod thermocouple_wiring_tests {
    use super::*;
    fn config(id:u8,cs:u8)->super::super::thermocouple::Config {
        super::super::thermocouple::Config{model:2,id,sclk:1,mosi:2,miso:3,cs}
    }
    #[test] fn thermocouple_shared_bus_distinct_selects_and_duplicate_identity() {
        let mut board=CircuitBoard::new(&[],&[]).unwrap();
        board.configure_thermocouples(&[config(0,4),config(1,5)],1_000_000).unwrap();
        assert!(board.configure_thermocouples(&[config(0,4),config(0,5)],1_000_000).is_err());
        assert!(board.configure_thermocouples(&[config(0,4),config(1,4)],1_000_000).is_err());
        assert!(board.configure_thermocouples(&[config(16,4)],1_000_000).is_err());
        board.gpio_drive(0,22,0);assert_eq!(board.input_levels(),vec![(3,false)]);
        board.gpio_drive(0,22,16);assert_eq!(board.input_levels(),vec![]);assert!(board.released_inputs().contains(&3));
    }
    #[test] fn thermocouple_rfid_chip_select_collision_both_orders() {
        let mut board=CircuitBoard::new(&[],&[]).unwrap();
        let reader=super::super::rfid::RfidConfig{id:0,sclk:1,mosi:2,miso:3,cs:4,reset:255};
        board.configure_thermocouples(&[config(0,4)],1_000_000).unwrap();assert!(board.configure_rfid(&[reader],1_000_000).is_err());
        board.configure_thermocouples(&[],1_000_000).unwrap();board.configure_rfid(&[reader],1_000_000).unwrap();assert!(board.configure_thermocouples(&[config(0,4)],1_000_000).is_err());
    }
    #[test] fn thermocouple_pin_sensor_input_conflict_both_orders() {
        let mut board=CircuitBoard::new(&[],&[]).unwrap();
        let sensor=super::super::pin_sensor::Config{model:2,id:0,pin:3,rom:[0;8]};
        board.configure_thermocouples(&[config(0,4)],1_000_000).unwrap();assert!(board.configure_pin_sensors(&[sensor],1_000_000).is_err());
        board.configure_thermocouples(&[],1_000_000).unwrap();board.configure_pin_sensors(&[sensor],1_000_000).unwrap();assert!(board.configure_thermocouples(&[config(0,4)],1_000_000).is_err());
    }
}
