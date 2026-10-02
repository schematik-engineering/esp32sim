use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Host-clocked mono PCM16, as voltage or post-attenuation raw ADC counts.
/// Attach a clone through `AnalogSource::Stream`; keep a handle for host pushes.
/// All timestamps use the attached chip's emulated cycle clock, never wall time.
#[derive(Clone)]
pub struct AnalogStream(Arc<Mutex<State>>);

struct State {
    rate: u32,
    cpu_hz: u64,
    output: Output,
    now: u64,
    phase: u64,
    current: i16,
    samples: VecDeque<i16>,
}

enum Output {
    Voltage { bias: f32, amplitude: f32 },
    Raw,
}

impl AnalogStream {
    /// The first queued sample becomes current after one host sample period.
    /// Use the chip's fixed cycle frequency (e.g. `Soc::CPU_HZ`).
    pub fn new(
        rate: u32,
        cpu_hz: u64,
        bias: f32,
        amplitude: f32,
        now: u64,
    ) -> Result<Self, &'static str> {
        if !(8000..=96000).contains(&rate)
            || cpu_hz == 0
            || !bias.is_finite()
            || !amplitude.is_finite()
            || amplitude < 0.0
        {
            return Err("analog stream needs 8000..96000 Hz, a nonzero cycle clock and finite bias/nonnegative amplitude");
        }
        Ok(Self(Arc::new(Mutex::new(State {
            rate,
            cpu_hz,
            output: Output::Voltage { bias, amplitude },
            now,
            phase: 0,
            current: 0,
            samples: VecDeque::new(),
        }))))
    }

    /// A post-attenuation 12-bit stream. Empty input reads `bias`; underrun holds last.
    /// `push_raw` accepts counts; `push` maps PCM16 with round((PCM + 32768) * 4095 / 65535).
    pub fn new_raw(rate: u32, cpu_hz: u64, bias: u16, now: u64) -> Result<Self, &'static str> {
        if bias > 4095 {
            return Err("raw ADC bias must be in 0..=4095");
        }
        let stream = Self::new(rate, cpu_hz, 0.0, 0.0, now)?;
        {
            let mut state = stream.0.lock().unwrap();
            state.output = Output::Raw;
            state.current = bias as i16;
        }
        Ok(stream)
    }

    /// Advance to `now` before appending. Keep only the newest two seconds.
    /// Call between emulator runs; a timestamp older than the last access cannot rewind audio.
    pub fn push(&self, samples: &[i16], now: u64) {
        let mut state = self.0.lock().unwrap();
        let raw = matches!(state.output, Output::Raw);
        state.push(
            samples.iter().map(|&sample| {
                if raw {
                    (((i32::from(sample) + 32768) * 4095 + 32767) / 65535) as i16
                } else {
                    sample
                }
            }),
            now,
        );
    }

    /// Append exact 12-bit counts. Invalid counts or a voltage stream leave all state intact.
    pub fn push_raw(&self, samples: &[u16], now: u64) -> Result<(), &'static str> {
        let mut state = self.0.lock().unwrap();
        if !matches!(state.output, Output::Raw) || samples.iter().any(|&v| v > 4095) {
            return Err("push_raw needs a raw stream and counts in 0..=4095");
        }
        state.push(samples.iter().map(|&sample| sample as i16), now);
        Ok(())
    }

    /// Number of pending samples at `now`, including when firmware stops converting.
    pub fn queued_samples(&self, now: u64) -> usize {
        let mut state = self.0.lock().unwrap();
        state.advance(now);
        state.samples.len()
    }

    pub(super) fn volts(&self, now: u64) -> f32 {
        let mut state = self.0.lock().unwrap();
        state.advance(now);
        state.volts()
    }
    pub(super) fn convert(&self, now: u64, code: impl FnOnce(f32) -> u32) -> u16 {
        let mut state = self.0.lock().unwrap();
        state.advance(now);
        match state.output {
            Output::Raw => state.current as u16,
            Output::Voltage { .. } => code(state.volts()) as u16,
        }
    }
}

impl State {
    fn volts(&self) -> f32 {
        match self.output {
            Output::Voltage { bias, amplitude } => {
                bias + f32::from(self.current) / 32768.0 * amplitude
            }
            Output::Raw => 0.0,
        }
    }

    fn push(&mut self, samples: impl ExactSizeIterator<Item = i16>, now: u64) {
        self.advance(now);
        let capacity = self.rate as usize * 2;
        let skip = samples.len().saturating_sub(capacity);
        let discard = (self.samples.len() + samples.len().min(capacity)).saturating_sub(capacity);
        self.samples.drain(..discard);
        self.samples.extend(samples.skip(skip));
    }

