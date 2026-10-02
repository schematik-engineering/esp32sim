use emu_core::ClockDomain;
use esp_periph::{Device, RegRam, WriteEffect};
use std::collections::VecDeque;

pub struct ParallelFrame {
    pub samples: Vec<u16>,
    pub width: u8,
    pub clock_hz: u32,
}

pub struct Parlio {
    ram: RegRam,
    pub fifo: VecDeque<u8>,
    cfg: u32,
    raw: u32,
    ena: u32,
    remaining: usize,
    samples: Vec<u16>,
    phase: u64,
    pub clock_hz: u32,
    pub done: Option<ParallelFrame>,
}

impl Default for Parlio { fn default() -> Self { Self::new() } }

impl Parlio {
    pub fn new() -> Self {
        Self {
            ram: RegRam::new(),
            fifo: VecDeque::new(),
            cfg: 0,
            raw: 0,
            ena: 0,
            remaining: 0,
            samples: Vec::new(),
            phase: 0,
            clock_hz: 40_000_000,
            done: None,
        }
    }
    fn width(&self) -> u8 {
        16u8.checked_shr((self.cfg >> 27) & 7).unwrap_or(0)
    }
    pub fn running(&self) -> bool {
        self.cfg & (1 << 19) != 0 && self.remaining > 0
    }
    pub fn set_clock(&mut self, value: u32) {
        self.clock_hz = if value & (1 << 18) == 0 {
            0
        } else {
            let source = match (value >> 16) & 3 {
                0 => 40_000_000,
                1 => 240_000_000,
                2 => 17_500_000,
                _ => 0,
            };
            source / ((value & 0xffff) + 1)
        };
        if value & (1 << 19) != 0 {
            self.fifo.clear();
            self.cfg &= !(1 << 19);
            self.remaining = 0;
            self.samples.clear();
        }
    }
}

impl Device for Parlio {
    fn read(&mut self, off: u32) -> u32 {
        match off {
            0x08 => self.cfg,
            0x10 => {
                if !self.fifo.is_empty() {
                    1 << 31
                } else {
                    0
                }
            }
            0x14 => self.ena,
            0x18 => self.raw,
            0x1c => self.raw & self.ena,
            0x3fc => 0x220_2240,
            _ => self.ram.read(off),
        }
    }
    fn write(&mut self, off: u32, value: u32) -> WriteEffect {
        match off {
            0x08 => {
                let start = value & (1 << 19) != 0 && self.cfg & (1 << 19) == 0;
                self.cfg = value;
                if value & (1 << 30) != 0 {
                    self.fifo.clear();
                    self.samples.clear();
                }
                if start {
                    self.remaining = ((value >> 2) & 0xffff) as usize;
                    self.samples.clear();
                    self.phase = 0;
                }
            }
            0x14 => self.ena = value & 7,
            0x20 => self.raw &= !value,
            _ => self.ram.write(off, value),
        }
        WriteEffect::NONE
    }
    fn clock(&self) -> Option<ClockDomain> {
        Some(ClockDomain::Cpu)
    }
    fn irq_sources(&self) -> u64 {
        u64::from(self.raw & self.ena != 0)
    }
    fn has_deadline(&self) -> bool {
        true
    }
    fn next_deadline(&self) -> Option<u64> {
        Some(if self.running() { 16 } else { u64::MAX })
    }
    fn tick(&mut self, cycles: u64) {
        let width = self.width();
        if !self.running() || width == 0 || self.clock_hz == 0 {
            return;
        }
        self.phase = self
            .phase
            .saturating_add(cycles.saturating_mul(u64::from(self.clock_hz)));
        let bytes = if width == 16 { 2 } else { 1 };
        let count = 8 * bytes / usize::from(width);
        let cost = 160_000_000 * count as u64;
        while self.phase >= cost && self.remaining >= bytes {
            if self.fifo.len() < bytes {
                self.phase = 0;
                break;
            }
            self.phase -= cost;
            let mut value = u16::from(self.fifo.pop_front().unwrap());
            if bytes == 2 {
                value |= u16::from(self.fifo.pop_front().unwrap()) << 8;
            }
            let mask = u16::MAX >> (16 - width);
            for i in 0..count {
                let shift = if self.cfg & (1 << 26) == 0 {
                    i
                } else {
                    count - i - 1
                } * usize::from(width);
                self.samples.push((value >> shift) & mask);
            }
            self.remaining -= bytes;
            if self.remaining == 0 {
                self.raw |= 4;
                self.cfg &= !(1 << 19);
                self.done = Some(ParallelFrame {
                    samples: std::mem::take(&mut self.samples),
                    width,
                    clock_hz: self.clock_hz,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fifo_readiness_timed_lane_samples_and_eof_interrupt() {
        let mut p = Parlio::new();
        p.set_clock((1 << 18) | (1 << 16) | 59); // 240MHz / 60
        assert_eq!(p.read(0x10), 0);
        p.fifo.extend([0b11_10_01_00, 0b00_01_10_11]);
        assert_eq!(p.read(0x10), 1 << 31);
        p.write(0x14, 4);
        p.write(0x08, (2 << 2) | (1 << 19) | (3 << 27) | (1 << 26));
        p.tick(159);
        assert!(p.done.is_none());
        p.tick(1);
        assert_eq!(p.samples, vec![3, 2, 1, 0]);
        p.tick(160);
        let frame = p.done.take().unwrap();
        assert_eq!(frame.samples, vec![3, 2, 1, 0, 0, 1, 2, 3]);
        assert_eq!(frame.width, 2);
        assert_eq!(frame.clock_hz, 4_000_000);
        assert_eq!(p.irq_sources(), 1);
        p.write(0x20, 4);
        assert_eq!(p.irq_sources(), 0);
        p.write(0x08, (1 << 2) | (1 << 19) | (3 << 27));
        p.fifo.push_back(0b11_10_01_00);
        p.tick(160);
        assert_eq!(p.done.take().unwrap().samples, vec![0, 1, 2, 3]);
    }
    #[test]
    fn absent_clock_and_fifo_do_not_complete_transfer() {
        let mut p = Parlio::new();
        p.write(0x08, (1 << 2) | (1 << 19) | (1 << 27));
        p.tick(1000);
        assert!(p.done.is_none());
        assert!(p.running());
        p.set_clock(0);
        p.fifo.push_back(5);
        p.tick(1000);
        assert!(p.done.is_none());
        assert_eq!(p.read(0x10), 1 << 31);
        p.set_clock(1 << 18);
        p.tick(4);
        assert_eq!(p.done.take().unwrap().samples, vec![5]);
    }
}
