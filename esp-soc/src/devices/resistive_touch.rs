use esp_periph::i2c::I2cDevice;
use std::sync::{Arc, Mutex};
use super::touch::TouchState;

#[derive(Clone, Copy, Debug)]
pub struct ResistiveConfig {
    pub model: u8, pub id: u8, pub pins: [u8; 4], pub irq: u8,
    pub width: u16, pub height: u16, pub calibration: [u16; 6],
}
impl ResistiveConfig {
    pub fn parse(b: &[u8]) -> Option<Self> {
        if b.len()!=24 || b[7]!=0 {return None;}
        let word=|i|u16::from_le_bytes([b[i],b[i+1]]);
        let c=Self{model:b[0],id:b[1],pins:[b[2],b[3],b[4],b[5]],irq:b[6],width:word(8),height:word(10),calibration:std::array::from_fn(|i|word(12+i*2))};
        c.valid().then_some(c)
    }
    pub fn gpio_pins(self)->Vec<u8> {
        let mut pins=if self.model==1 {self.pins.to_vec()}else{self.pins[..2].to_vec()};
        if self.irq!=255 {pins.push(self.irq);}pins
    }
    pub fn valid(self)->bool {
        let pins=self.gpio_pins();
        self.id<16 && matches!(self.model,1|2) && pins.iter().all(|p|*p<49)
            && pins.iter().enumerate().all(|(i,p)|!pins[..i].contains(p))
            && (self.model==1 || ((0x48..=0x4b).contains(&self.pins[2]) && self.pins[3]==255))
            && (1..=4096).contains(&self.width) && (1..=4096).contains(&self.height)
            && self.calibration.iter().all(|v|*v<=4095) && self.calibration[0]!=self.calibration[1] && self.calibration[2]!=self.calibration[3]
    }
}
pub struct ResistiveTouch {
    pub config: ResistiveConfig,
    state: TouchState,
    selected: bool,
    shift: u16,
    irq_enabled: bool,
    irq_disabled_high: bool,
    spi_remaining: u8,
    sampled_axes: u8,
    command: u8,
    conversion_pending: bool,
    ready_at: u64,
    result: u16,
    pending_result: u16,
    now: u64,
    hz: u64,
}
impl ResistiveTouch {
    pub fn new(config:ResistiveConfig,hz:u64)->Self {
        Self{config,state:TouchState{seen:true,..Default::default()},selected:false,shift:0,irq_enabled:true,irq_disabled_high:true,spi_remaining:0,sampled_axes:0,command:0,conversion_pending:false,ready_at:0,result:0,pending_result:0,now:0,hz}
    }
    pub fn input(&mut self,x:u16,y:u16,down:bool) {
        if down {self.state.x=x.min(self.config.width-1);self.state.y=y.min(self.config.height-1);self.state.down=true;self.state.seen=false;self.state.release_pending=false;}
        else if self.state.down && !self.state.seen {self.state.release_pending=true;}
        else {self.state.down=false;self.state.seen=false;self.state.release_pending=false;}
    }
    pub fn clear(&mut self) {self.state=TouchState{seen:true,..Default::default()};self.sampled_axes=0;}
    pub fn calibrate(&mut self,field:u32,value:u32)->bool {
        if field>=6 || value>4095 {return false;}
        let mut config=self.config;config.calibration[field as usize]=value as u16;
        if !config.valid() {return false;}self.config=config;true
    }
    pub fn irq_high(&self)->bool {if self.irq_enabled {!self.state.down}else{self.irq_disabled_high}}
    fn consume(&mut self) {
        if self.sampled_axes!=3 {return;}
        self.sampled_axes=0;self.state.seen=true;
        if self.state.release_pending {self.state.down=false;self.state.release_pending=false;self.state.seen=false;}
    }
    fn axis(&self,axis:usize)->u16 {
        if !self.state.down {return 4095;}
        let (value,size)=if axis==0 {(self.state.x,self.config.width)}else{(self.state.y,self.config.height)};
        let low=self.config.calibration[axis*2] as i64;let high=self.config.calibration[axis*2+1] as i64;
        ((low*size.saturating_sub(1).max(1) as i64+(high-low)*value as i64)/size.saturating_sub(1).max(1) as i64) as u16
    }
    fn sample(&mut self,channel:u8)->u16 {
        match channel {
            0=>{self.sampled_axes|=1;self.axis(0)},
            1=>{self.sampled_axes|=2;self.axis(1)},
            2=>if self.state.down {self.config.calibration[4]}else{0},
            3=>if self.state.down {self.config.calibration[5]}else{4095},
            _=>0,
        }
    }
    pub fn gpio(&mut self,pin:u8,level:bool) {
        if self.config.model==1 && pin==self.config.pins[3] {
            self.selected=!level;
            if level {self.shift=0;self.spi_remaining=0;if self.irq_enabled {self.consume();}}
        }
    }
    pub fn spi(&mut self,pins:crate::SpiPins,tx:&[u8],rx_len:usize)->Option<Vec<u8>> {
        let p=self.config.pins;
        if self.config.model!=1 || !(self.selected || pins.cs&(1u64<<p[3])!=0) || pins.sclk&(1u64<<p[0])==0 || pins.mosi&(1u64<<p[1])==0 {return None;}
        let mut rx=Vec::with_capacity(rx_len);
        for i in 0..tx.len().max(rx_len) {
            let byte=(self.shift>>8) as u8;self.shift<<=8;
            if self.spi_remaining>0 {self.spi_remaining-=1;if self.spi_remaining==0 {self.irq_enabled=self.command&1==0;}}
            if i<rx_len {rx.push(byte);}
            let command=tx.get(i).copied().unwrap_or(0);
            if command&0x80!=0 {
                let channel=match (command>>4)&7 {5=>0,1=>1,3=>2,4=>3,_=>255};
                let mut sample=self.sample(channel);
                if command&8!=0 {sample&=0xff0;}
                self.shift=sample<<3;self.command=command;self.spi_remaining=2;self.irq_enabled=false;self.irq_disabled_high=channel==255;
            }
        }
        if pins.cs&(1u64<<p[3])!=0 {self.shift=0;if self.irq_enabled {self.consume();}}
        if pins.miso!=Some(p[2]) {rx.fill(0xff);}
        Some(rx)
    }
    pub fn advance(&mut self,now:u64) {
        self.now=now;
        if self.ready_at!=0 && now>=self.ready_at {
            self.ready_at=0;self.result=self.pending_result;self.irq_enabled=self.command&4==0;
        }
    }
    fn conversion(&mut self) {
        if !self.conversion_pending {return;}
        self.conversion_pending=false;
        let channel=match self.command>>4 {12=>0,13=>1,14=>2,15=>3,_=>255};
        let mut result=self.sample(channel);
        if self.command&2!=0 {result&=0xff0;}
        self.pending_result=result<<4;
        let micros=if self.command&2!=0 {50}else{100};
        self.ready_at=self.now.saturating_add((self.hz*micros/1_000_000).max(1));
    }
}
pub struct Tsc2007 {pub device:Arc<Mutex<ResistiveTouch>>,byte:usize}
impl Tsc2007 {pub fn new(device:Arc<Mutex<ResistiveTouch>>)->Self {Self{device,byte:0}}}
impl I2cDevice for Tsc2007 {
    fn pins(&self)->Option<(u8,u8)> {let p=self.device.lock().unwrap().config.pins;Some((p[0],p[1]))}
    fn start(&mut self,read:bool)->bool {
        self.byte=0;let mut d=self.device.lock().unwrap();
        if read {d.conversion();}else{d.irq_enabled=false;d.irq_disabled_high=false;}true
    }
    fn write(&mut self,b:u8)->bool {
        let mut d=self.device.lock().unwrap();
        if !matches!(b>>4,0|2|4|8..=15) {return false;}
        d.command=b;
        if b>>4==11 {return b&12==0;}
        if matches!(b>>4,8..=10) {d.irq_enabled=false;return true;}
        d.conversion_pending=true;true
    }
    fn read(&mut self)->u8 {
        let mut d=self.device.lock().unwrap();let value=if self.byte&1==0 {(d.result>>8) as u8}else{d.result as u8};self.byte+=1;
        if self.byte==2 && d.ready_at==0 && d.command&4==0 {d.consume();}value
    }
    fn stop(&mut self) {self.device.lock().unwrap().conversion();}
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config(model:u8)->ResistiveConfig {ResistiveConfig{model,id:0,pins:if model==1 {[1,2,3,4]}else{[1,2,0x48,255]},irq:5,width:320,height:240,calibration:[0,4095,0,4095,1000,1800]}}
    fn pins()->crate::SpiPins {crate::SpiPins{sclk:2,mosi:4,miso:Some(3),cs:0}}
    #[test]
    fn xpt_pipeline_follows_commands_cs_and_physical_miso() {
        let mut d=ResistiveTouch::new(config(1),160_000_000);d.input(319,239,true);d.gpio(4,false);
        assert_eq!(d.spi(pins(),&[0xb1,0,0xc1,0,0xd1,0,0],7).unwrap(),[0,31,64,56,64,127,248]);
        assert!(!d.irq_high());d.spi(pins(),&[0xd0,0,0],3);assert!(!d.irq_high());
        assert_eq!(d.spi(crate::SpiPins{miso:Some(6),..pins()},&[0x91],1).unwrap(),[255]);
        d.gpio(4,true);assert!(d.spi(pins(),&[0x91],1).is_none());
    }
    #[test]
    fn tsc_conversion_has_latency_resolution_and_power_irq_control() {
        let device=Arc::new(Mutex::new(ResistiveTouch::new(config(2),1_000_000)));
        let mut t=Tsc2007::new(device.clone());device.lock().unwrap().input(319,239,true);
        assert!(!device.lock().unwrap().irq_high());t.start(false);assert!(!device.lock().unwrap().irq_high());t.write(0xc4);t.stop();
        device.lock().unwrap().advance(99);t.start(true);assert_eq!([t.read(),t.read()],[0,0]);
        device.lock().unwrap().advance(100);t.start(true);assert_eq!([t.read(),t.read()],[255,240]);assert!(!device.lock().unwrap().irq_high());
        t.start(false);t.write(0xd2);t.stop();device.lock().unwrap().advance(150);t.start(true);assert_eq!([t.read(),t.read()],[255,0]);
        assert!(!device.lock().unwrap().irq_high());assert!(!t.write(0x10));
    }
    #[test]
    fn calibration_is_bounded_reversible_and_keeps_physical_source() {
        let mut d=ResistiveTouch::new(config(1),160_000_000);d.input(0,239,true);
        assert!(d.calibrate(0,3900));assert!(d.calibrate(1,200));assert_eq!(d.sample(0),3900);assert_eq!(d.sample(1),4095);
        assert!(!d.calibrate(1,3900));assert!(!d.calibrate(3,4096));assert!(!d.calibrate(6,0));
        d.input(319,0,true);assert_eq!(d.sample(0),200);assert_eq!(d.sample(1),0);
        d.input(319,0,false);d.sample(0);d.sample(1);d.consume();assert!(d.irq_high());
    }
    #[test]
    fn disabled_pen_irq_levels_and_short_press_survive_conversion_boundaries() {
        let mut d=ResistiveTouch::new(config(1),1_000_000);d.gpio(4,false);
        assert!(d.irq_high());d.spi(pins(),&[0xd1],1);assert!(!d.irq_high());
        d.spi(pins(),&[0,0],2);assert!(!d.irq_high());
        d.spi(pins(),&[0xd0],1);assert!(!d.irq_high());d.spi(pins(),&[0,0],2);assert!(d.irq_high());
        d.input(100,80,true);d.input(100,80,false);assert!(!d.irq_high());
        d.spi(pins(),&[0x91,0,0xd0,0,0],5);assert!(!d.irq_high());
        d.gpio(4,true);assert!(d.irq_high());
    }

}
