//! I2C master controller (I2C0/I2C1) and the `I2cDevice` trait the board's bus devices implement.
//! The controller executes the command list (RSTART/WRITE/READ/STOP/END) written by the driver
//! at `trans_start`, moving bytes between the FIFOs and the addressed device, and raises the
//! NACK / END_DETECT / TRANS_COMPLETE interrupts the IDF `i2c_master` driver waits for.
use crate::device::{Device, WriteEffect};
use crate::regram::RegRam;
use emu_core::ClockDomain;
use std::collections::VecDeque;

pub trait I2cDevice {
    fn address(&self, configured: u8) -> u8 {
        configured
    }
    fn matches_address(&self, configured:u8, address:u8, _read:bool)->bool {self.address(configured)==address}
    fn start_address(&mut self,_address:u8,read:bool)->bool {self.start(read)}
    fn pins(&self) -> Option<(u8, u8)> {
        None
    }
    /// Address phase: the master addressed this device for a read (`read`) or a write. Return ACK.
    fn start(&mut self, _read: bool) -> bool {
        true
    }
    /// One data byte from the master. Return ACK.
    fn write(&mut self, b: u8) -> bool;
    /// One data byte to the master.
    fn read(&mut self) -> u8;
    fn stop(&mut self) {}
}

pub const INT_END_DETECT: u32 = 1 << 3;
pub const INT_TRANS_COMPLETE: u32 = 1 << 7;
pub const INT_NACK: u32 = 1 << 10;

pub struct I2c {
    pub regs: RegRam,
    tx: VecDeque<u8>,
    rx: VecDeque<u8>,
    pub int_raw: u32,
    pub int_ena: u32,
    cmd: [u32; 8],
    devices: Vec<(u8, Box<dyn I2cDevice>)>,
    cur: Vec<usize>,
    addressed: Vec<usize>,
    pins: Option<(u8, u8)>,
    expect_addr: bool,
    nack: bool,
    pub log: bool,
    pub transactions: u64,
    active: bool,
    command_index: usize,
    byte_index: usize,
    remaining: u64,
    pub external_clock_config: Option<u32>,
}