    fn advance(&mut self, now: u64) {
        let phase = u128::from(self.phase)
            + u128::from(now.saturating_sub(self.now)) * u128::from(self.rate);
        let due = (phase / u128::from(self.cpu_hz)).min(self.samples.len() as u128) as usize;
        if due > 0 {
            self.current = self.samples[due - 1];
            self.samples.drain(..due);
        }
        self.phase = (phase % u128::from(self.cpu_hz)) as u64;
        self.now = self.now.max(now);
        // ponytail: zero-order hold; add an anti-alias filter for audio-fidelity work.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AnalogInputs, AnalogSource};

    #[test]
    fn stream_clock_bound_hold_and_shared_handle() {
        assert!(AnalogStream::new(0, 10, 1.0, 1.0, 0).is_err());
        assert!(AnalogStream::new(8000, 0, 1.0, 1.0, 0).is_err());
        assert!(AnalogStream::new(8000, 10, f32::NAN, 1.0, 0).is_err());
        assert!(AnalogStream::new(8000, 10, 1.0, -1.0, 0).is_err());
        let stream = AnalogStream::new(8000, 80_000_000, 1.0, 0.5, 0).unwrap();
        let mut pads = AnalogInputs::new(80_000_000);
        pads.set(1, AnalogSource::Stream(stream.clone()));
        assert_eq!(pads.volts(1, 0), 1.0);
        stream.push(&[16384, -16384, 8192], 0);
        assert_eq!(pads.volts(1, 9999), 1.0);
        assert_eq!(pads.volts(1, 10000), 1.25);
        assert_eq!(pads.volts(1, 19999), 1.25);
        assert_eq!(pads.volts(1, 20000), 0.75);
        assert_eq!(pads.volts(1, 90000), 1.125);
        assert_eq!(stream.queued_samples(90000), 0);
        stream.push(&[-32768], 95000);
        assert_eq!(pads.volts(1, 99999), 1.125);
        assert_eq!(pads.volts(1, 100000), 0.5);
        stream.push(&vec![1; 16000], 100000);
        stream.push(&[2], 100000);
        assert_eq!(stream.queued_samples(100000), 16000);
        assert_eq!(pads.volts(1, 160100000), 1.0 + 0.5 * 2.0 / 32768.0);
        stream.push(&vec![3; 24000], 160100000);
        assert_eq!(stream.queued_samples(160100000), 16000);
        assert_eq!(stream.queued_samples(u64::MAX), 0);
        assert_eq!(pads.volts(1, u64::MAX), 1.0 + 0.5 * 3.0 / 32768.0);
    }

    #[test]
    fn raw_stream_counts_pcm_mapping_and_validation() {
        assert!(AnalogStream::new_raw(8000, 8000, 4096, 0).is_err());
        let stream = AnalogStream::new_raw(8000, 8000, 777, 0).unwrap();
        let read = |now| stream.convert(now, |_| panic!("raw stream invoked calibration"));
        assert_eq!(read(0), 777);
        stream.push_raw(&[0, 1, 1024, 2048, 4095], 0).unwrap();
        assert!(stream.push_raw(&[5, 4096], 100).is_err());
        assert_eq!(stream.queued_samples(0), 5);
        for (now, raw) in [0, 1, 1024, 2048, 4095].into_iter().enumerate() {
            assert_eq!(read(now as u64 + 1), raw);
        }
        assert_eq!(read(100), 4095);
        stream.push(&[i16::MIN, -16384, -1, 0, 16384, i16::MAX], 100);
        for (i, expected) in [0, 1024, 2047, 2048, 3071, 4095].into_iter().enumerate() {
            assert_eq!(read(101 + i as u64), expected);
        }
        stream.push_raw(&vec![23; 16000], 106).unwrap();
        stream.push_raw(&[42], 106).unwrap();
        assert_eq!(stream.queued_samples(106), 16000);
        assert_eq!(read(16106), 42);
        let voltage = AnalogStream::new(8000, 8000, 1.0, 0.5, 0).unwrap();
        assert!(voltage.push_raw(&[1], 100).is_err());
        assert_eq!(voltage.volts(0), 1.0);
    }

    #[test]
    fn stream_fractional_clock_is_read_independent() {
        let make = || {
            let stream = AnalogStream::new(44100, 160_000_000, 0.0, 1.0, 0).unwrap();
            stream.push(&(0..2000).collect::<Vec<_>>(), 0);
            stream
        };
        let a = make();
        let b = make();
        for now in (0..5_000_000).step_by(7919) {
            a.volts(now);
        }
        assert_eq!(a.volts(5_000_000), b.volts(5_000_000));
        assert_eq!(a.queued_samples(5_000_000), b.queued_samples(5_000_000));
    }
}
