//! Opt-in C3 link controller, derived from the rev3 ROM and Arduino 3.3.11 / IDF 5.5.5.
//! All register fields below are inferred, not public-header definitions or silicon validated.
//! See docs/evidence/ble-full-c3-2026-10-05/polls-disassembly.txt and the phase-1 receipt.
use emu_core::ClockDomain;
use esp_periph::{Device, RegRam, WriteEffect};

const REQUEST: u32 = 1 << 31;
const TIMER: u32 = 1 << 11;
const HALF_US_CYCLES: u64 = crate::periph::CPU_HZ / 2_000_000;
const PERIOD: u64 = (1 << 28) * 625;

#[derive(Default)]
pub struct BleLc {
    ram: RegRam,
    state: Option<Box<State>>,
}

#[derive(Default)]
struct State {
    cycles: u64,
    reset: Option<u64>,
    latch: Option<u64>,
    alarm: Option<u64>,
    raw: u32,
    fifo: u32,
}

impl BleLc {
    pub fn enable(&mut self) { self.state = Some(Box::default()); }
    pub fn enabled(&self) -> bool { self.state.is_some() }
}

impl Device for BleLc {
    fn read(&mut self, off: u32) -> u32 {
        let Some(s) = &self.state else { return self.ram.read(off) };
        match off {
            // Inferred identity required by app r_lld_core_init, load 0x4203ce76.
            0x004 => 0x0900_1b00,
            // Inferred masked status, read by ROM r_rwble_isr (0x4002e8ee).
            0x010 => s.raw & self.ram.read(0x00c),
            // Inferred W1C readback for read/modify/write acknowledgements in r_rwble_isr.
            0x018 => 0,
            // Inferred FIFO: r_rwble_isr_hack at 0x403869a6 extracts count [9:5]
            // and status [30:10], then writes bit 0 to pop. r_lld_core_init resets bit 31.
            0x2d8 => (self.ram.read(off) & 0x1e) | if s.fifo != 0 { 1 << 5 | s.fifo << 10 } else { 0 },
            _ => self.ram.read(off),
        }
    }

    fn write(&mut self, off: u32, v: u32) -> WriteEffect {
        let Some(s) = &mut self.state else {
            self.ram.write(off, v);
            return WriteEffect::NONE;
        };
        match off {
            0x004 | 0x010 | 0x020 => {},
            // Inferred reset/disable completion: r_rwip_driver_init / r_rwble_hw_disable.
            // One half-microsecond latency is a model choice, not a measured hardware delay.
            0x000 => {
                self.ram.write(off, v);
                if v & REQUEST != 0 { s.reset = Some(s.cycles + HALF_US_CYCLES); }
            }
            // Inferred atomic snapshot: r_rwip_time_get polls bit 31, masks 28 bits,
            // and returns 624 minus the fine register. Reads never advance time.
            0x01c => {
                if v & REQUEST != 0 {
                    self.ram.write(off, self.ram.read(off) | REQUEST);
                    s.latch = Some(s.cycles + HALF_US_CYCLES);
                }
            }
            // Inferred W1C: r_rwip_timer_hus_set and r_rwble_isr.
            0x018 => s.raw &= !v,
            0x2d8 => {
                self.ram.write(off, v & 0x1e);
                if v & (REQUEST | 1) != 0 { s.fifo = 0; }
            }
            // Inferred alarm pair: r_rwip_timer_hus_set writes coarse then 624-hus.
            // A target behind now by less than half the counter period is due immediately.
            0x0f0 => {
                self.ram.write(off, v & 0x3ff);
                let target = (self.ram.read(0x0ec) as u64 & 0x0fff_ffff) * 625
                    + 624u64.saturating_sub((v & 0x3ff) as u64);
                let now = s.cycles / HALF_US_CYCLES;
                let delta = (target + PERIOD - now % PERIOD) % PERIOD;
                s.alarm = Some(if delta >= PERIOD / 2 { s.cycles } else { (now + delta) * HALF_US_CYCLES });
            }
            _ => self.ram.write(off, v),
        }
        WriteEffect::NONE
    }

