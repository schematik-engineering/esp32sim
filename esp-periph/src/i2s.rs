//! I2S clocks, TX sample capture and GPIO-routed standard PCM RX through GDMA.
use crate::device::{Device, WriteEffect};
use crate::regram::RegRam;
use crate::{gpio::Gpio, pcm::PcmInput, gdma::GdmaInCh};
use emu_core::Bus;


pub struct I2s {
    pub rx_conf: u32, pub tx_conf: u32, pub int_raw: u32, pub int_ena: u32,
    ram: RegRam,
    /// TX_CONF1 (0x2c: slot width, BCK divider), TX_CLKM_CONF (0x34: source, integer MCLK divider),
    /// TX_CLKM_DIV_CONF (0x3c: fractional MCLK divider x/y/z/yn1), TX_TDM_CTRL (0x54: slot count)
    pub tx_conf1: u32, pub tx_clkm_conf: u32, pub tx_clkm_div_conf: u32, pub tx_tdm_ctrl: u32,
    /// frame rate on the wire, derived from the clock registers exactly as the silicon divides
    /// its source clock; 44.1 kHz until firmware programs the clock
    pub sample_rate: u32,
    pub bytes_per_frame: u32,
    acc: u64,
    /// decoded left-channel samples (host sink)
    pub pcm: Vec<i16>,
    pub frames_out: u64,
    pub tx_started_log: bool,
    cpu_hz: u64,
    pub inputs: [Option<PcmInput>; 16],
    pub selected_input: Option<usize>,
    rx_acc: u64,
}
impl I2s {
    pub fn new(cpu_hz: u64) -> Self { I2s { cpu_hz, inputs: std::array::from_fn(|_| None), selected_input: None, rx_acc: 0, rx_conf: 0, tx_conf: 0, int_raw: 0, int_ena: 0, ram: RegRam::new(), tx_conf1: 0, tx_clkm_conf: 0, tx_clkm_div_conf: 0, tx_tdm_ctrl: 0, sample_rate: 44100, bytes_per_frame: 4, acc: 0, pcm: Vec::new(), frames_out: 0, tx_started_log: false } }
    pub fn tx_running(&self) -> bool { self.tx_conf & (1 << 2) != 0 }
    pub fn read(&self, off: u32) -> u32 {
        match off {
            0xc => self.int_raw, 0x10 => self.int_raw & self.int_ena, 0x14 => self.int_ena,
            0x20 => self.rx_conf & !(1 << 8) & !3, 0x24 => self.tx_conf & !(1 << 8) & !3,   // update/reset bits self-clear
            0x6c => if self.tx_running() { 0 } else { 1 },                                   // STATE: tx_idle
            0x80 => 0x2003070,
            _ => self.ram.read(off),
        }
    }
    pub fn write(&mut self, off: u32, v: u32) {
        match off {
            0x14 => self.int_ena = v, 0x18 => self.int_raw &= !v,
            0x20 => { self.rx_conf = v; if v & 3 != 0 { self.rx_acc = 0; } }, 0x24 => { self.tx_conf = v; self.update_rate(); }
            0x2c => { self.tx_conf1 = v; self.ram.write(off, v); self.update_rate(); }
            0x34 => { self.tx_clkm_conf = v; self.ram.write(off, v); self.update_rate(); }
            0x3c => { self.tx_clkm_div_conf = v; self.ram.write(off, v); self.update_rate(); }
            0x54 => { self.tx_tdm_ctrl = v; self.ram.write(off, v); self.update_rate(); }
            _ => self.ram.write(off, v),
        }
    }
    pub fn irq(&self) -> bool { self.int_raw & self.int_ena != 0 }
    /// The TX frame rate the clock tree produces:
    ///   MCLK = src / (div_num + b/a)   with b/a recovered from the x/y/z/yn1 fields the way
    ///                                  `i2s_ll_tx_set_mclk` encodes them (b = z, a = (x+1)·z + y;
    ///                                  with yn1 the fraction is 1 − b/a)
    ///   BCK  = MCLK / (bck_div_num + 1)
    ///   fs   = BCK / (slot_bits · slots)
    /// Returns None while the clock is off or unprogrammed, so the default stays in force.
    pub fn derive_rate(&self) -> Option<u32> {
        Self::clock_rate(self.tx_clkm_conf, self.tx_clkm_div_conf, self.tx_conf1, self.tx_tdm_ctrl, false)
    }
    fn clock_rate(c: u32, d: u32, conf1: u32, tdm: u32, rx: bool) -> Option<u32> {
        if c & (1 << 26) == 0 { return None; }                                   // TX_CLK_ACTIVE
        let src: u64 = match (c >> 27) & 3 { 0 => 40_000_000, 1 => 240_000_000, 2 => 160_000_000, _ => return None };   // XTAL / PLL_F240M / PLL_F160M / external
        let n = (c & 0xff) as u64;
        if n == 0 { return None; }

        let (z, y, x, yn1) = ((d & 0x1ff) as u64, ((d >> 9) & 0x1ff) as u64, ((d >> 18) & 0x1ff) as u64, d & (1 << 27) != 0);
        let (a, b) = if z == 0 { (1, 0) } else { let a = (x + 1) * z + y; (a, if yn1 { a - z } else { z }) };
        let bck = ((conf1 >> 7) & 0x3f) as u64 + 1;
        let slot_bits = (if rx { (conf1 >> 24) & 0x1f } else { conf1 & 0x7f }) as u64 + 1;                       // TX_TDM_WS_WIDTH = slot width − 1
        let slots = ((tdm >> 16) & 0xf) as u64 + 1;                 // TX_TDM_TOT_CHAN_NUM = slots − 1
        let denom = (n * a + b) * bck * slot_bits * slots;
        if denom == 0 { return None; }
        let fs = (src * a + denom / 2) / denom;
        if !(1_000..=400_000).contains(&fs) { return None; }
        Some(fs as u32)
    }
    pub fn rx_running(&self) -> bool { self.rx_conf & 4 != 0 }
    pub fn rx_bits(&self) -> u32 { ((self.ram.read(0x28) >> 13) & 0x1f) + 1 }
    pub fn rx_channels(&self) -> u32 { if self.rx_conf & (1 << 5) != 0 { 1 } else { (self.ram.read(0x50) & 0xffff).count_ones() } }
    pub fn rx_rate(&self) -> u32 { Self::clock_rate(self.ram.read(0x30), self.ram.read(0x38), self.ram.read(0x28), self.ram.read(0x50), true).unwrap_or(0) }
    pub fn rx_eof_bytes(&self) -> u32 { self.ram.read(0x64) }
    /// C6 moves MCLK source/dividers into PCR with a different bit layout.
    pub fn rx_pcr_clock(&mut self, conf: u32, div: u32) {
        self.ram.write(0x30, ((conf >> 12) & 0xff) | (((conf >> 20) & 3) << 27) | (((conf >> 22) & 1) << 26));
        self.ram.write(0x38, div);
    }
    pub fn rx_data(&mut self, cycles: u64, gpio: &Gpio, data_signal: usize, input_select_bit: u32, output_mask: u32) -> Vec<u8> {
        let rate = self.rx_rate();
        let bits = self.rx_bits();
        let channels = self.rx_channels();
        let tdm = self.ram.read(0x50);
        let supported = self.rx_running() && rate != 0 && [16, 24, 32].contains(&bits) && (1..=2).contains(&channels)
            && self.rx_conf & ((1 << 3) | (1 << 7) | (3 << 10) | (1 << 18) | (1 << 20)) == 0 && (tdm >> 16) & 0xf == 1;
        self.selected_input = if supported { self.inputs.iter().position(|input| {
            let Some(input) = input else { return false; };
            let [data, bclk, ws] = input.pins;
            gpio.func_in_sel[data_signal] & ((1 << (input_select_bit + 1)) - 1) == data | (1 << input_select_bit)
                && gpio.func_out_sel[bclk as usize] & output_mask == data_signal as u32 + 1
                && gpio.func_out_sel[ws as usize] & output_mask == data_signal as u32 + 2
        }) } else { None };
        for (id, input) in self.inputs.iter_mut().enumerate() {
            if Some(id) != self.selected_input {
                if let Some(input) = input { input.advance(cycles, self.cpu_hz); }
            }
        }
        let Some(id) = self.selected_input else { self.rx_acc = 0; return Vec::new(); };
        let input = self.inputs[id].as_mut().unwrap();
        self.rx_acc += cycles * u64::from(rate);
        let frames = self.rx_acc / self.cpu_hz;
        self.rx_acc %= self.cpu_hz;
        let mut bytes = Vec::new();
        for _ in 0..frames {
            // ponytail: zero-order resampling; add an anti-alias filter for audio-fidelity work.
            let frame = input.advance(1, u64::from(rate));
            for lane in 0..2 {
                if tdm & (1 << lane) == 0 { continue; }
                if bits == 16 { bytes.extend_from_slice(&frame[lane].to_le_bytes()); }
                else { bytes.extend_from_slice(&(i32::from(frame[lane]) << 16).to_le_bytes()); }
                if channels == 1 { break; }
            }
        }
        bytes
    }
    fn update_rate(&mut self) { if let Some(fs) = self.derive_rate() { self.sample_rate = fs; } }
    /// Number of frames due after `cycles` CPU cycles at the configured sample rate.
    pub fn frames_due(&mut self, cycles: u64) -> u32 {
        if !self.tx_running() { self.acc = 0; return 0; }
        self.acc += cycles * self.sample_rate as u64;
        let n = (self.acc / self.cpu_hz) as u32;
        self.acc %= self.cpu_hz;
        n
    }
}

