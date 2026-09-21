//! Register-driven 12-bit ADC oneshot conversions. Host inputs are raw codes after attenuation;
//! voltage transfer curves and analog calibration are not inferred from a raw-code control.
use crate::{Device, RegRam, WriteEffect};
use emu_core::ClockDomain;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdcLayout { S3, C3, C6 }

#[derive(Clone, Copy)]
struct Conversion { sample: u16, channel: usize, remaining: u64 }

#[derive(Clone, Copy, Default)]
pub struct AdcSample { pub generation: u32, pub counts: u16 }

pub struct Adc {
    layout: AdcLayout,
    pub inputs: [[u16; 10]; 2],
    pub sampled: [[AdcSample; 10]; 2],
    ram: RegRam,
    pending: [Option<Conversion>; 2],
    raw: u32,
    pub audio: [[Option<crate::pcm::PcmInput>; 10]; 2],
}
impl Adc {
    pub fn new(layout: AdcLayout) -> Self {
        Self { layout, inputs: [[0; 10]; 2], sampled: [[AdcSample::default(); 10]; 2], ram: RegRam::new(), pending: [None; 2], raw: 0, audio: std::array::from_fn(|_| std::array::from_fn(|_| None)) }
    }
    pub fn set_input(&mut self, pin: u32, value: u32) -> bool {
        if value > 4095 { return false; }
        let Some((unit, channel)) = self.channel(pin) else { return false; };
        self.inputs[unit][channel] = value as u16;
        true
    }
    pub fn observation(&self, pin: u32) -> Option<AdcSample> {
        let (unit, channel) = self.channel(pin)?;
        Some(self.sampled[unit][channel])
    }
    pub fn audio_input(&mut self, pin: u32) -> Option<&mut Option<crate::pcm::PcmInput>> {
        let (unit, channel) = self.channel(pin)?;
        Some(&mut self.audio[unit][channel])
    }
    fn channel(&self, pin: u32) -> Option<(usize, usize)> {
        match (self.layout, pin) {
            (AdcLayout::S3, 1..=10) => Some((0, (pin - 1) as usize)),
            (AdcLayout::S3, 11..=20) => Some((1, (pin - 11) as usize)),
            (AdcLayout::C3, 0..=4) | (AdcLayout::C6, 0..=6) => Some((0, pin as usize)),
            (AdcLayout::C3, 5) => Some((1, 0)),
            _ => None,
        }
    }
    fn start(&mut self, unit: usize, channel: usize) {
        let channels = match (self.layout, unit) { (AdcLayout::S3, _) => 10, (AdcLayout::C3, 0) => 5, (AdcLayout::C3, 1) => 1, (AdcLayout::C6, 0) => 7, _ => 0 };
        if channel >= channels || self.pending[unit].is_some() { return; }
        let divider = if self.layout == AdcLayout::S3 { self.ram.read(if unit == 0 { 0 } else { 0x24 }) & 0xff } else { (self.ram.read(0) >> 7) & 0xff };
        // ponytail: 12 SAR bits plus two sampling clocks; model the analog clock mux and sampling-cycle register for timed acquisition.
        let remaining = 14 * u64::from(divider.max(1));
        self.pending[unit] = Some(Conversion { sample: self.inputs[unit][channel], channel, remaining });
        self.raw &= !(1 << (31 - unit));
    }
    fn finish(&mut self, unit: usize, channel: usize, sample: u16) {
        let observed = &mut self.sampled[unit][channel];
        observed.generation = if observed.generation >= u32::MAX - 1 { 1 } else { observed.generation + 1 };
        observed.counts = sample;
        if self.layout == AdcLayout::S3 {
            let control = if unit == 0 { 0x0c } else { 0x30 };
            let reader = if unit == 0 { 0 } else { 0x24 };
            let sample = if self.ram.read(reader) & (1 << (28 + unit)) != 0 { sample ^ 0xfff } else { sample };
            self.ram.write(control, (self.ram.read(control) & !0x1ffff) | (1 << 16) | u32::from(sample));
        } else {
            self.ram.write(if unit == 0 { 0x2c } else { 0x30 }, u32::from(sample));
        }
        self.raw |= 1 << (31 - unit);
    }
}
impl Device for Adc {
    fn read(&mut self, off: u32) -> u32 {
        if self.layout != AdcLayout::S3 {
            match off { 0x44 => return self.raw, 0x48 => return self.raw & self.ram.read(0x40), 0x4c => return 0, _ => {} }
        } else if off == 0x50 { return (self.ram.read(off) & !0x1ff) | 0x180; }
        else if off == 0x40 { return (self.ram.read(off) & !(0xff << 22)) | (u32::from(self.pending[0].is_some()) << 22); }
        self.ram.read(off)
    }
    fn write(&mut self, off: u32, value: u32) -> WriteEffect {
        if self.layout == AdcLayout::S3 && (off == 0x0c || off == 0x30) {
            let unit = usize::from(off == 0x30);
            let old = self.ram.read(off);
            self.ram.write(off, (value & !0x1ffff) | (old & 0x1ffff));
            if value & (1 << 17) != 0 && old & (1 << 17) == 0 {
                self.ram.write(off, self.ram.read(off) & !(1 << 16));
                let pads = (value >> 19) & 0x3ff;
                if pads.count_ones() == 1 { self.start(unit, pads.trailing_zeros() as usize); }
            }
        } else if self.layout != AdcLayout::S3 && off == 0x20 {
            let old = self.ram.read(off);
            self.ram.write(off, value);
            if value & (1 << 29) != 0 && old & (1 << 29) == 0 {
                let channel = ((value >> 25) & 0xf) as usize;
                let unit = channel >> 3;
                if value & (1 << (31 - unit)) != 0 { self.start(unit, channel & 7); }
            }
        } else if self.layout != AdcLayout::S3 && off == 0x4c { self.raw &= !value; }
        else if self.layout != AdcLayout::S3 && matches!(off, 0x2c | 0x30 | 0x44 | 0x48) {}
        else { self.ram.write(off, value); }
        WriteEffect::NONE
    }
    fn clock(&self) -> Option<ClockDomain> { Some(ClockDomain::Apb) }
    fn has_deadline(&self) -> bool { true }
    fn next_deadline(&self) -> Option<u64> { self.pending.iter().flatten().map(|conversion| conversion.remaining).min() }
    fn tick(&mut self, ticks: u64) {
        for unit in 0..2 {
            for channel in 0..10 {
                if let Some(pcm) = &mut self.audio[unit][channel] { self.inputs[unit][channel] = pcm.adc_code(ticks); }
            }
        }
        for unit in 0..2 {
            if let Some(mut conversion) = self.pending[unit] {
                conversion.remaining = conversion.remaining.saturating_sub(ticks);
                if conversion.remaining == 0 { self.pending[unit] = None; self.finish(unit, conversion.channel, conversion.sample); }
                else { self.pending[unit] = Some(conversion); }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn conversion_requires_start_and_time_and_latches_selected_channel() {
        for layout in [AdcLayout::S3, AdcLayout::C3, AdcLayout::C6] {
            let mut adc = Adc::new(layout);
            let pin = if layout == AdcLayout::S3 { 1 } else { 0 };
            assert!(adc.set_input(pin, 1024));
            assert!(!adc.set_input(99, 1)); assert!(!adc.set_input(pin, 4096));
            assert_eq!(adc.next_deadline(), None);
            assert_eq!(adc.observation(pin).unwrap().generation, 0);
            assert!(adc.observation(99).is_none());
            if layout == AdcLayout::S3 { adc.write(0, 0); adc.write(0x0c, (1 << 19) | (1 << 17)); }
            else { adc.write(0x20, (1 << 31) | (1 << 29)); }
            assert!(adc.set_input(pin, 3072));
            adc.tick(13);
            let done = if layout == AdcLayout::S3 { adc.read(0x0c) & (1 << 16) } else { adc.read(0x44) };
            assert_eq!(done, 0);
            assert_eq!(adc.observation(pin).unwrap().generation, 0);
            adc.tick(1);
            let observed=adc.observation(pin).unwrap();
            assert_eq!((observed.generation,observed.counts),(1,1024));
            assert_eq!(adc.observation(pin+1).unwrap().generation,0);
            let value = if layout == AdcLayout::S3 { assert_ne!(adc.read(0x0c) & (1 << 16), 0); adc.read(0x0c) & 0xfff } else { assert_eq!(adc.read(0x44), 1 << 31); adc.read(0x2c) };
            assert_eq!(value, 1024);
            assert_eq!(adc.next_deadline(), None);
        }
    }
}

#[cfg(test)]
mod register_tests {
    use super::*;
    #[test]
    fn unit_two_channel_selection_inversion_and_readonly_results() {
        let mut s3 = Adc::new(AdcLayout::S3);
        assert!(s3.set_input(20, 3072));
        s3.write(0x24, 1 << 29);
        s3.write(0x30, (1 << 28) | (1 << 17));
        s3.tick(14);
        assert_eq!(s3.read(0x30) & 0xfff, 1023);
        s3.write(0x30, 0);
        assert_eq!(s3.read(0x30) & 0xfff, 1023);

        let mut c3 = Adc::new(AdcLayout::C3);
        assert!(c3.set_input(5, 3072));
        c3.write(0x20, (8 << 25) | (1 << 30) | (1 << 29));
        c3.tick(14);
        assert_eq!(c3.read(0x30), 3072);
        assert_eq!(c3.read(0x44), 1 << 30);
        c3.write(0x30, 0); assert_eq!(c3.read(0x30), 3072);
        c3.write(0x4c, 1 << 30); assert_eq!(c3.read(0x44), 0);
        c3.write(0x44, u32::MAX); assert_eq!(c3.read(0x44), 0);

        let mut c6 = Adc::new(AdcLayout::C6);
        assert!(c6.set_input(6, 4095)); assert!(!c6.set_input(7, 0));
        c6.write(0x20, (6 << 25) | (1 << 29));
        assert_eq!(c6.next_deadline(), None, "disabled unit must not convert");
        c6.write(0x20, 0);
        c6.write(0x20, (8 << 25) | (1 << 30) | (1 << 29));
        assert_eq!(c6.next_deadline(), None, "C6 has no ADC2");
    }
}