impl I2c {
    pub fn new() -> Self {
        I2c {
            regs: RegRam::new(),
            tx: VecDeque::new(),
            rx: VecDeque::new(),
            int_raw: 0,
            int_ena: 0,
            cmd: [0; 8],
            devices: Vec::new(),
            cur: Vec::new(),
            addressed: Vec::new(),
            pins: None,
            expect_addr: false,
            nack: false,
            log: false,
            transactions: 0,
            active: false,
            command_index: 0,
            byte_index: 0,
            remaining: 0,
            external_clock_config: None,
        }
    }
    pub fn attach(&mut self, addr: u8, dev: Box<dyn I2cDevice>) {
        self.devices.push((addr, dev));
    }
    pub fn reset(&mut self) {
        let devices = std::mem::take(&mut self.devices);
        let log = self.log;
        *self = Self::new();
        self.devices = devices;
        self.log = log;
    }
    pub fn clear_devices(&mut self) {
        self.devices.clear();
        self.addressed.clear();
        self.cur.clear();
    }
    pub fn route(&mut self, gpio: &crate::Gpio, sda: u32, scl: u32, pin_mask: u32) {
        let input_pin = |signal: u32| {
            let config = gpio.func_in_sel[signal as usize];
            let pin = (config & pin_mask) as usize;
            (config & ((pin_mask + 1) << 1) != 0
                && pin < gpio.func_out_sel.len()
                && gpio.func_out_sel[pin] & 0x1ff == signal)
                .then_some(pin as u8)
        };
        self.pins = input_pin(sda).zip(input_pin(scl));
    }
    pub fn has_device(&self, addr: u8) -> bool {
        self.devices.iter().any(|(attached, _)| *attached == addr)
    }
    pub fn irq(&self) -> bool {
        self.int_raw & self.int_ena != 0
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            0x08 => {
                (self.nack as u32)
                    | ((self.active as u32) << 4)
                    | ((self.rx.len() as u32 & 0x3f) << 8)
                    | ((self.tx.len() as u32 & 0x3f) << 18)
            } // SR: resp_rec, rxfifo_cnt, txfifo_cnt
            0x14 => ((self.rx.len() as u32 & 0x1f) << 5) | ((self.tx.len() as u32 & 0x1f) << 15), // FIFO_ST: waddr = count, raddr = 0
            0x1c => self.rx.pop_front().unwrap_or(0) as u32,
            0x20 => self.int_raw,
            0x28 => self.int_ena,
            0x2c => self.int_raw & self.int_ena,
            0x58..=0x74 => self.cmd[((off - 0x58) / 4) as usize],
            _ => self.regs.read(off),
        }
    }

    /// FIFO reset bits are independent and may both be set by one write.
    #[allow(clippy::possible_missing_else)]
    pub fn write(&mut self, off: u32, v: u32) {
        match off {
            0x04 => {
                self.regs.write(off, v & !((1 << 5) | (1 << 10)));
                if v & (1 << 10) != 0 {
                    self.active = false;
                    self.cur.clear();
                    self.addressed.clear();
                    self.expect_addr = false;
                    self.nack = false;
                    self.remaining = 0;
                }
                if v & (1 << 5) != 0 {
                    self.run();
                }
            } // CTR.TRANS_START
            0x18 => {
                if v & (1 << 13) != 0 {
                    self.tx.clear();
                }
                if v & (1 << 12) != 0 {
                    self.rx.clear();
                }
                self.regs.write(off, v & !(3 << 12));
            }
            0x1c => {
                if self.tx.len() < 32 {
                    self.tx.push_back(v as u8);
                }
            }
            0x24 => self.int_raw &= !v,
            0x28 => self.int_ena = v,
            0x58..=0x74 => self.cmd[((off - 0x58) / 4) as usize] = v & !(1 << 31),
            _ => self.regs.write(off, v),
        }
    }

    fn clock_ticks(&self, cycles: u64) -> u64 {
        let c = self
            .external_clock_config
            .unwrap_or_else(|| self.regs.read(0x54));
        let a = ((c >> 8) & 63) as u64;
        let b = ((c >> 14) & 63) as u64;
        let denominator = a.max(1);
        let divisor = ((c & 255) as u64 + 1) * denominator + if a > 0 { b } else { 0 };
        let hz = if c & (1 << 20) != 0 {
            17_500_000
        } else {
            40_000_000
        };
        (cycles * divisor * 80_000_000)
            .div_ceil(hz * denominator)
            .max(1)
    }
    fn scl_ticks(&self) -> u64 {
        let high = self.regs.read(0x38);
        let cycles = (self.regs.read(0) & 511) + 1 + (high & 511) + ((high >> 9) & 127);
        self.clock_ticks(cycles as u64)
    }
    fn schedule(&mut self) {
        if self.command_index >= 8 {
            self.active = false;
            return;
        }
        let op = (self.cmd[self.command_index] >> 11) & 7;
        self.remaining = match op {
            1 | 3 => self.scl_ticks() * 9,
            6 => self.clock_ticks(
                ((self.regs.read(0x40) & 511) + (self.regs.read(0x44) & 511) + 2) as u64,
            ),
            2 => self.clock_ticks(
                ((self.regs.read(0x48) & 511) + (self.regs.read(0x4c) & 511) + 2) as u64,
            ),
            _ => 1,
        };
    }
    fn run(&mut self) {
        if self.active {
            return;
        }
        self.nack = false;
        self.transactions += 1;
        self.active = true;
        self.command_index = 0;
        self.byte_index = 0;
        self.schedule();
    }
    fn advance(&mut self, mut ticks: u64) {
        while self.active && ticks >= self.remaining {
            ticks -= self.remaining;
            self.step();
            if self.active {
                self.schedule()
            }
        }
        if self.active {
            self.remaining -= ticks
        }
    }
    fn step(&mut self) {
        let i = self.command_index;
        let c = self.cmd[i];
        let op = (c >> 11) & 7;
        let n = (c & 255) as usize;
        match op {
            6 => self.expect_addr = true,
            1 if self.byte_index < n => {
                let b = self.tx.pop_front().unwrap_or(0);
                let ack = if self.expect_addr {
                    self.expect_addr = false;
                    let addr = b >> 1;
                    let rd = b & 1 != 0;
                    self.cur.clear();
                    for (k,(configured,device)) in self.devices.iter_mut().enumerate() {
                        if device.matches_address(*configured,addr,rd) && device.pins().is_none_or(|pins|Some(pins)==self.pins) && device.start_address(addr,rd) {
                            self.cur.push(k);
                            if !self.addressed.contains(&k) {self.addressed.push(k);}
                        }
                    }
                    if self.log {eprintln!("[i2c] start {addr:#x} read={rd}");}
                    !self.cur.is_empty()
                } else {
                    let mut ack=false;
                    for &k in &self.cur {ack |= self.devices[k].1.write(b);}
                    ack
                };
                if !ack && c & (1 << 8) != 0 {
                    self.nack = true;
                    self.int_raw |= INT_NACK;
                    self.cmd[i] |= 1 << 31;
                    self.cur.clear();
                    self.active = false;
                    return;
                }
                self.byte_index += 1;
                if self.byte_index < n {
                    return;
                }
            }
            3 if self.byte_index < n => {
                let mut b=0xff;
                for &k in &self.cur {b &= self.devices[k].1.read();}
                if self.rx.len() < 32 {
                    self.rx.push_back(b)
                }
                self.byte_index += 1;
                if self.byte_index < n {
                    return;
                }
            }
            2 => {
                for k in self.addressed.drain(..) {self.devices[k].1.stop();}
                self.cur.clear();
                self.int_raw |= INT_TRANS_COMPLETE;
                self.active = false
            }
            4 => {
                self.int_raw |= INT_END_DETECT;
                self.active = false
            }
            _ => self.active = false,
        }
        self.cmd[i] |= 1 << 31;
        self.command_index += 1;
        self.byte_index = 0;
    }
}