    fn clock(&self) -> Option<ClockDomain> { self.state.as_ref().map(|_| ClockDomain::Cpu) }
    fn has_deadline(&self) -> bool { self.enabled() }
    fn next_deadline(&self) -> Option<u64> {
        let s = self.state.as_ref()?;
        [s.reset, s.latch, s.alarm].into_iter().flatten().min().map(|t| t.saturating_sub(s.cycles).max(1))
    }
    fn irq_sources(&self) -> u64 {
        self.state.as_ref().is_some_and(|s| s.raw & self.ram.read(0x00c) != 0) as u64
    }
    fn tick(&mut self, ticks: u64) {
        let Some(s) = &mut self.state else { return };
        s.cycles += ticks;
        if s.reset.is_some_and(|t| t <= s.cycles) {
            s.reset = None;
            s.alarm = None;
            s.raw = 0;
            s.fifo = 0;
            self.ram.write(0, self.ram.read(0) & !REQUEST);
        }
        if s.latch.is_some_and(|t| t <= s.cycles) {
            // Snapshot at completion, even if the scheduler advances beyond the deadline.
            let hus = s.latch.take().unwrap() / HALF_US_CYCLES;
            self.ram.write(0x01c, (hus / 625) as u32 & 0x0fff_ffff);
            self.ram.write(0x020, 624 - (hus % 625) as u32);
        }
        if s.alarm.is_some_and(|t| t <= s.cycles) {
            s.alarm = None;
            s.raw |= TIMER;
            // ponytail: one timer source can be pending; use a queue when adding event IRQs.
            s.fifo |= TIMER;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latch_is_atomic_and_wraps_in_modeled_time() {
        let mut d = BleLc::default();
        d.enable();
        for hus in [0, 623, 624, 625, PERIOD - 1, PERIOD, PERIOD + 625] {
            d.state.as_mut().unwrap().cycles = hus * HALF_US_CYCLES;
            d.write(0x01c, REQUEST);
            for _ in 0..10 { assert_ne!(d.read(0x01c) & REQUEST, 0); }
            d.tick(HALF_US_CYCLES - 1);
            assert_ne!(d.read(0x01c) & REQUEST, 0);
            d.tick(1);
            let snapshot = (d.read(0x01c), d.read(0x020));
            assert_eq!(snapshot, (((hus + 1) / 625) as u32 & 0x0fff_ffff, 624 - ((hus + 1) % 625) as u32));
            d.tick(1000);
            assert_eq!(snapshot, (d.read(0x01c), d.read(0x020)));
        }
    }

    #[test]
    fn reset_completes_without_polling_and_cancels_alarm() {
        let mut d = BleLc::default();
        d.enable();
        for control in [0x100, 0] {
            d.write(0x0ec, 10);
            d.write(0x0f0, 624);
            d.write(0, REQUEST | control);
            d.tick(HALF_US_CYCLES - 1);
            assert_eq!(d.read(0), REQUEST | control);
            d.tick(1);
            assert_eq!(d.read(0), control);
            assert_eq!(d.read(4), 0x0900_1b00);
            d.tick(1_000_000);
            assert_eq!(d.state.as_ref().unwrap().raw, 0);
        }
    }

    #[test]
    fn alarm_mask_ack_rearm_and_wrap() {
        let mut d = BleLc::default();
        d.enable();
        for now in [0, PERIOD - 2, PERIOD + 10] {
            d.state.as_mut().unwrap().cycles = now * HALF_US_CYCLES;
            let target = (now + 3) % PERIOD;
            d.write(0x00c, 0);
            d.write(0x0ec, (target / 625) as u32);
            d.write(0x0f0, 624 - (target % 625) as u32);
            assert_eq!(d.next_deadline(), Some(3 * HALF_US_CYCLES));
            d.tick(3 * HALF_US_CYCLES - 1);
            assert_eq!(d.state.as_ref().unwrap().raw, 0);
            d.tick(1);
            assert_eq!(d.state.as_ref().unwrap().raw, TIMER);
            assert_eq!(d.irq_sources(), 0);
            d.write(0x00c, TIMER);
            assert_eq!(d.read(0x010), TIMER);
            assert_eq!(d.irq_sources(), 1);
            assert_eq!(d.read(0x2d8), (TIMER << 10) | (1 << 5));
            d.write(0x2d8, 1);
            assert_eq!(d.read(0x2d8), 0);
            d.write(0x018, 1);
            assert_eq!(d.irq_sources(), 1);
            d.write(0x018, TIMER);
            assert_eq!(d.irq_sources(), 0);
            d.tick(10_000);
            assert_eq!(d.read(0x010), 0);
        }
    }

    #[test]
    fn optional_device_routes_source_eight_only_when_enabled() {
        let mut p = crate::periph::Peripherals::new([0; 6]);
        assert_eq!(p.ble_lc.clock(), None);
        assert!(!p.ble_lc.has_deadline());
        p.write32(0x6003101c, REQUEST);
        p.tick(1000);
        assert_eq!(p.read32(0x6003101c), REQUEST);
        assert!(p.misc.active_optional.is_empty());
        p.ble_lc.enable();
        p.write32(0x600c2020, 9);
        p.write32(0x6003100c, TIMER);
        p.write32(0x600310ec, 1);
        p.write32(0x600310f0, 624);
        p.tick(625 * HALF_US_CYCLES);
        p.refresh_lines();
        assert_eq!(p.source_status()[0] & (1 << 8), 1 << 8);
        assert_ne!(p.intc.lines.level & (1 << 9), 0);
        p.write32(0x60031018, TIMER);
        p.refresh_lines();
        assert_eq!(p.intc.lines.level & (1 << 9), 0);
    }
}
