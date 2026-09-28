//! Bounded host PCM16LE, clocked by peripheral time rather than firmware read count.
use std::collections::VecDeque;

#[derive(Clone)]
pub struct PcmInput {
    pub sample_rate: u32,
    pub channels: u32,
    pub pins: [u32; 3],
    frames: VecDeque<[i16; 2]>,
    phase: u64,
    clock_hz: u64,
    current: [i16; 2],
}
impl PcmInput {
    pub fn new(sample_rate: u32, channels: u32, pins: [u32; 3]) -> Option<Self> {
        if !(8000..=96000).contains(&sample_rate) || !(1..=2).contains(&channels) { return None; }
        Some(Self { sample_rate, channels, pins, frames: VecDeque::new(), phase: 0, clock_hz: 80_000_000, current: [0; 2] })
    }
    pub fn push(&mut self, bytes: &[u8]) -> bool {
        let frame_bytes = self.channels as usize * 2;
        if bytes.len() % frame_bytes != 0 || bytes.len() > 768000 { return false; }
        let capacity = self.sample_rate as usize * 2;
        for frame in bytes.chunks_exact(frame_bytes) {
            let left = i16::from_le_bytes([frame[0], frame[1]]);
            let right = if self.channels == 2 { i16::from_le_bytes([frame[2], frame[3]]) } else { left };
            if self.frames.len() == capacity { self.frames.pop_front(); }
            self.frames.push_back([left, right]);
        }
        true
    }
    pub fn reset(&mut self) { self.frames.clear(); self.current = [0; 2]; self.phase = 0; }
    pub fn advance(&mut self, ticks: u64, clock_hz: u64) -> [i16; 2] {
        if self.clock_hz != clock_hz { self.phase = self.phase * clock_hz / self.clock_hz; self.clock_hz = clock_hz; }
        self.phase += ticks * u64::from(self.sample_rate);
        let due = self.phase / clock_hz;
        self.phase %= clock_hz;
        let available = self.frames.len() as u64;
        for _ in 0..due.min(available) { self.current = self.frames.pop_front().unwrap(); }
        if due > available { self.current = [0; 2]; }
        self.current
    }
    pub fn adc_code(&mut self, ticks: u64) -> u16 {
        let sample = self.advance(ticks, 80_000_000);
        let mono = (i32::from(sample[0]) + i32::from(sample[1])) / 2;
        (((mono + 32768) as u32 * 4095 + 32767) / 65535) as u16
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pcm_bounds_clock_channels_and_reset() {
        let mut pcm = PcmInput::new(8000, 1, [1, 0, 0]).unwrap();
        assert!(!pcm.push(&[0]));
        assert!(pcm.push(&[0, 128, 0, 0, 255, 127, 0, 0]));
        assert_eq!(pcm.adc_code(10000), 0);
        assert_eq!(pcm.adc_code(5000), 0);
        assert_eq!(pcm.adc_code(5000), 2048);
        assert_eq!(pcm.adc_code(10000), 4095);
        pcm.reset(); assert_eq!(pcm.adc_code(10000), 2048);
        pcm.push(&vec![0; 80000]); assert_eq!(pcm.frames.len(), 16000);
        assert!(PcmInput::new(0, 1, [0; 3]).is_none());
    }
}
