use crate::gpio::Gpio;
use std::collections::VecDeque;

/// Physical microphone wiring. PDM supplies already converted PCM, not raw bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PcmPins {
    I2s { bclk: u8, ws: u8, data: u8 },
    Pdm { clk: u8, data: u8 },
}

/// A host-clocked stereo PCM16 source. Duplicate mono samples into both lanes.
/// Time advances even when no receiver or DMA channel is running.
pub struct PcmSource {
    pins: PcmPins,
    rate: u32,
    frames: VecDeque<[i16; 2]>,
    phase: u64,
    current: [i16; 2],
    /// RX frames sampled from this source, including silence on underrun.
    pub consumed_frames: u64,
}
impl PcmSource {
    pub fn new(rate: u32, pins: PcmPins) -> Result<Self, &'static str> {
        let valid = match pins {
            PcmPins::I2s { bclk, ws, data } => {
                bclk < 49 && ws < 49 && data < 49 && bclk != ws && bclk != data && ws != data
            }
            PcmPins::Pdm { clk, data } => clk < 49 && data < 49 && clk != data,
        };
        if !(8000..=96000).contains(&rate) || !valid {
            return Err("PCM source needs 8000..96000 Hz and distinct GPIOs in 0..49");
        }
        Ok(Self {
            pins,
            rate,
            frames: VecDeque::new(),
            phase: 0,
            current: [0; 2],
            consumed_frames: 0,
        })
    }
    pub fn sample_rate(&self) -> u32 {
        self.rate
    }
    pub fn pins(&self) -> PcmPins {
        self.pins
    }
    pub fn queued_frames(&self) -> usize {
        self.frames.len()
    }
    /// Append audio, retaining only the newest two seconds at the host rate.
    pub fn push(&mut self, frames: &[[i16; 2]]) {
        let capacity = self.rate as usize * 2;
        let frames = &frames[frames.len().saturating_sub(capacity)..];
        let discard = (self.frames.len() + frames.len()).saturating_sub(capacity);
        self.frames.drain(..discard);
        self.frames.extend(frames);
    }
    pub fn clear(&mut self) {
        self.frames.clear();
        self.current = [0; 2];
        self.phase = 0;
    }
    // Offset within the current device tick. Looking up instead of popping allows
    // two receivers to hear the same timeline without advancing it twice.
    fn sample(&self, cycles: u64, cpu_hz: u64) -> [i16; 2] {
        let due = (self.phase + cycles * u64::from(self.rate)) / cpu_hz;
        if due == 0 {
            self.current
        } else {
            self.frames
                .get((due - 1) as usize)
                .copied()
                .unwrap_or([0; 2])
        }
    }
    fn advance(&mut self, cycles: u64, cpu_hz: u64) {
        self.current = self.sample(cycles, cpu_hz);
        let phase = self.phase + cycles * u64::from(self.rate);
        let due = (phase / cpu_hz).min(self.frames.len() as u64) as usize;
        self.frames.drain(..due);
        self.phase = phase % cpu_hz;
    }
}

/// Shared by all RX ports. Any attached source enables physical routing;
/// with no sources, the existing per-port `I2s::rx_input` remains in use.
/// Slots, audio and clock phase survive chip reset. Lowest matching slot wins.
#[derive(Default)]
pub struct PcmSources {
    pub inputs: [Option<PcmSource>; 16],
}
impl PcmSources {
    pub(crate) fn active(&self) -> bool {
        self.inputs.iter().any(Option::is_some)
    }
    /// Advance once after all receivers have sampled this device tick.
    pub fn advance(&mut self, cycles: u64, cpu_hz: u64) {
        for source in self.inputs.iter_mut().flatten() {
            source.advance(cycles, cpu_hz);
        }
    }
    pub(crate) fn select(&self, gpio: &Gpio, signals: RxSignals, pdm: bool) -> Option<usize> {
        let RxSignals {
            data,
            input_select_bit,
            output_mask,
        } = signals;
        self.inputs.iter().position(|source| {
            let Some(source) = source else {
                return false;
            };
            let (din, bclk, ws) = match (source.pins, pdm) {
                (PcmPins::I2s { bclk, ws, data }, false) => (data, bclk, Some(ws)),
                (PcmPins::Pdm { clk, data }, true) => (data, clk, None),
                _ => return false,
            };
            gpio.func_in_sel[data] & ((1 << (input_select_bit + 1)) - 1)
                == u32::from(din) | (1 << input_select_bit)
                && gpio.func_out_sel[bclk as usize] & output_mask
                    == data as u32 + if pdm { 2 } else { 1 }
                && ws.is_none_or(|ws| {
                    gpio.func_out_sel[ws as usize] & output_mask == data as u32 + 2
                })
        })
    }
    pub(crate) fn sample(&mut self, id: usize, cycles: u64, cpu_hz: u64) -> [i16; 2] {
        let source = self.inputs[id].as_mut().unwrap();
        source.consumed_frames = source.consumed_frames.wrapping_add(1);
        // ponytail: zero-order resampling; add an anti-alias filter for audio-fidelity work.
        source.sample(cycles, cpu_hz)
    }
}

