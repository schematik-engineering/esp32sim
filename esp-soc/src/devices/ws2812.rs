// WS2812-class decoder policy, not hardware calibration.
pub const MIN_PULSE_NS: u64 = 150;
pub const MAX_HIGH_NS: u64 = 1100;
pub const ONE_HIGH_NS: u64 = 550;
pub const RESET_US: u64 = 50;

/// A chain of WS2812-class LEDs fed by a decoded RMT bit stream: 24 bits per LED, G then R then
/// B, MSB first, in chain order. `leds` holds the colours in *physical* order when the chain was
/// made with a map (a ring wired out of sequence, a grid wired serpentine), so everything above
/// the board — the page, the PNG, the report, the tests — sees the module as the eye does.
pub struct Ws2812Chain {
    pub leds: Vec<[u8; 3]>,
    pub updates: u64,
    physical: Option<&'static [usize]>,
}

impl Ws2812Chain {
    /// `n` LEDs in chain order.
    pub fn new(n: usize) -> Self { Ws2812Chain { leds: vec![[0; 3]; n], updates: 0, physical: None } }

    /// A chain whose LED `i` sits at physical position `map[i]`. `map` must be a permutation.
    pub fn mapped(map: &'static [usize]) -> Self {
        let mut seen = vec![false; map.len()];
        for &p in map { assert!(p < map.len() && !seen[p], "WS2812 physical map is not a permutation: {:?}", map); seen[p] = true; }
        Ws2812Chain { leds: vec![[0; 3]; map.len()], updates: 0, physical: Some(map) }
    }

    /// Decode one transmission. A frame shorter than one LED (a lone reset pulse, a truncated
    /// stream) changes nothing and does not count as an update.
    pub fn from_bits(&mut self, bits: &[bool]) {
        let n = bits.len() / 24;
        if n == 0 { return; }
        for i in 0..n.min(self.leds.len()) {
            let mut v = 0u32;
            for b in 0..24 { v = (v << 1) | bits[i * 24 + b] as u32; }
            let at = match self.physical { Some(m) => m[i], None => i };
            self.leds[at] = [((v >> 8) & 0xff) as u8, ((v >> 16) & 0xff) as u8, (v & 0xff) as u8];   // GRB -> RGB
        }
        self.updates += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn bits(bytes: &[u8]) -> Vec<bool> { bytes.iter().flat_map(|&v| (0..8).map(move |i| v & (0x80 >> i) != 0)).collect() }

    #[test]
    fn grb_on_the_wire_becomes_rgb_in_chain_order() {
        let mut c = Ws2812Chain::new(2);
        c.from_bits(&bits(&[0x10, 0xab, 0x03, 1, 2, 3]));
        assert_eq!((c.leds[0], c.leds[1], c.updates), ([0xab, 0x10, 0x03], [2, 1, 3], 1));
        c.from_bits(&bits(&[0, 0, 0])[..20]);
        assert_eq!(c.updates, 1, "a short frame is not an update");
        c.from_bits(&bits(&[9, 9, 9, 9, 9, 9, 9, 9, 9]));
        assert_eq!((c.leds.len(), c.updates), (2, 2), "extra LEDs on the wire fall off the end");
    }

    #[test]
    fn a_map_places_each_chain_led_physically() {
        static REVERSED: [usize; 3] = [2, 1, 0];
        let mut c = Ws2812Chain::mapped(&REVERSED);
        c.from_bits(&bits(&[0, 1, 0, 0, 2, 0, 0, 3, 0]));
        assert_eq!(c.leds, [[3, 0, 0], [2, 0, 0], [1, 0, 0]]);
    }

    #[test]
    #[should_panic(expected = "not a permutation")]
    fn a_map_must_be_a_permutation() { static BAD: [usize; 3] = [0, 0, 2]; Ws2812Chain::mapped(&BAD); }
}

/// Recover WS2812-class lane pulses from parallel samples and deliver them through
/// the existing board frame callback. Timing windows are decoder policy, not calibration.
pub fn parallel_output(board: &mut dyn crate::BoardModel, pins: impl IntoIterator<Item = (u8, u8)>, samples: &[u16], clock_hz: u32) {
        for (pin, lane) in pins {
            if lane >= 16 || clock_hz == 0 { continue; }
            let mut bits=Vec::new();
            let mut at=0;
            while at<samples.len() {
                while at<samples.len() && samples[at] & (1<<lane)==0 { at+=1; }
                let start=at;
                while at<samples.len() && samples[at] & (1<<lane)!=0 { at+=1; }
                let high=at-start;
                let start=at;
                while at<samples.len() && samples[at] & (1<<lane)==0 { at+=1; }
                let low=at-start;
                if high==0 { break; }
                let high_ns=high as u64*1_000_000_000/u64::from(clock_hz);
                if !(MIN_PULSE_NS..=MAX_HIGH_NS).contains(&high_ns) { bits.clear(); continue; }
                bits.push(high_ns>=ONE_HIGH_NS);
                if low as u64*1_000_000>=RESET_US*u64::from(clock_hz) {
                    board.rmt_frame(pin, &bits); bits.clear();
                }
            }
            if !bits.is_empty() { board.rmt_frame(pin, &bits); }
        }
    }

#[cfg(test)]
mod parallel_tests {
    use super::*;
    use crate::BoardModel;
    #[derive(Default)]
    struct Frames(Vec<(u8, Vec<bool>)>);
    impl BoardModel for Frames {
        fn name(&self) -> &'static str { "parallel-test" }
        fn rmt_frame(&mut self, pin: u8, bits: &[bool]) { self.0.push((pin, bits.to_vec())); }
    }
    #[test]
    fn parallel_lanes_mirrors_and_reset_boundaries_reach_existing_boards() {
        let mut samples = Vec::new();
        for _ in 0..24 { samples.extend([3, 2, 0, 0]); }
        samples.extend(std::iter::repeat_n(0, 200));
        for _ in 0..24 { samples.extend([3, 1, 0, 0]); }
        let mut board = Frames::default();
        parallel_output(&mut board, [(4, 0), (5, 1), (6, 0)], &samples, 3_200_000);
        assert_eq!(board.0, vec![(4, vec![false; 24]), (4, vec![true; 24]),
            (5, vec![true; 24]), (5, vec![false; 24]), (6, vec![false; 24]), (6, vec![true; 24])]);
        parallel_output(&mut board, [(4, 0)], &samples, 0);
        parallel_output(&mut board, [(4, 16)], &samples, 3_200_000);
        assert_eq!(board.0.len(), 6);
    }
}