impl Device for I2s {
    fn read(&mut self, off: u32) -> u32 { I2s::read(self, off) }
    fn write(&mut self, off: u32, v: u32) -> WriteEffect { I2s::write(self, off, v); WriteEffect::NONE }
    fn irq_sources(&self) -> u64 { self.irq() as u64 }
}

/// Peripheral RX bytes enter memory through firmware-owned GDMA descriptors.
pub fn receive_dma(bus: &mut impl Bus, channel: &mut GdmaInCh, bytes: &[u8], eof_bytes: u32) {
    let mut pos = 0;
    while pos < bytes.len() && channel.running && channel.desc != 0 {
        let result = (|| {
            let dw0 = bus.read32(channel.desc)?;
            let size = dw0 & 0xfff;
            if size == 0 || eof_bytes == 0 || channel.eof_pos >= eof_bytes || channel.buf_pos >= size || (channel.conf1 & (1 << 12) != 0 && dw0 & (1 << 31) == 0) {
                return Err(emu_core::Fault::Unmapped);
            }
            let buffer = bus.read32(channel.desc + 4)?;
            let next = bus.read32(channel.desc + 8)?;
            let take = (size - channel.buf_pos).min(eof_bytes - channel.eof_pos).min((bytes.len() - pos) as u32);
            for byte in &bytes[pos..pos + take as usize] {
                bus.write8(buffer.wrapping_add(channel.buf_pos), *byte)?;
                channel.buf_pos += 1;
            }
            pos += take as usize;
            channel.eof_pos += take;
            let eof = channel.eof_pos == eof_bytes;
            if channel.buf_pos == size || eof {
                bus.write32(channel.desc, (dw0 & !(0xfff << 12) & !(3 << 30)) | (channel.buf_pos << 12) | if eof { 1 << 30 } else { 0 })?;
                channel.int_raw |= 1; // IN_DONE
                if eof { channel.int_raw |= 2; channel.eof_desc = channel.desc; channel.eof_pos = 0; }
                channel.desc = next;
                channel.buf_pos = 0;
                if next == 0 { channel.running = false; }
            }
            Ok::<(), emu_core::Fault>(())
        })();
        if result.is_err() { channel.int_raw |= 1 << 3; channel.running = false; break; }
    }
}