/// Chip-specific GPIO matrix encoding for an RX controller.
#[derive(Clone, Copy)]
pub struct RxSignals {
    pub data: usize,
    pub input_select_bit: u32,
    pub output_mask: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_clock_bound_and_underrun() {
        let pins = PcmPins::I2s {
            bclk: 4,
            ws: 5,
            data: 6,
        };
        assert!(PcmSource::new(0, pins).is_err());
        assert!(PcmSource::new(8000, PcmPins::Pdm { clk: 49, data: 1 }).is_err());
        assert!(PcmSource::new(8000, PcmPins::Pdm { clk: 1, data: 1 }).is_err());
        let mut source = PcmSource::new(8000, pins).unwrap();
        source.push(&[[1; 2], [2; 2], [3; 2]]);
        assert_eq!(source.sample(9999, 80_000_000), [0; 2]);
        assert_eq!(source.sample(10000, 80_000_000), [1; 2]);
        source.advance(15000, 80_000_000);
        assert_eq!(source.queued_frames(), 2);
        assert_eq!(source.sample(4999, 80_000_000), [1; 2]);
        assert_eq!(source.sample(5000, 80_000_000), [2; 2]);
        source.advance(25000, 80_000_000);
        assert_eq!(source.sample(0, 80_000_000), [0; 2]);
        source.push(&vec![[11; 2]; 16000]);
        source.push(&[[22; 2]]);
        assert_eq!(source.queued_frames(), 16000);
        assert_eq!(source.sample(16000 * 10000, 80_000_000), [22; 2]);
        source.push(&vec![[33; 2]; 16001]);
        assert_eq!(source.queued_frames(), 16000);
        assert_eq!(source.sample(10000, 80_000_000), [33; 2]);
        source.advance(80_000_000 * 3, 80_000_000);
        assert_eq!(source.queued_frames(), 0);
        assert_eq!(source.current, [0; 2]);
        source.clear();
        assert_eq!(source.phase, 0);
    }

    #[test]
    fn resampling_is_tick_independent_and_shared_by_receivers() {
        use crate::i2s::I2s;
        let run = |ticks: &[u64]| {
            let mut sources = PcmSources::default();
            let mut input = PcmSource::new(
                16000,
                PcmPins::I2s {
                    bclk: 4,
                    ws: 5,
                    data: 6,
                },
            )
            .unwrap();
            input.push(&(1..=16).map(|sample| [sample; 2]).collect::<Vec<_>>());
            sources.inputs[0] = Some(input);
            let mut gpio = Gpio::new();
            gpio.func_in_sel[25] = 6 | (1 << 7);
            gpio.func_out_sel[4] = 26;
            gpio.func_out_sel[5] = 27;
            let signals = RxSignals {
                data: 25,
                input_select_bit: 7,
                output_mask: 0x3ff,
            };
            let mut receivers = [I2s::new(160_000_000), I2s::new(160_000_000)];
            for rx in &mut receivers {
                rx.write(0x30, (1 << 26) | (2 << 27) | 25);
                rx.write(0x28, (24 << 7) | (15 << 13) | (15 << 18));
                rx.write(0x50, (1 << 16) | 3);
                rx.write(0x20, 4);
            }
            let mut output = [Vec::new(), Vec::new()];
            for &cycles in ticks {
                for (rx, output) in receivers.iter_mut().zip(&mut output) {
                    output.extend(rx.rx_routed_data(cycles, false, &gpio, signals, &mut sources));
                }
                sources.advance(cycles, 160_000_000);
            }
            assert_eq!(output[0], output[1]);
            let source = sources.inputs[0].as_ref().unwrap();
            assert_eq!(source.queued_frames(), 8);
            assert_eq!(source.consumed_frames, 8);
            output[0].clone()
        };
        let expected: Vec<_> = [2i16, 4, 6, 8]
            .into_iter()
            .flat_map(|n| [n, n].into_iter().flat_map(i16::to_le_bytes))
            .collect();
        assert_eq!(run(&[80000]), expected);
        assert_eq!(run(&[1, 9998, 20002, 17000, 33000 - 1]), expected);
    }

    #[test]
    fn routing_checks_every_wire_and_inversion() {
        for (data, bit, mask) in [
            (25, 7, 0x3ff),
            (30, 7, 0x3ff),
            (15, 6, 0x1ff),
            (15, 7, 0x1ff),
        ] {
            let signals = RxSignals {
                data,
                input_select_bit: bit,
                output_mask: mask,
            };
            let mut sources = PcmSources::default();
            sources.inputs[15] = Some(
                PcmSource::new(
                    8000,
                    PcmPins::I2s {
                        bclk: 4,
                        ws: 5,
                        data: 6,
                    },
                )
                .unwrap(),
            );
            let mut gpio = Gpio::new();
            gpio.func_in_sel[data] = 6 | (1 << bit);
            gpio.func_out_sel[4] = data as u32 + 1;
            gpio.func_out_sel[5] = data as u32 + 2;
            assert_eq!(sources.select(&gpio, signals, false), Some(15));
            assert_eq!(sources.select(&gpio, signals, true), None);
            for pin in [4, 5] {
                gpio.func_out_sel[pin] ^= 1;
                assert_eq!(sources.select(&gpio, signals, false), None);
                gpio.func_out_sel[pin] ^= 1;
            }
            gpio.func_in_sel[data] ^= 1 << (bit - 1);
            assert_eq!(sources.select(&gpio, signals, false), None);
            gpio.func_in_sel[data] ^= 1 << (bit - 1);
            gpio.func_in_sel[data] ^= 1 << bit;
            assert_eq!(sources.select(&gpio, signals, false), None);
            gpio.func_in_sel[data] ^= 1 << bit;
            sources.inputs[15] =
                Some(PcmSource::new(8000, PcmPins::Pdm { clk: 4, data: 6 }).unwrap());
            gpio.func_out_sel[4] = data as u32 + 2;
            gpio.func_out_sel[5] = 0;
            assert_eq!(sources.select(&gpio, signals, true), Some(15));
            assert_eq!(sources.select(&gpio, signals, false), None);
        }
    }
}
