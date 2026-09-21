use std::{collections::HashMap, sync::{Arc, Mutex}};
use esp_periph::i2c::I2cDevice;
/// What the board needs to know about the sensor's configuration (written over SCCB).
#[derive(Default, Debug)]
pub struct SensorState { pub width: u32, pub height: u32, pub format: u8, pub streaming: bool, pub jpeg: bool }

/// OV5640 image sensor over SCCB: 16-bit register addresses, auto-increment.
pub struct Ov5640 { pub regs: HashMap<u16, u8>, addr: u16, phase: u8, pub writes: u64, state: std::sync::Arc<std::sync::Mutex<SensorState>> }
impl Ov5640 {
    pub fn new(state: std::sync::Arc<std::sync::Mutex<SensorState>>) -> Self {
        let mut regs = HashMap::new();
        regs.insert(0x300a, 0x56); regs.insert(0x300b, 0x40);   // chip ID 0x5640
        regs.insert(0x3008, 0x02);                              // system control: normal
        regs.insert(0x302a, 0xb0);                              // silicon revision
        Ov5640 { regs, addr: 0, phase: 0, writes: 0, state }
    }
    pub fn get(&self, r: u16) -> u8 { *self.regs.get(&r).unwrap_or(&0) }
    fn sync_state(&self) {
        let mut st = self.state.lock().unwrap();
        st.width = ((self.get(0x3808) as u32 & 0xf) << 8) | self.get(0x3809) as u32;    // DVP output width
        st.height = ((self.get(0x380a) as u32 & 0x7) << 8) | self.get(0x380b) as u32;   // DVP output height
        st.format = self.get(0x4300);
        st.jpeg = self.get(0x3821) & 0x20 != 0;
        st.streaming = self.get(0x3008) & 0x40 == 0;
    }
}
impl I2cDevice for Ov5640 {
    fn start(&mut self, read: bool) -> bool { if !read { self.phase = 0; } true }
    fn write(&mut self, b: u8) -> bool {
        match self.phase {
            0 => { self.addr = (b as u16) << 8; self.phase = 1; }
            1 => { self.addr |= b as u16; self.phase = 2; }
            _ => { let v = if self.addr == 0x3008 { b & !0x80 } else { b }; if self.addr == 0x3008 && b & 0x80 != 0 { self.regs.retain(|r,_| [0x300a,0x300b,0x302a].contains(r)); } self.regs.insert(self.addr, v); if (0x3808..=0x380b).contains(&self.addr) || self.addr == 0x3821 || self.addr == 0x4300 || self.addr == 0x3008 { self.sync_state(); } self.addr = self.addr.wrapping_add(1); self.writes += 1; }
        }
        true
    }
    fn read(&mut self) -> u8 { let v = self.get(self.addr); self.addr = self.addr.wrapping_add(1); v }
}

#[derive(Clone, Copy, Debug)]
pub struct CameraConfig { pub sensor: u16, pub id: u8, pub pins: [u8; 16], pub fps: u8 }
impl CameraConfig {
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != 20 { return None; }
        let sensor = u16::from_le_bytes([bytes[0], bytes[1]]);
        if ![0x5640, 0x26].contains(&sensor) || !(1..=30).contains(&bytes[19]) { return None; }
        let pins: [u8; 16] = bytes[3..19].try_into().ok()?;
        let mut seen = 0u64;
        for (i, pin) in pins.iter().enumerate() {
            if i >= 14 && *pin == 255 { continue; }
            if *pin >= 49 || seen & (1 << pin) != 0 { return None; }
            seen |= 1 << pin;
        }
        Some(Self { sensor, id: bytes[2], pins, fps: bytes[19] })
    }
}

pub struct Ov2640 { regs: [[u8; 256]; 2], bank: usize, addr: u8, first: bool, state: Arc<Mutex<SensorState>> }
impl Ov2640 {
    pub fn new(state: Arc<Mutex<SensorState>>) -> Self {
        let mut sensor = Self { regs: [[0;256];2], bank: 1, addr: 0, first: true, state };
        sensor.reset(); sensor
    }
    fn reset(&mut self) {
        self.regs = [[0;256];2];
        self.regs[1][0x0a] = 0x26; self.regs[1][0x0b] = 0x42;
        self.regs[1][0x1c] = 0x7f; self.regs[1][0x1d] = 0xa2;
        *self.state.lock().unwrap() = SensorState::default();
    }
    fn sync(&self) {
        let r = &self.regs[0]; let mut state = self.state.lock().unwrap();
        state.width = (r[0x5a] as u32 | ((r[0x5c] as u32 & 3) << 8)) * 4;
        state.height = (r[0x5b] as u32 | ((r[0x5c] as u32 & 4) << 6)) * 4;
        state.format = r[0xda];
        state.streaming = r[0xe0] & 0x14 == 0 && self.regs[1][9] & 0x10 == 0;
    }
}
impl I2cDevice for Ov2640 {
    fn start(&mut self, read: bool) -> bool { if !read { self.first = true; } true }
    fn write(&mut self, value: u8) -> bool {
        if self.first { self.addr = value; self.first = false; }
        else if self.addr == 0xff { self.bank = (value & 1) as usize; }
        else if self.bank == 1 && self.addr == 0x12 && value & 0x80 != 0 { self.reset(); }
        else { self.regs[self.bank][self.addr as usize] = value; self.sync(); }
        true
    }
    fn read(&mut self) -> u8 { if self.addr == 0xff { self.bank as u8 } else { self.regs[self.bank][self.addr as usize] } }
}

