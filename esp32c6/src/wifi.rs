//! C6 MAC v2 register layout, observed from the normal ESP-IDF driver.
use esp_periph::{Device, RegRam, WriteEffect};
pub use esp_wifi::wifi::*;
pub struct WifiMacC6 { pub mac: esp_wifi::WifiMac, ram: RegRam }
impl WifiMacC6 {
    pub fn new() -> Self { Self { mac: esp_wifi::WifiMac::new(super::periph::CPU_HZ, 0x4080_0000), ram: RegRam::new() } }
    fn canonical(off: u32) -> Option<(u32,u32)> {
        let mapped = match off {
            0xddc => (0x33,0xd14), 0xc48 => (0x33,0xc3c), 0xc4c => (0x33,0xc40),
            0x080 => (0x33,0x084), 0x084 => (0x33,0x088), 0x088 => (0x33,0x08c), 0x08c => (0x33,0x090),
            0x9018 => (0x35,0x10), 0x901c => (0x35,0x14), 0x9020 => (0x35,0x18), 0x9024 => (0x35,0x1c),
            0x90b0 => (0x35,0x118), 0x90b4 => (0x35,0x11c),
            o if o <= 0xd6c && (0xd6c-o)%16 == 0 && (0xd6c-o)/16 < 11 => (0x33,0xd08 - 8*((0xd6c-o)/16)),
            _ => return None,
        }; Some(mapped)
    }
}
impl Device for WifiMacC6 {
    fn read(&mut self, off:u32)->u32 {
        match off {
            0xcb0 => self.mac.txq_error, 0xcb8 => self.mac.txq_complete & 0x7ff,
            0x9014 => self.ram.read(off),
            _ => match Self::canonical(off) { Some((b,o)) => self.mac.read(b,o), None => self.ram.read(off) },
        }
    }
    fn write(&mut self, off:u32,v:u32)->WriteEffect {
        match off {
            0xcac => self.mac.txq_error &= !v,
            0xcb4 => self.mac.txq_complete &= !(v & 0x7ff),
            0x9014 => { self.ram.write(off,v); self.mac.write(0x35,0x0c,(v&3) | if v & 0xa0 != 0 {16} else {0}); },
            _ => match Self::canonical(off) { Some((b,o)) => self.mac.write(b,o,v), None => self.ram.write(off,v) },
        }
        WriteEffect::NONE
    }
    fn irq_sources(&self)->u64 { (self.mac.events != 0) as u64 | (((self.mac.pwr_events != 0) as u64)<<1) }
    fn debug(&mut self,on:bool) { self.mac.log=on; }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mac_v2_dma_irq_and_tsf_register_layout() {
        let mut wifi = WifiMacC6::new();
        wifi.write(0xddc, 2);
        assert_eq!(wifi.read(0xddc), 3);
        wifi.write(0x084, 0x1234);
        assert_eq!(wifi.mac.rx_next, 0x40801234);
        wifi.write(0xd6c - 16 * 3, (3 << 30) | 0x4567);
        assert_eq!(wifi.mac.tx_pending, [(3, 0x40804567)]);
        wifi.mac.tx_done(3);
        assert_eq!(wifi.read(0xcb8), 1 << 3);
        assert_eq!(wifi.read(0xc48), 1 << 7);
        wifi.write(0xcb4, 1 << 3);
        wifi.write(0xc4c, 1 << 7);
        assert_eq!(wifi.irq_sources(), 0);
        wifi.mac.now_cycles = 160_000_000;
        wifi.write(0x9014, 1);
        assert_eq!(wifi.read(0x9020), 1_000_000);
        wifi.write(0x9018, 42);
        wifi.write(0x901c, 0);
        wifi.write(0x9014, 0x20);
        wifi.write(0x9014, 1);
        assert_eq!(wifi.read(0x9020), 42);
    }
}
