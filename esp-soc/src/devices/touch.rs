use esp_periph::i2c::I2cDevice;
/// Touch state shared between the board (UI) and the GT911 model.
#[derive(Default, Debug, Clone, Copy)]
pub struct TouchState { pub down: bool, pub x: u16, pub y: u16, pub seen: bool, pub release_pending: bool }

/// Goodix GT911 capacitive touch controller: 16-bit register addresses; product ID at 0x8140,
/// config at 0x8047.., status + up to 5 points at 0x814E...
pub struct Gt911 { addr: u16, phase: u8, touch: std::sync::Arc<std::sync::Mutex<TouchState>>, pub reads: u64, w: u16, h: u16 }
impl Gt911 {
    pub fn new(touch: std::sync::Arc<std::sync::Mutex<TouchState>>, w: u16, h: u16) -> Self { Gt911 { addr: 0, phase: 0, touch, reads: 0, w, h } }
    fn reg(&self, a: u16) -> u8 {
        let mut tl = self.touch.lock().unwrap();
        if a == 0x814e {
            // like the real controller's buffer: a touch stays readable until the host has seen it once
            if tl.release_pending && tl.seen { tl.down = false; tl.release_pending = false; }
            if tl.down { tl.seen = true; }
        }
        let t = *tl;
        match a {
            0x8140 => b'9', 0x8141 => b'1', 0x8142 => b'1', 0x8143 => 0, 0x8144 => 0x60, 0x8145 => 0x10,      // "911", firmware 0x1060
            0x8047 => 0x41,                                                                                     // config version
            0x8048 => self.w as u8, 0x8049 => (self.w >> 8) as u8, 0x804a => self.h as u8, 0x804b => (self.h >> 8) as u8,
            0x804c => 5,                                                                                        // touch number
            0x814e => 0x80 | t.down as u8,                                                                      // buffer ready + count
            0x814f => 0, 0x8150 => t.x as u8, 0x8151 => (t.x >> 8) as u8, 0x8152 => t.y as u8, 0x8153 => (t.y >> 8) as u8, 0x8154 => 20, 0x8155 => 0, 0x8156 => 0,
            _ => 0,
        }
    }
}
impl I2cDevice for Gt911 {
    fn start(&mut self, read: bool) -> bool { if !read { self.phase = 0; } true }
    fn write(&mut self, b: u8) -> bool { match self.phase { 0 => { self.addr = (b as u16) << 8; self.phase = 1; } 1 => { self.addr |= b as u16; self.phase = 2; } _ => { self.addr = self.addr.wrapping_add(1); } } true }
    fn read(&mut self) -> u8 { let v = self.reg(self.addr); self.addr = self.addr.wrapping_add(1); self.reads += 1; v }
}

/// Hynitron CST820 touch controller used on the Waveshare Touch AMOLED 1.8 V2.
/// The register report matches the CST816S-compatible driver used by its board support package.
pub struct Cst820 { ptr: u8, first: bool, touch: std::sync::Arc<std::sync::Mutex<TouchState>>, pub reads: u64 }
impl Cst820 {
    pub fn new(touch: std::sync::Arc<std::sync::Mutex<TouchState>>) -> Self { Cst820 { ptr: 0, first: true, touch, reads: 0 } }
    fn reg(&self, addr: u8) -> u8 {
        let mut touch = self.touch.lock().expect("CST820 touch state mutex poisoned");
        if addr == 0x02 {
            if touch.release_pending && touch.seen { touch.down = false; touch.release_pending = false; }
            if touch.down { touch.seen = true; }
        }
        let touch = *touch;
        match addr {
            0x01 => 0,
            0x02 => u8::from(touch.down),
            0x03 => ((touch.x >> 8) as u8) & 0x0f,
            0x04 => touch.x as u8,
            0x05 => ((touch.y >> 8) as u8) & 0x0f,
            0x06 => touch.y as u8,
            0xa7 => 0xb7,
            0xa8 => 0x41,
            0xa9 => 0x02,
            _ => 0,
        }
    }
}
impl I2cDevice for Cst820 {
    fn start(&mut self, read: bool) -> bool { if !read { self.first = true; } true }
    fn write(&mut self, byte: u8) -> bool {
        if self.first { self.ptr = byte; self.first = false; } else { self.ptr = self.ptr.wrapping_add(1); }
        true
    }
    fn read(&mut self) -> u8 { let value = self.reg(self.ptr); self.ptr = self.ptr.wrapping_add(1); self.reads += 1; value }
}