pub struct Camera {
    pub config: CameraConfig,
    pub state: Arc<Mutex<SensorState>>,
    sensor: Box<dyn I2cDevice>,
    controls: [bool;2],
    frame: Option<(u32, u32, u32, Arc<Vec<u8>>)>,
}
impl Camera {
    pub fn new(config: CameraConfig) -> Self {
        let state = Arc::new(Mutex::new(SensorState::default()));
        let sensor: Box<dyn I2cDevice> = if config.sensor == 0x26 { Box::new(Ov2640::new(state.clone())) } else { Box::new(Ov5640::new(state.clone())) };
        Self { config, state, sensor, controls: [false,true], frame: None }
    }
    pub fn gpio(&mut self, pin: u8, level: bool) {
        for i in 0..2 {
            if pin == self.config.pins[14+i] {
                if i == 1 && !level && self.controls[i] {
                    self.sensor = if self.config.sensor == 0x26 { Box::new(Ov2640::new(self.state.clone())) } else { Box::new(Ov5640::new(self.state.clone())) };
                    *self.state.lock().unwrap() = SensorState::default();
                }
                self.controls[i] = level;
            }
        }
    }
    fn powered(&self) -> bool { !self.controls[0] && self.controls[1] }
    pub fn format(&self) -> u32 {
        let f = self.state.lock().unwrap().format;
        if self.config.sensor == 0x26 { if f & 0x10 != 0 { 3 } else if f & 0x40 != 0 { 2 } else if f & 0x0c == 8 { 0 } else if f & 0x0c == 0 { 1 } else { u32::MAX } }
        else if self.state.lock().unwrap().jpeg { 3 } else { match f { 0x61 => 0, 0x30 => 1, 0x10 => 2, _ => u32::MAX } }
    }
    pub fn info(&self, field: u32) -> u32 {
        if field == 2 { return self.format(); }
        if field == 3 && self.format() == u32::MAX { return 0; }
        let s = self.state.lock().unwrap();
        match field { 0 => s.width, 1 => s.height, 3 => u32::from(self.powered() && s.streaming && s.width != 0 && s.height != 0), _ => 0 }
    }
    pub fn reset_frame(&mut self) { self.frame = None; }
    pub fn push(&mut self, w: u32, h: u32, format: u32, bytes: &[u8]) -> bool {
        if w == 0 || h == 0 || w > 1600 || h > 1200 || bytes.len() > 5_760_000 { return false; }
        let pixels = w as usize * h as usize;
        let valid = match format { 0 => bytes.len() == pixels * 2, 1 => w % 2 == 0 && bytes.len() == pixels * 2, 2 => bytes.len() == pixels, 3 => jpeg_size(bytes) == Some((w,h)), 4 => bytes.len() == pixels * 3, _ => false };
        if !valid { return false; }
        self.frame = Some((w,h,format,Arc::new(bytes.to_vec()))); true
    }
    pub fn frame(&self) -> Option<(u32,u32,Arc<Vec<u8>>)> {
        let (w,h,format,bytes) = self.frame.as_ref()?;
        let target = self.format(); let s = self.state.lock().unwrap();
        if !self.powered() || !s.streaming || (*w,*h) != (s.width,s.height) { return None; }
        if target == u32::MAX { return None; }
        if *format == 2 && target == 2 { return Some((*w,*h,bytes.clone())); }
        if *format == 3 { return (target == 3).then(|| (*w,*h,bytes.clone())); }
        if target == 3 { return None; }
        if *format == 1 && target == 1 { return Some((*w,*h,bytes.clone())); }
        let rgb = match *format {
            0 => bytes.chunks_exact(2).flat_map(|p| { let v=u16::from_le_bytes([p[0],p[1]]); [((v>>11)*255/31) as u8, (((v>>5)&63)*255/63) as u8, ((v&31)*255/31) as u8] }).collect(),
            2 => bytes.iter().flat_map(|v| [*v,*v,*v]).collect(), 4 => bytes.as_ref().clone(), _ => return None,
        };
        let picture = crate::picture::Picture { w:*w,h:*h,rgb };
        let output = if target == 0 { picture.rgb.chunks_exact(3).flat_map(|p| (((p[0] as u16 >> 3)<<11)|((p[1] as u16 >> 2)<<5)|(p[2] as u16 >> 3)).to_be_bytes()).collect() }
            else if target == 2 { picture.rgb.chunks_exact(3).map(|p| ((77*p[0] as u32+150*p[1] as u32+29*p[2] as u32+128)>>8) as u8).collect() }
            else { crate::picture::to_yuyv(&picture,*w,*h) };
        Some((*w,*h,Arc::new(output)))
    }
}
pub struct CameraI2c(pub Arc<Mutex<Camera>>);
impl I2cDevice for CameraI2c {
    fn pins(&self) -> Option<(u8,u8)> { let c=self.0.lock().unwrap(); Some((c.config.pins[0],c.config.pins[1])) }
    fn start(&mut self, read: bool) -> bool { { let mut camera=self.0.lock().unwrap(); camera.powered() && camera.sensor.start(read) } }
    fn write(&mut self, byte: u8) -> bool { self.0.lock().unwrap().sensor.write(byte) }
    fn read(&mut self) -> u8 { self.0.lock().unwrap().sensor.read() }
    fn stop(&mut self) { self.0.lock().unwrap().sensor.stop(); }
}

