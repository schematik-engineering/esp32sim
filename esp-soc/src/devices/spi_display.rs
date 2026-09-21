use super::{DcsPanel, SpiBitBang};

#[derive(Clone, Copy, Debug)]
pub struct SpiDisplayConfig {
    pub id: u8,
    pub model: u8,
    pub sclk: u8,
    pub mosi: u8,
    pub cs: Option<u8>,
    pub dc: u8,
    pub reset: Option<u8>,
    pub width: u16,
    pub height: u16,
    pub x: u16,
    pub y: u16,
    pub address_width: u16,
    pub address_height: u16,
    pub bgr: bool,
    pub inverted: bool,
    pub backlight: Option<u8>,
    pub backlight_active_low: bool,
}

pub struct SpiDisplay {
    pub config: SpiDisplayConfig,
    panel: DcsPanel,
    wire: SpiBitBang,
    selected: bool,
    reset_low: bool,
    brightness: u32,
    pub generation: u64,
}

impl SpiDisplay {
    pub fn new(config: SpiDisplayConfig) -> Result<Self, String> {
        let (cols, rows) = match config.model {
            1 | 2 | 5 => (240, 320), // ST7789 / ILI9341
            3 => (132, 162),     // ST7735
            4 => (240, 240),     // GC9A01
            _ => return Err("unsupported SPI display controller".into()),
        };
        let (cols,rows)=match (config.address_width,config.address_height) {
            (0,0)=>(cols,rows),
            (w,h) if w>0 && h>0 && w as usize<=cols && h as usize<=rows=>(w as usize,h as usize),
            _=>return Err("invalid SPI display address geometry".into()),
        };
        let pins: Vec<_> = [Some(config.sclk), Some(config.mosi), config.cs, Some(config.dc), config.reset, config.backlight].into_iter().flatten().collect();
        if pins.iter().any(|&pin| pin >= 49) || pins.iter().enumerate().any(|(i,pin)| pins[..i].contains(pin))
            || config.width == 0 || config.height == 0
            || config.x as usize + config.width as usize > cols
            || config.y as usize + config.height as usize > rows {
            return Err("invalid SPI display wiring or visible window".into());
        }
        let mut wire = SpiBitBang::new(config.sclk, config.mosi, config.cs.unwrap_or(255));
        if config.cs.is_none() { wire.pin(255, false); }
        Ok(Self { config, panel: DcsPanel::new(cols, rows), wire, selected: config.cs.is_none(), reset_low: false, brightness: if config.backlight.is_none() || config.backlight_active_low {65535} else {0}, generation: 1 })
    }

    fn byte(&mut self, byte: u8) {
        let before = (self.panel.pixels_written, self.panel.madctl, self.panel.on, self.panel.sleeping, self.panel.inverted, self.panel.resets);
        self.panel.byte(byte);
        let after = (self.panel.pixels_written, self.panel.madctl, self.panel.on, self.panel.sleeping, self.panel.inverted, self.panel.resets);
        if before != after { self.generation = self.generation.wrapping_add(1).max(1); }
    }

    pub fn gpio(&mut self, pin: u8, high: bool) {
        self.backlight_duty(pin, if high {65535} else {0});
        if self.config.reset == Some(pin) {
            if !high && !self.reset_low { self.panel.reset(); self.generation = self.generation.wrapping_add(1).max(1); }
            self.reset_low = !high;
        }
        if self.config.cs == Some(pin) { self.selected = !high; }
        if pin == self.config.dc { self.panel.dc = high; }
        if let Some(byte) = self.wire.pin(pin, high) { if !self.reset_low { self.byte(byte); } }
    }

    pub fn backlight_duty(&mut self, pin: u8, duty: u32) {
        if self.config.backlight != Some(pin) { return; }
        let brightness = if self.config.backlight_active_low {65535-duty.min(65535)} else {duty.min(65535)};
        if brightness != self.brightness { self.brightness=brightness; self.generation=self.generation.wrapping_add(1).max(1); }
    }

    pub fn transfer(&mut self, sclk: u64, mosi: u64, hardware_cs: u64, bytes: &[u8]) {
        let selected = self.selected || self.config.cs.is_some_and(|pin| hardware_cs & (1u64 << pin) != 0);
        if self.reset_low || !selected || sclk & (1u64 << self.config.sclk) == 0 || mosi & (1u64 << self.config.mosi) == 0 { return; }
        for &byte in bytes { self.byte(byte); }
    }

