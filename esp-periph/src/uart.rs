//! UART: TX to the host console, RX from it through the 128-byte receive FIFO; the transmit
//! side reads as idle (its FIFO count is 0 and TXFIFO_EMPTY/TX_DONE stay raised). The register
//! map differs a little between the chips — field widths, where rxfifo_rst sits — so the chip
//! crate picks a [`UartLayout`].
use std::collections::VecDeque;
use crate::device::{Device, WriteEffect};
use crate::regram::RegRam;

const RX_FIFO_SIZE: usize = 128;
const INT_RXFIFO_TOUT: u32 = 1 << 8;
const INT_RXFIFO_FULL: u32 = 1 << 0;
const INT_TXFIFO_EMPTY: u32 = 1 << 1;
const INT_RXFIFO_OVF: u32 = 1 << 4;
const INT_TX_DONE: u32 = 1 << 14;
/// TXFIFO_EMPTY and TX_DONE: always true here, so INT_CLR cannot take them down.
const INT_ALWAYS: u32 = INT_TXFIFO_EMPTY | INT_TX_DONE;

/// What differs between the chips' UART register maps (IDF `uart_reg.h` per chip).
#[derive(Clone, Copy, Debug)]
pub struct UartLayout {
    /// CONF1 rxfifo_full_thrhd: mask of the field at bit 0; txfifo_empty_thrhd starts right above it
    pub thrhd_mask: u32,
    /// CONF0 (CONF0_SYNC on the C6) rxfifo_rst bit
    pub rxfifo_rst: u32,
    /// STATUS rxfifo_cnt: mask of the field at bit 0
    pub rxfifo_cnt_mask: u32,
    pub timeout_enable_reg: u32,
    pub timeout_enable: u32,
    pub timeout_threshold_reg: u32,
    pub timeout_threshold_shift: u32,
    pub clock_conf: bool,
}
impl UartLayout {
    pub const S3: UartLayout = UartLayout { thrhd_mask: 0x3ff, rxfifo_rst: 1 << 17, rxfifo_cnt_mask: 0x3ff, timeout_enable_reg: 0x24, timeout_enable: 1 << 23, timeout_threshold_reg: 0x60, timeout_threshold_shift: 17, clock_conf: true };
    pub const C3: UartLayout = UartLayout { thrhd_mask: 0x1ff, rxfifo_rst: 1 << 17, rxfifo_cnt_mask: 0x3ff, timeout_enable_reg: 0x24, timeout_enable: 1 << 21, timeout_threshold_reg: 0x60, timeout_threshold_shift: 16, clock_conf: true };
    pub const C6: UartLayout = UartLayout { thrhd_mask: 0xff, rxfifo_rst: 1 << 22, rxfifo_cnt_mask: 0xff, timeout_enable_reg: 0x64, timeout_enable: 1, timeout_threshold_reg: 0x64, timeout_threshold_shift: 2, clock_conf: false };
}