fn jpeg_size(bytes: &[u8]) -> Option<(u32,u32)> {
    if !bytes.starts_with(&[0xff,0xd8]) || !bytes.ends_with(&[0xff,0xd9]) { return None; }
    let mut pos = 2;
    while pos + 4 <= bytes.len() {
        if bytes[pos] != 0xff { return None; }
        let marker = bytes[pos+1];
        let len = u16::from_be_bytes([bytes[pos+2],bytes[pos+3]]) as usize;
        if len < 2 || pos + 2 + len > bytes.len() { return None; }
        if [0xc0,0xc1,0xc2].contains(&marker) {
            if len < 8 { return None; }
            return Some((u16::from_be_bytes([bytes[pos+7],bytes[pos+8]]) as u32,u16::from_be_bytes([bytes[pos+5],bytes[pos+6]]) as u32));
        }
        if marker == 0xda { return None; }
        pos += 2 + len;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config(sensor: u16) -> CameraConfig { let mut bytes = [0u8;20]; bytes[..2].copy_from_slice(&sensor.to_le_bytes()); for i in 0..14 { bytes[3+i]=i as u8; } bytes[17]=255;bytes[18]=255;bytes[19]=10; CameraConfig::parse(&bytes).unwrap() }
    fn write(sensor: &mut dyn I2cDevice, address: &[u8], value: u8) { sensor.start(false); for byte in address { sensor.write(*byte); } sensor.write(value); sensor.stop(); }
    #[test]
    fn sccb_sensor_identity_geometry_reset_and_host_bounds() {
        for pid in [0x26,0x5640] {
            let mut camera = Camera::new(config(pid));
            assert_eq!(camera.info(3),0);
            assert!(!camera.push(1601,1200,0,&[]));
            assert!(!camera.push(2,2,0,&[0;7]));
            assert!(!camera.push(2,2,3,&[255,216,255,217]));
            assert!(camera.push(2,2,0,&[0;8]));
            assert!(camera.frame().is_none());
            if pid == 0x26 {
                write(&mut *camera.sensor,&[0xff],0);write(&mut *camera.sensor,&[0x5a],24);write(&mut *camera.sensor,&[0x5b],24);write(&mut *camera.sensor,&[0xda],8);
            } else {
                write(&mut *camera.sensor,&[0x38,0x09],96);write(&mut *camera.sensor,&[0x38,0x0b],96);write(&mut *camera.sensor,&[0x43,0],0x61);
            }
            assert_eq!([camera.info(0),camera.info(1),camera.info(2),camera.info(3)],[96,96,0,1]);
            let mut bytes=vec![0;96*96*2];bytes[..2].copy_from_slice(&0xf81fu16.to_le_bytes());
            assert!(camera.push(96,96,0,&bytes));
            assert_eq!(&camera.frame().unwrap().2[..2], &[0xf8,0x1f]);
            if pid==0x26 { write(&mut *camera.sensor,&[0xff],1);write(&mut *camera.sensor,&[0x12],0x80); }
            else { write(&mut *camera.sensor,&[0x30,8],0x82); }
            assert_eq!(camera.info(0),0);
            assert!(camera.frame.is_some(),"sensor reset preserves the external host source");
            camera.reset_frame();assert!(camera.frame.is_none());
        }
    }
}