impl Default for I2c {
    fn default() -> Self {
        Self::new()
    }
}

// ------------------------------------------------------------------ devices

/// Generic 8-bit-register device (audio codecs etc.): first written byte selects the register,
/// following bytes / reads auto-increment.
pub struct Reg8Device {
    pub name: &'static str,
    pub regs: [u8; 256],
    ptr: u8,
    first: bool,
}
impl Reg8Device {
    pub fn new(name: &'static str, defaults: &[(u8, u8)]) -> Self {
        let mut d = Reg8Device {
            name,
            regs: [0; 256],
            ptr: 0,
            first: true,
        };
        for &(r, v) in defaults {
            d.regs[r as usize] = v;
        }
        d
    }
}
impl I2cDevice for Reg8Device {
    fn start(&mut self, read: bool) -> bool {
        if !read {
            self.first = true;
        }
        true
    }
    fn write(&mut self, b: u8) -> bool {
        if self.first {
            self.ptr = b;
            self.first = false;
        } else {
            self.regs[self.ptr as usize] = b;
            self.ptr = self.ptr.wrapping_add(1);
        }
        true
    }
    fn read(&mut self) -> u8 {
        let v = self.regs[self.ptr as usize];
        self.ptr = self.ptr.wrapping_add(1);
        v
    }
}