#[cfg(test)]
mod rx_tests {
    use super::*;
    #[test]
    fn rx_respects_clock_wiring_format_and_dma_ownership() {
        let mut i2s = I2s::new(160_000_000);
        i2s.inputs[0] = PcmInput::new(16000, 2, [4, 5, 6]);
        i2s.inputs[0].as_mut().unwrap().push(&[0xd2, 4, 0xd7, 0xf6, 0xd2, 4, 0xd7, 0xf6]);
        i2s.write(0x30, (1 << 26) | (2 << 27) | 25);
        i2s.write(0x28, 15 | (24 << 7) | (15 << 13) | (15 << 24));
        i2s.write(0x50, (1 << 16) | 3);
        i2s.write(0x64, 4);
        assert_eq!(i2s.rx_rate(), 8000);
        let mut gpio = Gpio::new();
        gpio.func_in_sel[15] = 4 | 0x80;
        gpio.func_out_sel[5] = 16;
        gpio.func_out_sel[6] = 17;
        assert!(i2s.rx_data(20000, &gpio, 15, 7, 0x3ff).is_empty());
        i2s.write(0x20, 4);
        assert!(i2s.rx_data(20000, &gpio, 25, 7, 0x3ff).is_empty());
        i2s.inputs[0].as_mut().unwrap().push(&[0xd2, 4, 0xd7, 0xf6, 0xd2, 4, 0xd7, 0xf6]);
        let bytes = i2s.rx_data(20000, &gpio, 15, 7, 0x3ff);
        assert_eq!(bytes, [0xd2, 4, 0xd7, 0xf6]);
        i2s.write(0x20, 4 | (1 << 7));
        assert!(i2s.rx_data(20000, &gpio, 15, 7, 0x3ff).is_empty());
        i2s.write(0x20, 4);
        gpio.func_out_sel[5] |= 1 << 9;
        assert!(i2s.rx_data(20000, &gpio, 15, 7, 0x3ff).is_empty());
        assert_eq!(i2s.selected_input, None);
        let mut ram = emu_core::FlatRam::new(0, 256);
        ram.write32(16, 2 | (1 << 31)).unwrap(); ram.write32(20, 128).unwrap(); ram.write32(24, 32).unwrap();
        ram.write32(32, 2 | (1 << 31)).unwrap(); ram.write32(36, 130).unwrap();
        let mut channel = GdmaInCh { running: true, desc: 16, conf1: 1 << 12, ..Default::default() };
        receive_dma(&mut ram, &mut channel, &bytes[..2], 4);
        assert_eq!(channel.int_raw, 1);
        assert_eq!(ram.read32(16).unwrap(), 2 | (2 << 12));
        receive_dma(&mut ram, &mut channel, &bytes[2..], 4);
        assert_eq!(channel.int_raw, 3);
        assert_eq!(channel.eof_desc, 32);
        assert_eq!(&ram.mem[128..132], &bytes);
        channel.running = true; channel.desc = 16; channel.int_raw = 0;
        receive_dma(&mut ram, &mut channel, &bytes, 4);
        assert_eq!(channel.int_raw, 8);
        assert!(!channel.running);
    }
}
