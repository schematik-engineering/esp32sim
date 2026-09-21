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

    /// Recover pulse widths from a clocked physical lane. Low reset periods delimit frames.
    pub fn from_samples(&mut self, samples:&[u16], lane:u8, clock_hz:u32) {
        if lane >= 16 || clock_hz == 0 { return; }
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
            if !(150..=1100).contains(&high_ns) { bits.clear(); continue; }
            bits.push(high_ns>=550);
            if low as u64*1_000_000>=50*u64::from(clock_hz) {
                self.from_bits(&bits); bits.clear();
            }
        }
        if !bits.is_empty() { self.from_bits(&bits); }
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
    fn physical_parallel_lanes_preserve_identity_and_reset_boundaries() {
        let a=bits(&[0x10,0xab,0x03]); let b=bits(&[0x98,0x76,0x54]);
        let mut samples=Vec::new();
        for i in 0..24 { samples.extend([3, u16::from(a[i]) | (u16::from(b[i])<<1),0,0]); }
        samples.extend(std::iter::repeat_n(0,200));
        let mut first=Ws2812Chain::new(1);let mut second=Ws2812Chain::new(1);
        first.from_samples(&samples,0,3_200_000);second.from_samples(&samples,1,3_200_000);
        assert_eq!(first.leds,vec![[0xab,0x10,0x03]]);
        assert_eq!(second.leds,vec![[0x76,0x98,0x54]]);
        assert_eq!((first.updates,second.updates),(1,1));
        first.from_samples(&samples,0,0); assert_eq!(first.updates,1);
    }

    #[test]
    #[should_panic(expected = "not a permutation")]
    fn a_map_must_be_a_permutation() { static BAD: [usize; 3] = [0, 0, 2]; Ws2812Chain::mapped(&BAD); }
}