// ------------------------------------------------------------------ UART
pub struct Uart { pub tx_out: Vec<u8>, pub int_raw: u32, pub int_ena: u32, layout: UartLayout, rx: VecDeque<u8>, idle_apb_ticks: u64, ram: RegRam }
impl Uart {
    pub fn new(layout: UartLayout) -> Self { Uart { tx_out: Vec::new(), int_raw: INT_ALWAYS, int_ena: 0, layout, rx: VecDeque::new(), idle_apb_ticks: 0, ram: RegRam::new() } }
    /// Bytes from the host into the receive FIFO; what does not fit is dropped and flagged RXFIFO_OVF.
    pub fn host_input(&mut self, data: &[u8]) {
        if !data.is_empty() { self.idle_apb_ticks = 0; }
        for &b in data {
            if self.rx.len() >= RX_FIFO_SIZE { self.int_raw |= INT_RXFIFO_OVF; break; }
            self.rx.push_back(b);
        }
        self.refresh_rx_full();
    }
    /// CONF1 rxfifo_full_thrhd (the layout says how wide); the silicon reset value is 0x60, a
    /// driver that wants every byte sets 1. RXFIFO_FULL is a level here: it stays raised while the count is at or
    /// over the threshold, so a driver that clears it before draining is woken again.
    fn rx_full_threshold(&self) -> usize { ((self.ram.read(0x24) & self.layout.thrhd_mask) as usize).max(1) }
    fn refresh_rx_full(&mut self) { if self.rx.len() >= self.rx_full_threshold() { self.int_raw |= INT_RXFIFO_FULL; } }
    fn timeout_ticks(&self) -> Option<u64> {
        if self.rx.is_empty() || self.int_raw & INT_RXFIFO_TOUT != 0 ||
            self.ram.read(self.layout.timeout_enable_reg) & self.layout.timeout_enable == 0 {
            return None;
        }
        let bits = ((self.ram.read(self.layout.timeout_threshold_reg) >> self.layout.timeout_threshold_shift) & 0x3ff) as u64;
        if bits == 0 { return None; }
        let div = self.ram.read(0x14);
        let baud_div16 = ((div & 0xfff) as u64 * 16 + ((div >> 20) & 0xf) as u64).max(16);
        let (clock_ratio, divisor16) = if self.layout.clock_conf {
            let conf = self.ram.read(0x78);
            let ratio = match (conf >> 20) & 3 { 1 => 1, 2 => 10, _ => 2 };
            let denominator = ((conf >> 6) & 0x3f) as u64;
            let fraction = if denominator == 0 { 0 } else { (conf & 0x3f) as u64 * 16 / denominator };
            (ratio, (((conf >> 12) & 0xff) as u64 + 1) * 16 + fraction)
        } else {
            // ponytail: C6 uses its default 80MHz UART clock; model PCR clock selection for non-default sources.
            (1, 16)
        };
        Some((bits * baud_div16 * clock_ratio * divisor16).div_ceil(256))
    }
    fn timeout_remaining(&self) -> Option<u64> {
        self.timeout_ticks().map(|ticks| ticks.saturating_sub(self.idle_apb_ticks).max(1))
    }
    /// Receive a completed 8N1 character from a physical UART transmitter.
    pub fn pin_input(&mut self, baud: u32, byte: u8, external_clock: Option<u32>) {
        let Some(actual) = self.baud(external_clock) else {return;};
        if actual.abs_diff(baud as u64)*100 > baud as u64*3 {
            self.int_raw |= 1 << 3;
            return;
        }
        self.host_input(&[byte]);
    }
    pub fn matches_baud(&self, baud: u32, external_clock: Option<u32>) -> bool {
        self.baud(external_clock).is_some_and(|actual|actual.abs_diff(baud as u64)*100 <= baud as u64*3)
    }
    fn baud(&self, external_clock: Option<u32>) -> Option<u64> {
        let clock = external_clock.unwrap_or_else(|| self.ram.read(0x78));
        let source = match (clock >> 20) & 3 {
            1 => 80000000u64,
            2 => {
                if self.layout.clock_conf {
                    8000000
                } else {
                    20000000
                }
            }
            3 => 40000000,
            _ => return None,
        };
        let denominator = (clock & 0x3f) as u64;
        let numerator = ((clock >> 6) & 0x3f) as u64;
        let divider16 = (((clock >> 12) & 0xff) as u64 + 1) * 16
            + if denominator == 0 {
                0
            } else {
                numerator * 16 / denominator
            };
        let div = self.ram.read(0x14);
        let baud_div16 = (div as u64 & 0xfff) * 16 + ((div >> 20) & 0xf) as u64;
        if baud_div16 == 0 {
            return None;
        }
        let actual = source * 256 / (divider16 * baud_div16);
        Some(actual)
    }
    pub fn rx_pending(&self) -> usize { self.rx.len() }
    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            0x0 => self.rx.pop_front().map(|b| b as u32).unwrap_or(0),
            0x4 => self.int_raw,
            0x8 => self.int_raw & self.int_ena,
            0xc => self.int_ena,
            0x1c => 0xe000_c000 | (self.rx.len() as u32 & self.layout.rxfifo_cnt_mask),   // STATUS: rxfifo_cnt, tx count 0, TXD/RTSN/DSRN idle levels as on silicon
            0x98 => 0,                              // REG_UPDATE (C6 and later): the driver sets it and spins until hardware clears it
            _ => self.ram.read(off),
        }
    }
    pub fn write(&mut self, off: u32, v: u32) {
        match off {
            0x0 => self.tx_out.push(v as u8),
            0xc => self.int_ena = v,
            0x10 => { self.int_raw &= !v | INT_ALWAYS; if v & INT_RXFIFO_TOUT != 0 { self.idle_apb_ticks = 0; } self.refresh_rx_full(); }
            0x20 => { if v & self.layout.rxfifo_rst != 0 { self.rx.clear(); self.idle_apb_ticks = 0; self.int_raw &= !(INT_RXFIFO_TOUT | INT_RXFIFO_FULL); } self.ram.write(off, v); }   // CONF0 rxfifo_rst
            0x24 => { self.ram.write(off, v); self.refresh_rx_full(); }
            _ => self.ram.write(off, v),
        }
    }
    pub fn irq(&self) -> bool { self.int_raw & self.int_ena != 0 }
}
impl Device for Uart {
    fn read(&mut self, off: u32) -> u32 { Uart::read(self, off) }
    fn write(&mut self, off: u32, v: u32) -> WriteEffect { Uart::write(self, off, v); WriteEffect::NONE }
    fn irq_sources(&self) -> u64 { self.irq() as u64 }
    fn clock(&self) -> Option<emu_core::ClockDomain> { Some(emu_core::ClockDomain::Apb) }
    fn has_deadline(&self) -> bool { true }
    fn next_deadline(&self) -> Option<u64> { self.timeout_remaining() }
    fn tick(&mut self, ticks: u64) {
        if let Some(timeout) = self.timeout_ticks() {
            self.idle_apb_ticks = self.idle_apb_ticks.saturating_add(ticks);
            if self.idle_apb_ticks >= timeout { self.int_raw |= INT_RXFIFO_TOUT; }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn short_receive_raises_timeout_below_fifo_threshold() {
        for layout in [UartLayout::S3, UartLayout::C3, UartLayout::C6] {
            let mut u = Uart::new(layout);
            u.write(0x24, 120);
            u.write(0x14, 80);
            u.write(0x78, 1 << 20);
            u.write(layout.timeout_threshold_reg, 10 << layout.timeout_threshold_shift);
            let enabled = u.read(layout.timeout_enable_reg) | layout.timeout_enable;
            u.write(layout.timeout_enable_reg, enabled);
            u.write(0xc, INT_RXFIFO_FULL | INT_RXFIFO_TOUT);
            u.host_input(b"E");
            assert!(!u.irq());
            assert_eq!(u.next_deadline(), Some(800));
            u.tick(799); assert!(!u.irq());
            u.tick(1); assert!(u.irq());
            assert_eq!(u.read(0x8), INT_RXFIFO_TOUT);
            assert_eq!(u.read(0x0), b'E' as u32);
            u.write(0x10, INT_RXFIFO_TOUT);
            assert!(!u.irq());
            assert_eq!(u.next_deadline(), None);
            u.host_input(b"x"); u.tick(700);
            u.host_input(b"y"); u.tick(100); assert!(!u.irq());
            u.tick(700); assert!(u.irq());
            u.write(0x20, layout.rxfifo_rst);
            assert!(!u.irq()); assert_eq!(u.next_deadline(), None);
        }
    }
    /// The Linux esp32_uart driver's receive path: threshold 1, RXFIFO_FULL enabled, count from
    /// STATUS, pop the FIFO, then INT_CLR — and the line must drop only once the FIFO is empty.
    #[test]
    fn receive_fifo_drives_rxfifo_full_as_a_level() {
        let mut u = Uart::new(UartLayout::S3);
        u.write(0x24, 1); u.write(0xc, INT_RXFIFO_FULL);
        assert!(!u.irq());
        u.host_input(b"ro");
        assert!(u.irq()); assert_eq!(u.read(0x1c) & 0x3ff, 2);
        u.write(0x10, INT_RXFIFO_FULL);          // cleared early: still two bytes waiting
        assert!(u.irq());
        assert_eq!((u.read(0x0), u.read(0x0)), (b'r' as u32, b'o' as u32));
        u.write(0x10, INT_RXFIFO_FULL);
        assert!(!u.irq()); assert_eq!(u.read(0x1c) & 0x3ff, 0); assert_eq!(u.read(0x0), 0);
        assert_eq!(u.read(0x4) & INT_ALWAYS, INT_ALWAYS);
    }
    #[test]
    fn receive_fifo_overflow_is_flagged_and_reset_by_conf0() {
        let mut u = Uart::new(UartLayout::S3);
        u.host_input(&[b'x'; RX_FIFO_SIZE + 3]);
        assert_eq!(u.read(0x1c) & 0x3ff, RX_FIFO_SIZE as u32);
        assert_ne!(u.read(0x4) & INT_RXFIFO_OVF, 0);
        u.write(0x20, 1 << 17);
        assert_eq!(u.read(0x1c) & 0x3ff, 0);
    }
    /// The C6 map: the IDF driver's default txfifo_empty_thrhd of 10 sits in bits 15:8 of CONF1,
    /// right above an 8-bit rxfifo_full_thrhd, and rxfifo_rst is bit 22 of CONF0_SYNC.
    #[test]
    fn c6_layout_reads_its_own_fields() {
        let mut u = Uart::new(UartLayout::C6);
        u.write(0x24, (10 << 8) | 1); u.write(0xc, INT_RXFIFO_FULL);
        u.host_input(b"x");
        assert!(u.irq());
        u.write(0x20, 1 << 17);   // the S3's rxfifo_rst bit means nothing here
        assert_eq!(u.read(0x1c) & 0xff, 1);
        u.write(0x20, 1 << 22);
        assert_eq!(u.read(0x1c) & 0xff, 0);
    }
    #[test]
    fn physical_uart_checks_baud_and_uses_existing_fifo() {
        for layout in [UartLayout::S3,UartLayout::C3,UartLayout::C6] {
            let mut uart=Uart::new(layout);
            uart.pin_input(9600,b'x',None);
            assert_eq!(uart.rx_pending(),0);
            assert_eq!(uart.int_raw & (1<<3),0);
            let clock=(3<<20)|(1<<12);
            uart.write(0x78,clock); uart.write(0x14,(5<<20)|2083);
            uart.pin_input(19200,b'x',Some(clock));
            assert_eq!(uart.rx_pending(),0);
            assert_ne!(uart.int_raw & (1<<3),0);
            uart.pin_input(9600,b'y',Some(clock));
            assert_eq!(uart.read(0),b'y' as u32);
            assert_eq!(uart.rx_pending(),0);
        }
    }

}
