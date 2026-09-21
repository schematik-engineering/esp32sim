use std::collections::VecDeque;
use crate::device::{Device, WriteEffect};
use crate::regram::RegRam;
use emu_core::ClockDomain;

// ------------------------------------------------------------------ RMT (TX channels 0-3) — enough for WS2812 via the legacy driver
pub const RMT_MEM_WORDS: usize = 48;
const MAX_CAPTURED_BITS: usize = 24 * 4096;
#[derive(Clone, Default)]
pub struct RmtTxCh {
    pub conf0: u32, pub tx_lim: u32, pub carrier: u32,
    pub running: bool, pub rd: usize, pub since_thr: u32, pub wr: usize,
    pub acc_cycles: i64,
    pub bits: Vec<bool>,
}
pub struct Rmt {
    pub ch: [RmtTxCh; 4],
    pub mem: [u32; RMT_MEM_WORDS * 8],
    pub int_raw: u32, pub int_ena: u32, pub sys_conf: u32,
    ram: RegRam,
    /// completed transmissions: (channel, bits)
    pub done: Vec<(usize, Vec<bool>)>,
    pub tx_count: u64,
    pub dma_fifo: VecDeque<u32>,
    cpu_per_apb: i64,
}
impl Rmt {
    pub fn new(cpu_hz: u64) -> Self { Rmt { cpu_per_apb: (cpu_hz / crate::APB_HZ) as i64, ch: Default::default(), mem: [0; RMT_MEM_WORDS * 8], int_raw: 0, int_ena: 0, sys_conf: 0, ram: RegRam::new(), done: Vec::new(), tx_count: 0, dma_fifo: VecDeque::new() } }
    pub fn irq(&self) -> bool { self.int_raw & self.int_ena != 0 }
    pub fn read(&self, off: u32) -> u32 {
        match off {
            0x20..=0x2c => { let c = &self.ch[((off - 0x20) / 4) as usize]; c.conf0 & !(1 << 0) & !(1 << 1) & !(1 << 2) & !(1 << 23) & !(1 << 24) }
            0x50..=0x5c => { let n = ((off - 0x50) / 4) as usize; let c = &self.ch[n]; ((c.wr as u32 + (n as u32) * 48) << 11) | if c.running { 2 << 22 } else { 0 } }
            0x70 => self.int_raw, 0x74 => self.int_raw & self.int_ena, 0x78 => self.int_ena,
            0x80..=0x8c => self.ch[((off - 0x80) / 4) as usize].carrier,
            0xa0..=0xac => self.ch[((off - 0xa0) / 4) as usize].tx_lim,
            0xc0 => self.sys_conf, 0xcc => 0x2101271,
            0x800..=0xbfc => self.mem[((off - 0x800) / 4) as usize],
            _ => self.ram.read(off),
        }
    }
    pub fn write(&mut self, off: u32, v: u32) {
        match off {
            0x0..=0xc => { let n = ((off) / 4) as usize; let c = &mut self.ch[n]; if c.wr < RMT_MEM_WORDS { self.mem[n * RMT_MEM_WORDS + c.wr] = v; c.wr += 1; } }
            0x20..=0x2c => {
                let n = ((off - 0x20) / 4) as usize;
                let c = &mut self.ch[n];
                c.conf0 = v;
                if n == 3 && (v & (1 << 23) != 0 || v & (1 << 25) == 0) { self.dma_fifo.clear(); }
                if v & (1 << 2) != 0 { c.wr = 0; }                          // APB_MEM_RST
                if v & (1 << 1) != 0 { c.rd = 0; }                          // MEM_RD_RST
                if v & (1 << 0) != 0 { c.running = true; c.rd = 0; c.since_thr = 0; c.acc_cycles = 0; c.bits.clear(); }   // TX_START
                if v & (1 << 7) != 0 { c.running = false; }                 // TX_STOP
            }
            0x78 => self.int_ena = v, 0x7c => self.int_raw &= !v,
            0x80..=0x8c => self.ch[((off - 0x80) / 4) as usize].carrier = v,
            0xa0..=0xac => self.ch[((off - 0xa0) / 4) as usize].tx_lim = v,
            0xc0 => self.sys_conf = v,
            0x800..=0xbfc => self.mem[((off - 0x800) / 4) as usize] = v,
            _ => self.ram.write(off, v),
        }
    }
    /// Advance transmitters by CPU cycles; symbols are consumed at their programmed duration.
    pub fn tick(&mut self, cycles: u64) {
        for n in 0..4 {
            let c = &mut self.ch[n];
            if !c.running { continue; }
            c.acc_cycles += cycles as i64;
            let div = ((c.conf0 >> 8) & 0xff).max(1) as i64;
            let cycles_per_tick = self.cpu_per_apb * div;   // RMT clock = APB 80 MHz / div
            let mem_words = (((c.conf0 >> 16) & 0xf).max(1) as usize) * RMT_MEM_WORDS;
            let base = n * RMT_MEM_WORDS;
            let mut guard = 0;
            while c.acc_cycles > 0 && guard < 4096 {
                guard += 1;
                let dma = n == 3 && c.conf0 & (1 << 25) != 0;
                let sym = if dma {
                    let Some(symbol) = self.dma_fifo.pop_front() else { c.acc_cycles = 0; break; };
                    symbol
                } else { self.mem[base + (c.rd % mem_words)] };
                let (d0, l0, d1, l1) = ((sym & 0x7fff) as i64, sym & 0x8000 != 0, ((sym >> 16) & 0x7fff) as i64, sym & 0x8000_0000 != 0);
                if d0 == 0 { // end marker
                    c.running = false;
                    self.int_raw |= 1 << n;
                    self.tx_count += 1;
                    self.done.push((n, std::mem::take(&mut c.bits)));
                    break;
                }
                // decode WS2812 bit: compare high vs low durations
                let high = if l0 { d0 } else { 0 } + if l1 { d1 } else { 0 };
                let low = if !l0 { d0 } else { 0 } + if !l1 { d1 } else { 0 };
                if c.bits.len() < MAX_CAPTURED_BITS { c.bits.push(high > low); }
                c.acc_cycles -= (d0 + d1) * cycles_per_tick;
                c.rd += 1;
                c.since_thr += 1;
                if !dma && c.tx_lim & 0x1ff != 0 && c.since_thr >= c.tx_lim & 0x1ff { c.since_thr = 0; self.int_raw |= 1 << (8 + n); }   // TX_THR_EVENT
                if d1 == 0 && !l1 && c.rd.is_multiple_of(mem_words) && c.conf0 & (1 << 4) == 0 { /* no wrap: stop at end of memory */ }
            }
        }
    }
}

