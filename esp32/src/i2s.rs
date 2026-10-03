//! Classic I2S RX register adapter and native descriptor DMA.
use esp_periph::{Device, RegRam, WriteEffect};
use esp_periph::i2s::{I2s, PcmPins, PcmSources};
use crate::periph::{ClassicGpio, CPU_HZ};

pub struct ClassicI2s {
    pub inner: I2s,
    ram: RegRam,
    pub(crate) descriptor: u32,
    pub(crate) offset: usize,
}
impl Default for ClassicI2s { fn default() -> Self { Self::new() } }
impl ClassicI2s {
    pub fn new() -> Self {
        Self { inner: I2s::new(CPU_HZ), ram: RegRam::new(), descriptor: 0, offset: 0 }
    }
    fn sync(&mut self) {
        let conf = self.ram.read(8);
        let clock = self.ram.read(0xac);
        let sample = self.ram.read(0xb0);
        let bits = (sample >> 18) & 63;
        let bck = (sample >> 6) & 63;
        let (a, b) = ((clock >> 14) & 63, (clock >> 8) & 63);
        let div = if b == 0 || a < b { 0 } else { b | ((a % b) << 9) | ((a / b - 1) << 18) };
        let supported = clock & (1 << 21) == 0 && bck != 0 && [16, 24, 32].contains(&bits);
        self.inner.write(0x30, (clock & 255) | (2 << 27) | if supported { 1 << 26 } else { 0 });
        self.inner.write(0x38, div);
        self.inner.write(0x28, (bck.saturating_sub(1) << 7) | (bits.saturating_sub(1) << 13) | ((if bits <= 16 { 15 } else { 31 }) << 18));
        let channel = (self.ram.read(0x2c) >> 3) & 3;
        self.inner.write(0x50, (1 << 16) | match channel { 1 => 2, 2 => 1, _ => 3 });
        self.inner.write(0x20, ((conf >> 3) & 4) | ((conf >> 1) & 3) | ((conf & (1 << 7)) >> 4));
    }
    pub(crate) fn data(&mut self, cycles: u64, port: usize, gpio: &ClassicGpio, sources: &mut PcmSources) -> Vec<u8> {
        let (data, bclk, ws) = [(155, 27, 28), (181, 164, 165)][port];
        let selected = sources.inputs.iter().position(|source| source.as_ref().is_some_and(|s| {
            let PcmPins::I2s { data: d, bclk: b, ws: w } = s.pins() else { return false; };
            gpio.input_pin(data) == Some(d) && gpio.output_pins(bclk) & (1 << b) != 0 && gpio.output_pins(ws) & (1 << w) != 0
        }));
        self.inner.rx_selected_data(cycles, false, sources, selected)
    }
    pub(crate) fn complete(&mut self, descriptor: u32) {
        self.ram.write(0x3c, descriptor);
        self.inner.int_raw |= 1 << 9;
    }
    pub(crate) fn fault(&mut self) { self.inner.int_raw |= 1 << 13; self.descriptor = 0; }
}
impl Device for ClassicI2s {
    fn read(&mut self, off: u32) -> u32 {
        match off {
            0xc | 0x10 | 0x14 => self.inner.read(off),
            8 => self.ram.read(off) & !15,
            0xbc => 1,
            _ => self.ram.read(off),
        }
    }
    fn write(&mut self, off: u32, value: u32) -> WriteEffect {
        self.ram.write(off, value);
        match off {
            0x14 | 0x18 => self.inner.write(off, value),
            0x34 => {
                if value & (1 << 28) != 0 { self.descriptor = 0; }
                if value & (3 << 29) != 0 { self.descriptor = 0x3ff0_0000 | (value & 0xfffff); self.offset = 0; }
            }
            8 | 0x2c | 0xac | 0xb0 => self.sync(),
            _ => {}
        }
        WriteEffect::NONE
    }
    fn irq_sources(&self) -> u64 { self.inner.irq() as u64 }
}

impl crate::bus::SocBus {
    pub(crate) fn i2s_step(&mut self, cycles: u64) {
        for port in 0..2 {
            let clock_bit = if port == 0 { 4 } else { 21 };
            if self.periph.dport.ram.read(0xc0) & (1 << clock_bit) == 0 || self.periph.i2s[port].descriptor == 0 { continue; }
            let data = self.periph.i2s[port].data(cycles, port, &self.periph.gpio, &mut self.pcm_sources);
            for byte in data {
                let desc = self.periph.i2s[port].descriptor;
                let off = self.periph.i2s[port].offset;
                let Some(control) = self.dma_read_word(desc) else { self.periph.i2s[port].fault(); break; };
                let size = (control & 0xfff) as usize;
                let buf = self.dma_read_word(desc + 4).unwrap_or(0);
                if control & (1 << 31) == 0 || off >= size || !self.dma_write_byte(buf.wrapping_add(off as u32), byte) {
                    self.periph.i2s[port].fault(); break;
                }
                self.periph.i2s[port].offset += 1;
                if off + 1 == size {
                    self.dma_write_word(desc, (control & !0x80ff_f000) | ((size as u32) << 12) | (1 << 30));
                    self.periph.i2s[port].complete(desc);
                    self.periph.i2s[port].descriptor = self.dma_read_word(desc + 8).unwrap_or(0);
                    self.periph.i2s[port].offset = 0;
                    self.irq_dirty = true;
                }
            }
        }
        self.pcm_sources.advance(cycles, CPU_HZ);
    }
}