/// Waveshare's CH32V003 IO expander: regs 0x02 direction, 0x03 output, 0x04 input, 0x05 PWM, 0x06 ADC, 0x07 RTC.
impl Device for I2c {
    fn read(&mut self, off: u32) -> u32 {
        I2c::read(self, off)
    }
    fn write(&mut self, off: u32, v: u32) -> WriteEffect {
        I2c::write(self, off, v);
        WriteEffect::NONE
    }
    fn irq_sources(&self) -> u64 {
        self.irq() as u64
    }
    fn clock(&self) -> Option<ClockDomain> {
        Some(ClockDomain::Apb)
    }
    fn tick(&mut self, ticks: u64) {
        self.advance(ticks)
    }
    fn has_deadline(&self) -> bool {
        true
    }
    fn next_deadline(&self) -> Option<u64> {
        self.active.then_some(self.remaining)
    }
    fn debug(&mut self, on: bool) {
        self.log = on;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct WiredDevice;
    impl I2cDevice for WiredDevice {
        fn pins(&self) -> Option<(u8, u8)> {
            Some((4, 5))
        }
        fn write(&mut self, _: u8) -> bool {
            true
        }
        fn read(&mut self) -> u8 {
            0x42
        }
    }
    fn bus_100khz() -> I2c {
        let mut bus = I2c::new();
        bus.write(0, 199);
        bus.write(0x38, 102 | (98 << 9));
        for reg in [0x40, 0x44, 0x48, 0x4c] {
            bus.write(reg, 199);
        }
        bus
    }
    struct ByteDevice;
    impl I2cDevice for ByteDevice {
        fn write(&mut self, _: u8) -> bool {
            true
        }
        fn read(&mut self) -> u8 {
            0xa5
        }
    }
    #[test]
    fn byte_ack_and_stop_have_wire_duration_before_interrupt() {
        let mut bus = bus_100khz();
        bus.attach(0x20, Box::new(ByteDevice));
        bus.write(0x1c, 0x40);
        bus.write(0x58, 6 << 11);
        bus.write(0x5c, (1 << 11) | (1 << 8) | 1);
        bus.write(0x60, 2 << 11);
        bus.write(0x28, INT_TRANS_COMPLETE);
        bus.write(0x04, 1 << 5);
        assert_eq!(bus.next_deadline(), Some(800));
        bus.advance(799);
        assert!(!bus.irq());
        assert_eq!(bus.read(0x58) >> 31, 0);
        bus.advance(1);
        assert_eq!(bus.read(0x58) >> 31, 1);
        assert_eq!(bus.next_deadline(), Some(7200));
        bus.advance(7199);
        assert_eq!(bus.read(0x5c) >> 31, 0);
        bus.advance(1);
        assert_eq!(bus.read(0x5c) >> 31, 1);
        bus.advance(799);
        assert!(!bus.irq());
        bus.advance(1);
        assert!(bus.irq());
        assert_eq!(bus.next_deadline(), None);
        assert_eq!(bus.read(0x08) & 16, 0);
    }
    #[test]
    fn end_preserves_selected_device_for_fifo_refill_and_repeated_start() {
        let mut bus = bus_100khz();
        bus.attach(0x20, Box::new(ByteDevice));
        bus.write(0x1c, 0x40);
        bus.write(0x58, 6 << 11);
        bus.write(0x5c, (1 << 11) | (1 << 8) | 1);
        bus.write(0x60, 4 << 11);
        bus.write(0x04, 1 << 5);
        bus.advance(8001);
        assert_eq!(bus.int_raw, INT_END_DETECT);
        assert_eq!(bus.cur, vec![0]);
        bus.write(0x24, u32::MAX);
        bus.write(0x1c, 0x41);
        bus.write(0x58, 6 << 11);
        bus.write(0x5c, (1 << 11) | (1 << 8) | 1);
        bus.write(0x60, (3 << 11) | 1);
        bus.write(0x64, 2 << 11);
        bus.write(0x04, 1 << 5);
        bus.advance(800 + 7200 + 7199);
        assert_eq!(bus.rx.len(), 0);
        bus.advance(1);
        assert_eq!(bus.read(0x1c), 0xa5);
        bus.advance(800);
        assert_eq!(bus.int_raw, INT_TRANS_COMPLETE);
        assert!(bus.cur.is_empty());
    }
    #[test]
    fn nack_is_delayed_until_address_ack_and_reset_cancels_active_transfer() {
        let mut bus = bus_100khz();
        bus.write(0x1c, 0x40);
        bus.write(0x58, 6 << 11);
        bus.write(0x5c, (1 << 11) | (1 << 8) | 1);
        bus.write(0x60, 2 << 11);
        bus.write(0x04, 1 << 5);
        bus.advance(7999);
        assert_eq!(bus.int_raw, 0);
        bus.advance(1);
        assert_eq!(bus.int_raw, INT_NACK);
        assert!(!bus.active);
        bus.write(0x04, 1 << 5);
        bus.reset();
        bus.advance(100000);
        assert_eq!(bus.int_raw, 0);
        assert!(!bus.active);
    }
    #[test]
    fn driver_timeout_fsm_reset_cancels_pending_ack_without_resetting_clock() {
        let mut bus = bus_100khz();
        bus.write(0x1c, 0x40);
        bus.write(0x58, 6 << 11);
        bus.write(0x5c, (1 << 11) | (1 << 8) | 1);
        bus.write(0x60, 2 << 11);
        bus.write(0x04, 1 << 5);
        bus.advance(800);
        bus.write(0x04, 1 << 10);
        assert_eq!(bus.read(0x04) & (1 << 10), 0);
        assert_eq!(bus.next_deadline(), None);
        bus.advance(8000);
        assert_eq!(bus.int_raw, 0);
        assert_eq!(bus.scl_ticks(), 800);
        bus.attach(0x20, Box::new(ByteDevice));
        bus.write(0x04, 1 << 5);
        bus.advance(8800);
        assert_eq!(bus.int_raw, INT_TRANS_COMPLETE);
    }
    #[test]
    fn divider_fraction_and_external_clock_control_wire_period() {
        let mut bus = bus_100khz();
        assert_eq!(bus.scl_ticks(), 800);
        bus.write(0x54, 1 | (2 << 8) | (1 << 14));
        assert_eq!(bus.scl_ticks(), 2000);
        bus.external_clock_config = Some(0);
        assert_eq!(bus.scl_ticks(), 800);
        bus.external_clock_config = Some(1 << 20);
        assert_eq!(bus.scl_ticks(), (400_u64 * 80_000_000).div_ceil(17_500_000));
    }
    #[test]
    fn active_matrix_route_selects_device_and_controller_reset_keeps_external_devices() {
        let mut i2c = I2c::new();
        i2c.attach(0x3c, Box::new(WiredDevice));
        let mut gpio = crate::Gpio::new();
        gpio.func_out_sel[2] = 54; // stale output route from an earlier bus
        gpio.func_out_sel[4] = 54;
        gpio.func_out_sel[5] = 53;
        gpio.func_in_sel[54] = 4 | 64;
        gpio.func_in_sel[53] = 5 | 64;
        i2c.route(&gpio, 54, 53, 31);
        assert_eq!(i2c.pins, Some((4, 5)));
        let probe = |bus: &mut I2c| {
            bus.write(0x24, u32::MAX);
            bus.write(0x1c, 0x78);
            bus.write(0x58, 6 << 11);
            bus.write(0x5c, (1 << 11) | (1 << 8) | 1);
            bus.write(0x60, 2 << 11);
            bus.write(0x04, 1 << 5);
            bus.advance(1000);
        };
        probe(&mut i2c);
        assert_eq!(i2c.int_raw, INT_TRANS_COMPLETE);
        i2c.reset();
        assert!(i2c.has_device(0x3c));
        gpio.func_in_sel[54] = 2 | 64;
        i2c.route(&gpio, 54, 53, 31);
        probe(&mut i2c);
        assert_eq!(i2c.int_raw, INT_NACK);
    }
    #[test]
    fn group_addresses_write_all_read_wired_and_and_stop_prior_repeated_start_devices() {
        use std::sync::{Arc,Mutex};
        struct Group {value:u8,state:Arc<Mutex<Vec<u8>>>}
        impl I2cDevice for Group {
            fn matches_address(&self,configured:u8,address:u8,_:bool)->bool {address==configured || address==0x70}
            fn write(&mut self,b:u8)->bool {self.state.lock().unwrap().push(b);true}
            fn read(&mut self)->u8 {self.value}
            fn stop(&mut self){self.state.lock().unwrap().push(0xff);}
        }
        let a=Arc::new(Mutex::new(Vec::new()));let b=Arc::new(Mutex::new(Vec::new()));
        let mut bus=bus_100khz();bus.attach(0x40,Box::new(Group{value:0xf0,state:a.clone()}));bus.attach(0x41,Box::new(Group{value:0xcc,state:b.clone()}));
        for byte in [0xe0,0x12,0xe1] {bus.write(0x1c,byte);}
        for (i,command) in [6<<11,(1<<11)|(1<<8)|2,6<<11,(1<<11)|(1<<8)|1,(3<<11)|1,2<<11].into_iter().enumerate(){bus.write(0x58+i as u32*4,command);}
        bus.write(0x04,1<<5);bus.advance(100_000);assert_eq!(bus.int_raw,INT_TRANS_COMPLETE);assert_eq!(bus.read(0x1c),0xc0);
        assert_eq!(*a.lock().unwrap(),vec![0x12,0xff]);assert_eq!(*b.lock().unwrap(),vec![0x12,0xff]);
        bus.write(0x24,u32::MAX);
        for byte in [0x80,0x23,0x82,0x34] {bus.write(0x1c,byte);}
        for (i,command) in [6<<11,(1<<11)|(1<<8)|2,6<<11,(1<<11)|(1<<8)|2,2<<11].into_iter().enumerate(){bus.write(0x58+i as u32*4,command);}
        bus.write(0x04,1<<5);bus.advance(100_000);
        assert_eq!(*a.lock().unwrap(),vec![0x12,0xff,0x23,0xff]);assert_eq!(*b.lock().unwrap(),vec![0x12,0xff,0x34,0xff]);
    }

}