impl Device for Rmt {
    fn read(&mut self, off: u32) -> u32 { Rmt::read(self, off) }
    fn write(&mut self, off: u32, v: u32) -> WriteEffect { Rmt::write(self, off, v); WriteEffect::NONE }
    fn irq_sources(&self) -> u64 { self.irq() as u64 }
    fn clock(&self) -> Option<ClockDomain> { Some(ClockDomain::Cpu) }
    fn tick(&mut self, cycles: u64) { Rmt::tick(self, cycles) }
    fn has_deadline(&self) -> bool { true }
    fn next_deadline(&self) -> Option<u64> { Some(if self.ch.iter().any(|c| c.running) { 32 } else { u64::MAX }) }
}

/// The C3/C6 RMT: the S3's transmitter IP with two TX and two RX channels on a compacted register
/// map. The TX channels' CONF0 bits and the interrupt bits the model raises (TX_END n, TX_THR
/// 8+n) are the same, so this only maps offsets onto the shared model; the RX channels read back
/// what was written.
pub struct RmtCompact { pub rmt: Rmt, ram: RegRam }
impl RmtCompact {
    pub fn new(cpu_hz: u64) -> Self { RmtCompact { rmt: Rmt::new(cpu_hz), ram: RegRam::new() } }
    fn map(off: u32) -> Option<u32> {
        Some(match off {
            0x00..=0x0c => off,                                   // CHnDATA
            0x10 | 0x14 => 0x20 + (off - 0x10),                   // CH0/1 CONF0 (TX)
            0x28 | 0x2c => 0x50 + (off - 0x28),                   // CH0/1 STATUS
            0x38 => 0x70, 0x3c => 0x74, 0x40 => 0x78, 0x44 => 0x7c,   // INT_RAW / ST / ENA / CLR
            0x48 | 0x4c => 0x80 + (off - 0x48),                   // CH0/1 CARRIER_DUTY
            0x58 | 0x5c => 0xa0 + (off - 0x58),                   // CH0/1 TX_LIM
            0x68 => 0xc0, 0x6c => 0xc4, 0x70 => 0xc8,             // SYS_CONF, TX_SIM, REF_CNT_RST
            0x400..=0x6fc => 0x800 + (off - 0x400),               // symbol memory, 48 words per channel
            _ => return None,
        })
    }
}
impl Device for RmtCompact {
    fn read(&mut self, off: u32) -> u32 { match Self::map(off) { Some(o) => self.rmt.read(o), None => self.ram.read(off) } }
    fn write(&mut self, off: u32, v: u32) -> WriteEffect { match Self::map(off) { Some(o) => self.rmt.write(o, v), None => self.ram.write(off, v) } WriteEffect::NONE }
    fn irq_sources(&self) -> u64 { self.rmt.irq() as u64 }
    fn clock(&self) -> Option<ClockDomain> { Some(ClockDomain::Cpu) }
    fn tick(&mut self, cycles: u64) { self.rmt.tick(cycles) }
    /// A channel mid-transmission raises TX_END/TX_THR from its own symbol clock: while one
    /// runs, time may only be skipped one symbol at a time (the shortest WS2812 symbol is
    /// 0.4 µs; 32 cycles is under that at any RMT divider the led_strip driver uses).
    fn has_deadline(&self) -> bool { true }
    fn next_deadline(&self) -> Option<u64> { Some(if self.rmt.ch.iter().any(|c| c.running) { 32 } else { u64::MAX }) }
}

#[cfg(test)]
mod capture_tests {
    use super::*;
    #[test]
    fn endless_symbol_loop_keeps_observation_memory_bounded() {
        let mut rmt=Rmt::new(240_000_000);
        rmt.mem[..48].fill(0x8000 | 10 | (20<<16));
        rmt.write(0x20,1|(1<<16));
        for _ in 0..100 {rmt.tick(1_000_000);}
        assert_eq!(rmt.ch[0].bits.len(),MAX_CAPTURED_BITS);
        assert!(rmt.ch[0].running);
        assert!(rmt.done.is_empty());
    }
}