#[derive(Clone, Copy, Debug)]
pub struct TouchConfig {
    pub model:u8, pub id:u8, pub sda:u8, pub scl:u8, pub address:u8,
    pub irq:u8, pub reset:u8, pub report_hz:u8, pub width:u16, pub height:u16,
}
impl TouchConfig {
    pub fn parse(b:&[u8])->Option<Self> {
        if b.len()!=12 {return None;}
        let c=Self {model:b[0],id:b[1],sda:b[2],scl:b[3],address:b[4],irq:b[5],reset:b[6],report_hz:b[7],
            width:u16::from_le_bytes([b[8],b[9]]),height:u16::from_le_bytes([b[10],b[11]])};
        c.valid().then_some(c)
    }
    pub fn valid(self)->bool {
        let pins=[self.sda,self.scl,self.irq,self.reset];
        self.id<16 && (self.report_hz==0 || self.model==5 && self.report_hz<=120) && self.sda<49 && self.scl<49 && (self.irq<49||self.irq==255) && (self.reset<49||self.reset==255)
            && pins.iter().enumerate().all(|(i,p)|*p==255 || !pins[..i].contains(p))
            && (1..=4096).contains(&self.width) && (1..=4096).contains(&self.height)
            && match self.model {1=>matches!(self.address,0x14|0x5d),2..=4=>self.address==0x15,5=>self.address==0x63,_=>false}
    }
}
enum Packet { Gt(Gt911), Cap(Cst820) }
pub struct TouchController {
    pub config:TouchConfig,
    packet:Packet,
    state:std::sync::Arc<std::sync::Mutex<TouchState>>,
    regs:[u8;256],
    gt_config:[u8;184],
    pub irq_high:bool,
    reset_high:bool,
    strap_high:bool,
    pub address:u8,
    asleep:bool,
    hz:u64,
    now:u64,
    next_scan:u64,
    pulse_end:u64,
    pulse_pending:bool,
}
impl TouchController {
    pub fn new(config:TouchConfig,hz:u64)->Self {
        let state=std::sync::Arc::new(std::sync::Mutex::new(TouchState{seen:true,..Default::default()}));
        let packet=if config.model==1 {Packet::Gt(Gt911::new(state.clone(),config.width,config.height))} else {Packet::Cap(Cst820::new(state.clone()))};
        let mut t=Self {config,packet,state,regs:[0;256],gt_config:[0;184],irq_high:true,reset_high:true,strap_high:config.address==0x14,address:config.address,asleep:false,hz,now:0,next_scan:0,pulse_end:0,pulse_pending:false};
        t.reset_registers();t
    }
    fn reset_registers(&mut self) {
        self.regs=[0;256];self.asleep=false;
        if self.config.model==1 {
            self.regs[0]=0x41;self.regs[1..3].copy_from_slice(&self.config.width.to_le_bytes());
            self.regs[3..5].copy_from_slice(&self.config.height.to_le_bytes());
            self.regs[5]=5;self.regs[6]=1;self.regs[15]=5;
            self.gt_config.copy_from_slice(&self.regs[..184]);
            self.regs[184]=0u8.wrapping_sub(self.regs[..184].iter().fold(0u8,|sum,b|sum.wrapping_add(*b)));
        } else {self.regs[0xa8]=0x41;self.regs[0xa9]=2;self.regs[0xed]=10;self.regs[0xee]=1;self.regs[0xfa]=0x60;}
        self.next_scan=self.now.saturating_add(self.hz/100);
    }
    pub fn input(&mut self,x:u16,y:u16,down:bool) {
        self.pulse_pending=self.config.model==1 || self.config.model==5 || self.regs[0xfa]&0x20!=0;
        let mut t=self.state.lock().unwrap();
        if down {t.x=x.min(self.config.width-1);t.y=y.min(self.config.height-1);t.down=true;t.seen=false;t.release_pending=false;}
        else if t.down && !t.seen {t.release_pending=true;}
        else {t.down=false;t.seen=false;t.release_pending=false;}
    }
    pub fn set_report_hz(&mut self,hz:u32)->bool {
        if self.config.model!=5 || !(1..=120).contains(&hz) {return false;}
        self.config.report_hz=hz as u8;self.next_scan=self.now.saturating_add(self.hz/hz as u64);true
    }
    pub fn clear(&mut self) { *self.state.lock().unwrap()=TouchState{seen:true,..Default::default()};self.irq_high=true;self.pulse_end=0;self.pulse_pending=false; }
    pub fn gpio(&mut self,pin:u8,level:bool) {
        if pin==self.config.irq {self.strap_high=level;}
        if pin==self.config.reset && level!=self.reset_high {
            self.reset_high=level;
            if level {
                if self.config.model==1 {self.address=if self.strap_high {0x14}else{0x5d};}
                self.reset_registers();
                let mut t=self.state.lock().unwrap();t.seen=!t.down;self.pulse_pending=t.down;
            } else {self.irq_high=true;self.pulse_end=0;self.pulse_pending=false;}
        }
    }
    pub fn advance(&mut self,now:u64) {
        self.now=now;
        if !self.reset_high || self.asleep {self.irq_high=true;return;}
        let mut t=self.state.lock().unwrap();
        if self.config.model!=1 {
            if self.pulse_end!=0 && now>=self.pulse_end {self.irq_high=true;self.pulse_end=0;}
            if t.release_pending && t.seen && self.irq_high {t.down=false;t.release_pending=false;t.seen=false;self.pulse_pending=true;}
            if now>=self.next_scan {
                let period=if self.config.model==5 {self.hz/if self.config.report_hz==0 {120}else{self.config.report_hz as u64}}else{self.hz*self.regs[0xee].clamp(1,30) as u64/100};
                self.next_scan=now.saturating_add(period.max(1));
                if self.config.model==5 {if t.down || !t.seen {self.pulse_pending=true;}}
                else if self.regs[0xfa]&0x80!=0 || (t.down && self.regs[0xfa]&0x40!=0) {self.pulse_pending=true;}
            }
            if self.config.model!=5 && self.regs[0xfa]&0xe0==0 {self.pulse_pending=false;}
            if self.irq_high && self.pulse_pending {
                self.irq_high=false;self.pulse_pending=false;
                let width=if self.config.model==5 {self.hz/1000}else{self.hz*self.regs[0xed].clamp(1,200) as u64/10_000};
                self.pulse_end=now.saturating_add(width.max(1));
            }
            return;
        }
        let mode=self.gt_config[6]&3;
        let active=mode==0 || mode==3;
        let period=(self.hz*(5+(self.gt_config[15]&15) as u64)/1000).max(1);
        if t.release_pending && t.seen {t.down=false;t.release_pending=false;t.seen=false;self.pulse_pending=true;}
        if mode>=2 {
            if now>=self.next_scan {self.next_scan=now.saturating_add(period);if t.down {t.seen=false;}}
            self.irq_high=if t.seen {!active}else{active};return;
        }
        if self.pulse_end!=0 {
            if now<self.pulse_end {return;}
            self.pulse_end=0;self.irq_high=!active;
            self.next_scan=now.saturating_add(1);return;
        }
        self.irq_high=!active;
        if self.pulse_pending || now>=self.next_scan {
            self.next_scan=now.saturating_add(period);
            if t.down {t.seen=false;}
            if !t.seen {self.irq_high=active;self.pulse_end=now.saturating_add(period);}
            self.pulse_pending=false;
        }
    }