    pub fn frame(&self) -> Vec<u8> {
        let mut frame = vec![0; self.config.width as usize * self.config.height as usize * 2];
        if !self.panel.on || self.panel.sleeping || self.reset_low || self.brightness==0 { return frame; }
        for y in 0..self.config.height as usize {
            for x in 0..self.config.width as usize {
                let mut pixel = self.panel.gram[(y + self.config.y as usize) * self.panel.cols + x + self.config.x as usize];
                if (self.panel.madctl & 8 != 0) != self.config.bgr { pixel = ((pixel & 31) << 11) | (pixel & 0x7e0) | (pixel >> 11); }
                if self.panel.inverted != self.config.inverted { pixel = !pixel; }
                if self.brightness != 65535 { let scale=|v:u16| ((v as u32*self.brightness+32767)/65535) as u16; pixel=(scale(pixel>>11)<<11)|(scale((pixel>>5)&63)<<5)|scale(pixel&31); }
                let offset = (y * self.config.width as usize + x) * 2;
                frame[offset..offset + 2].copy_from_slice(&pixel.to_le_bytes());
            }
        }
        frame
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> SpiDisplayConfig { SpiDisplayConfig { id: 0, model: 1, sclk: 4, mosi: 5, cs: Some(6), dc: 7, reset: Some(8), width: 2, height: 1, x: 0, y: 0, address_width: 0, address_height: 0, bgr: false, inverted: false, backlight: None, backlight_active_low: false } }
    fn command(display: &mut SpiDisplay, command: u8, data: &[u8]) {
        display.gpio(7, false); display.transfer(1<<4, 1<<5, 0, &[command]);
        display.gpio(7, true); display.transfer(1<<4, 1<<5, 0, data);
    }
    #[test]
    fn physical_routes_cs_window_colors_and_reset() {
        let mut display = SpiDisplay::new(config()).unwrap();
        command(&mut display, 0x11, &[]); assert!(display.panel.sleeping);
        display.gpio(6, false);
        command(&mut display, 0x11, &[]); command(&mut display, 0x29, &[]); command(&mut display, 0x3a, &[0x55]);
        command(&mut display, 0x2a, &[0,0,0,1]); command(&mut display, 0x2b, &[0,0,0,0]);
        command(&mut display, 0x2c, &[0xf8,0,0,0x1f]);
        assert_eq!(display.frame(), [0,0xf8,0x1f,0]);
        command(&mut display, 0x2c, &[]); display.transfer(1<<3,1<<5,0,&[0xff,0xff]);
        assert_eq!(display.frame(), [0,0xf8,0x1f,0]);
        command(&mut display, 0x36, &[8]); assert_eq!(display.frame(), [0x1f,0,0,0xf8]);
        display.config.backlight=Some(9); display.backlight_duty(9,0); assert_eq!(display.frame(),[0,0,0,0]);
        display.backlight_duty(9,65535); assert_eq!(display.frame(),[0x1f,0,0,0xf8]);
        display.gpio(8,false); assert_eq!(display.frame(), [0,0,0,0]);
        assert!(SpiDisplay::new(SpiDisplayConfig { dc:4,..config() }).is_err());
        assert!(SpiDisplay::new(SpiDisplayConfig { x:240,..config() }).is_err());
    }
    #[test]
    fn module_address_modes_preserve_every_pixel_in_all_four_rotations() {
        for (cols,rows,width,height,x,y,bgr) in [(128,160,128,160,0,0,false),(132,162,128,160,2,1,true),(132,132,128,128,2,1,true)] {
            let mut display=SpiDisplay::new(SpiDisplayConfig{model:3,width,height,x,y,bgr,address_width:cols,address_height:rows,..config()}).unwrap();
            display.gpio(6,false);command(&mut display,0x11,&[]);command(&mut display,0x29,&[]);
            for rotation in 0..4 {
                let rowstart=if rows==132&&rotation<2{3}else{y};
                let (xs,ys,w,h)=if rotation%2==0{(x,rowstart,width,height)}else{(rowstart,x,height,width)};
                command(&mut display,0x36,&[[0xc0,0xa0,0,0x60][rotation]|if bgr{8}else{0}]);
                let window=|display:&mut SpiDisplay,w:u16,h:u16|{
                    let pair=|a:u16,b:u16|{let [a,b0]=a.to_be_bytes();let [c,d]=b.to_be_bytes();[a,b0,c,d]};
                    command(display,0x2a,&pair(xs,xs+w-1));command(display,0x2b,&pair(ys,ys+h-1));
                };
                window(&mut display,w,h);command(&mut display,0x2c,&[0xff,0xff].repeat(w as usize*h as usize));
                window(&mut display,10,20);command(&mut display,0x2c,&[0,0x1f].repeat(200));
                let frame=display.frame();let (bw,bh)=if rotation%2==0{(10,20)}else{(20,10)};
                let left=if rotation==0||rotation==3{width-bw}else{0};let top=if rotation<2{height-bh}else{0};
                for yy in 0..height {for xx in 0..width {let off=((yy as usize*width as usize)+xx as usize)*2;let expected=if xx>=left&&xx<left+bw&&yy>=top&&yy<top+bh{0x001f}else{0xffff};assert_eq!(u16::from_le_bytes([frame[off],frame[off+1]]),expected,"address{cols}x{rows} rotation{rotation} at{xx},{yy}");}}
            }
            command(&mut display,1,&[]);assert_eq!((display.panel.cols,display.panel.rows),(cols as usize,rows as usize));
        }
        assert!(SpiDisplay::new(SpiDisplayConfig{address_width:128,..config()}).is_err());
        assert!(SpiDisplay::new(SpiDisplayConfig{model:3,address_width:133,address_height:162,..config()}).is_err());
        assert!(SpiDisplay::new(SpiDisplayConfig{model:3,width:128,height:160,x:2,address_width:128,address_height:160,..config()}).is_err());
    }

}