    fn register(&mut self,address:u16)->u8 {
        let state=*self.state.lock().unwrap();
        if self.config.model==1 {
            if address==0x814e {return if state.seen {0}else{0x80|u8::from(state.down)};}
            if (0x8047..=0x8100).contains(&address) {return self.regs[(address-0x8047) as usize];}
            if (0x8150..=0x8153).contains(&address) {
                let scale=|value:u16,size:u16,configured:u16|((value as u32*configured.saturating_sub(1) as u32)/size.saturating_sub(1).max(1) as u32) as u16;
                let x=scale(state.x,self.config.width,u16::from_le_bytes([self.gt_config[1],self.gt_config[2]]));
                let y=scale(state.y,self.config.height,u16::from_le_bytes([self.gt_config[3],self.gt_config[4]]));
                return [x as u8,(x>>8) as u8,y as u8,(y>>8) as u8][(address-0x8150) as usize];
            }
            if let Packet::Gt(gt)=&self.packet {return gt.reg(address);}
        } else {
            if address==2 {return u8::from(state.down);}
            if self.config.model!=5 && address==0xa7 {return match self.config.model {2=>0xb4,3=>0xb5,_=>0xb7};}
            if (3..=6).contains(&address) {if let Packet::Cap(cap)=&self.packet {return cap.reg(address as u8);}}
            return self.regs[address as u8 as usize];
        }
        0
    }
    fn write_register(&mut self,address:u16,value:u8) {
        if self.config.model==1 {
            if address==0x814e && value==0 {self.state.lock().unwrap().seen=true;}
            if address==0x8040 && value==5 {self.asleep=true;}
            if (0x8047..=0x8100).contains(&address) {self.regs[(address-0x8047) as usize]=value;
                if address==0x8100 && value==1 {
                    if self.regs[..185].iter().fold(0u8,|sum,b|sum.wrapping_add(*b))==0 {self.gt_config.copy_from_slice(&self.regs[..184]);}
                    self.regs[185]=0;
                }
            }
        } else if self.config.model!=5 {
            if address==0xe5 && value==3 {self.asleep=true;}
            if matches!(address,0xe5|0xec|0xed|0xee|0xfa|0xfe) {self.regs[address as usize]=value;}
        }
    }
}
pub struct TouchI2c { pub controller:std::sync::Arc<std::sync::Mutex<TouchController>>, pointer:u16, phase:u8 }
impl TouchI2c {pub fn new(controller:std::sync::Arc<std::sync::Mutex<TouchController>>)->Self {Self{controller,pointer:0,phase:0}}}
impl I2cDevice for TouchI2c {
    fn pins(&self)->Option<(u8,u8)> {let c=self.controller.lock().unwrap().config;Some((c.sda,c.scl))}
    fn address(&self,_configured:u8)->u8 {self.controller.lock().unwrap().address}
    fn start(&mut self,read:bool)->bool {if !read {self.phase=0;}let c=self.controller.lock().unwrap();c.reset_high&&!c.asleep}
    fn write(&mut self,b:u8)->bool {
        let mut c=self.controller.lock().unwrap();
        match (self.phase,c.config.model) {
            (0,1)=>{self.pointer=(b as u16)<<8;self.phase=1;},
            (1,1)=>{self.pointer|=b as u16;self.phase=2;},
            (0,_)=>{self.pointer=b as u16;self.phase=2;},
            _=>{c.write_register(self.pointer,b);self.pointer=self.pointer.wrapping_add(1);if c.config.model!=1 {self.pointer&=0xff;}}
        }true
    }
    fn read(&mut self)->u8 {
        let mut c=self.controller.lock().unwrap();let byte=c.register(self.pointer);
        if c.config.model!=1 && self.pointer==6 {c.state.lock().unwrap().seen=true;}
        self.pointer=self.pointer.wrapping_add(1);if c.config.model!=1 {self.pointer&=0xff;}byte
    }
}

#[cfg(test)]
mod project_tests {
    use super::*;
    fn config(model:u8)->TouchConfig {TouchConfig{model,id:0,sda:4,scl:5,address:if model==1{0x5d}else if model==5{0x63}else{0x15},irq:6,reset:7,report_hz:0,width:320,height:240}}
    fn device(model:u8)->TouchI2c {TouchI2c::new(std::sync::Arc::new(std::sync::Mutex::new(TouchController::new(config(model),160_000_000))))}
    fn read(d:&mut TouchI2c,address:u16,n:usize)->Vec<u8> {
        assert!(d.start(false));if d.controller.lock().unwrap().config.model==1 {d.write((address>>8) as u8);}d.write(address as u8);assert!(d.start(true));(0..n).map(|_|d.read()).collect()
    }
    fn write(d:&mut TouchI2c,address:u16,b:u8) {assert!(d.start(false));if d.controller.lock().unwrap().config.model==1 {d.write((address>>8) as u8);}d.write(address as u8);d.write(b);}
    #[test]
    fn distinct_cst_ids_and_official_axs_packet_share_coordinates_without_aliasing() {
        for (model,id) in [(2,0xb4),(3,0xb5),(4,0xb7),(5,0)] {
            let mut d=device(model);assert_eq!(read(&mut d,0xa7,1)[0],id);
            {let mut c=d.controller.lock().unwrap();c.input(123,67,true);c.advance(1);assert!(!c.irq_high);}
            let packet=read(&mut d,1,14);assert_eq!(&packet[..6],&[0,1,0,123,0,67]);
            d.controller.lock().unwrap().advance(200_000);
            assert_eq!(read(&mut d,2,5),[1,0,123,0,67]);
        }
    }
    #[test]
    fn fast_tap_preserves_down_and_release_reports_until_the_bus_consumes_them() {
        for model in [1,2,3,4,5] {
            let mut d=device(model);
            {let mut c=d.controller.lock().unwrap();c.input(30,40,true);c.input(30,40,false);c.advance(1);}
            if model==1 {assert_eq!(read(&mut d,0x814e,1),[0x81]);assert_eq!(read(&mut d,0x8150,4),[30,0,40,0]);write(&mut d,0x814e,0);}
            else {assert_eq!(read(&mut d,2,5),[1,0,30,0,40]);}
            d.controller.lock().unwrap().advance(if model==1 {1_600_001}else{200_000});
            d.controller.lock().unwrap().advance(if model==1 {1_600_002}else{200_001});assert!(!d.controller.lock().unwrap().irq_high);
            assert_eq!(read(&mut d,if model==1{0x814e}else{2},1),[if model==1{0x80}else{0}]);
            d.controller.lock().unwrap().clear();assert!(d.controller.lock().unwrap().irq_high);
        }
    }
    #[test]
    fn gt911_acknowledgement_config_and_reset_address_strap_are_hardware_state() {
        let mut d=device(1);assert_eq!(read(&mut d,0x8140,4),b"911\0");
        d.controller.lock().unwrap().input(10,20,true);assert_eq!(read(&mut d,0x814e,1),[0x81]);
        write(&mut d,0x814e,0);assert_eq!(read(&mut d,0x814e,1),[0]);
        write(&mut d,0x8048,100);assert_eq!(read(&mut d,0x8048,1),[100]);
        {let mut c=d.controller.lock().unwrap();c.gpio(7,false);c.gpio(6,true);}
        assert!(!d.start(false));d.controller.lock().unwrap().gpio(7,true);
        assert_eq!(d.address(0x5d),0x14);assert_eq!(read(&mut d,0x8048,2),320u16.to_le_bytes());
    }
    #[test]
    fn cst_sleep_requires_controller_reset_and_bounds_reject_invalid_models() {
        let mut d=device(3);write(&mut d,0xe5,3);assert!(!d.start(false));
        {let mut c=d.controller.lock().unwrap();c.gpio(7,false);c.gpio(7,true);}
        assert!(d.start(false));assert!(!TouchConfig{address:0x3b,..config(5)}.valid());
        assert!(!TouchConfig{irq:4,..config(2)}.valid());assert!(!TouchConfig{width:0,..config(1)}.valid());
    }
    #[test]
    fn axs_report_calibration_changes_cadence_without_losing_held_input() {
        let mut d=device(5);
        {let mut c=d.controller.lock().unwrap();c.input(123,67,true);c.advance(1);assert!(!c.irq_high);}
        assert_eq!(read(&mut d,2,5),[1,0,123,0,67]);
        {let mut c=d.controller.lock().unwrap();c.advance(160_001);assert!(c.irq_high);
            assert!(c.set_report_hz(20));assert!(!c.set_report_hz(0));assert!(!c.set_report_hz(121));
            c.advance(8_160_000);assert!(c.irq_high);c.advance(8_160_001);assert!(!c.irq_high);
            c.gpio(7,false);c.gpio(7,true);assert_eq!(c.config.report_hz,20);}
        assert_eq!(read(&mut d,2,5),[1,0,123,0,67]);
        assert!(!device(2).controller.lock().unwrap().set_report_hz(20));
        assert!(!TouchConfig{report_hz:121,..config(5)}.valid());
        assert!(!TouchConfig{report_hz:20,..config(2)}.valid());
    }
    #[test]
    fn cst_irq_width_and_scan_registers_control_real_pulses_without_read_ack() {
        let mut d=device(2);write(&mut d,0xed,20);write(&mut d,0xee,2);
        {let mut c=d.controller.lock().unwrap();c.input(1,2,true);c.advance(1);assert!(!c.irq_high);}
        read(&mut d,2,5);
        {let mut c=d.controller.lock().unwrap();c.advance(320_000);assert!(!c.irq_high);
            c.advance(320_001);assert!(c.irq_high);c.advance(1_600_000);assert!(!c.irq_high);
            c.advance(1_920_000);assert!(c.irq_high);c.advance(4_799_999);assert!(c.irq_high);
            c.advance(4_800_000);assert!(!c.irq_high);}
        write(&mut d,0xfa,0);
        {let mut c=d.controller.lock().unwrap();c.advance(5_120_000);assert!(c.irq_high);
            c.input(3,4,true);c.advance(10_000_000);assert!(c.irq_high);}
        assert_eq!(read(&mut d,0xff,4),[0,0,0,1]);
    }
    #[test]
    fn gt_resolution_registers_scale_physical_glass_coordinates() {
        let mut d=device(1);
        write(&mut d,0x8048,160);write(&mut d,0x8049,0);
        write(&mut d,0x804a,120);write(&mut d,0x804b,0);
        d.controller.lock().unwrap().input(319,239,true);
        assert_eq!(read(&mut d,0x8150,4),[63,1,239,0]);
        write(&mut d,0x8100,1);
        assert_eq!(read(&mut d,0x8150,4),[63,1,239,0]);
        let checksum=0u8.wrapping_sub(read(&mut d,0x8047,184).iter().fold(0u8,|sum,b|sum.wrapping_add(*b)));
        write(&mut d,0x80ff,checksum);write(&mut d,0x8100,1);
        assert_eq!(read(&mut d,0x8150,4),[159,0,119,0]);
        assert_eq!(d.controller.lock().unwrap().config.width,320);
    }

    #[test]
    fn gt_irq_period_survives_status_ack_and_repeats_for_held_input() {
        let mut d=device(1);
        {let mut c=d.controller.lock().unwrap();c.input(10,20,true);c.advance(1);assert!(!c.irq_high);}
        write(&mut d,0x814e,0);
        {let mut c=d.controller.lock().unwrap();c.advance(2);assert!(!c.irq_high);
            c.advance(1_600_001);assert!(c.irq_high);c.advance(1_600_002);assert!(!c.irq_high);}
        assert_eq!(read(&mut d,0x814e,1),[0x81]);
    }

}
