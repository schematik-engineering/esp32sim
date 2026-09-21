use crate::gdma::GdmaOutCh;

/// Fetch one SPI MOSI transaction from writable SRAM. Descriptor changes are committed only
/// after the entire requested payload has validated, so a broken chain cannot deliver a prefix.
pub fn transmit(
    sram: &mut [u8],
    base: u32,
    channel: &mut GdmaOutCh,
    bits: u32,
) -> Result<Vec<u8>, &'static str> {
    let result = gather(sram, base, channel, bits);
    channel.running = false;
    match result {
        Ok((bytes, descriptors, last)) => {
            if channel.conf0 & (1 << 2) != 0 {
                for offset in descriptors {
                    let flags = u32::from_le_bytes(sram[offset..offset + 4].try_into().unwrap());
                    sram[offset..offset + 4].copy_from_slice(&(flags & !(1 << 31)).to_le_bytes());
                }
            }
            channel.desc = 0;
            channel.eof_desc = last;
            channel.int_raw |= 1 | 2 | 8;
            Ok(bytes)
        }
        Err(error) => {
            channel.int_raw |= 1 << 2;
            Err(error)
        }
    }
}
fn gather(
    sram: &[u8],
    base: u32,
    channel: &GdmaOutCh,
    bits: u32,
) -> Result<(Vec<u8>, Vec<usize>, u32), &'static str> {
    if bits == 0 || bits > 262144 {
        return Err("invalid SPI length");
    }
    let want = (bits as usize).div_ceil(8);
    let mut data = Vec::with_capacity(want);
    let mut offsets = Vec::new();
    let mut visited = std::collections::HashSet::new();
    let mut addr = channel.desc;
    let offset = |addr: u32, n: usize| {
        addr.checked_sub(base)
            .map(|o| o as usize)
            .filter(|&o| o <= sram.len() && n <= sram.len() - o)
            .ok_or("DMA address outside SRAM")
    };
    loop {
        if addr & 3 != 0 || offsets.len() >= 1024 || !visited.insert(addr) {
            return Err("invalid DMA descriptor chain");
        }
        let o = offset(addr, 12)?;
        let word = |i| u32::from_le_bytes(sram[o + i..o + i + 4].try_into().unwrap());
        let flags = word(0);
        let length = ((flags >> 12) & 0xfff) as usize;
        let size = (flags & 0xfff) as usize;
        if flags & (1 << 31) == 0 || length == 0 || length > size {
            return Err("invalid DMA descriptor ownership or length");
        }
        let n = length.min(want - data.len());
        let buffer = offset(word(4), n)?;
        data.extend_from_slice(&sram[buffer..buffer + n]);
        offsets.push(o);
        if data.len() == want {
            return Ok((data, offsets, addr));
        }
        if flags & (1 << 30) != 0 || word(8) == 0 {
            return Err("short DMA payload");
        }
        addr = word(8);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Vec<u8>, GdmaOutCh) {
        let mut ram = vec![0; 256];
        for (at, flags, buf, next) in [
            (16, 1 << 31 | 4 << 12 | 4, 128, 32),
            (32, 1 << 31 | 1 << 30 | 4 << 12 | 4, 132, 0),
        ] {
            for (i, v) in [flags, buf, next].into_iter().enumerate() {
                ram[at + i * 4..at + i * 4 + 4].copy_from_slice(&(v as u32).to_le_bytes());
            }
        }
        ram[128..136].copy_from_slice(b"SPI data");
        (
            ram,
            GdmaOutCh {
                desc: 16,
                running: true,
                conf0: 1 << 2,
                ..Default::default()
            },
        )
    }
    #[test]
    fn dma_delivers_exact_payload_and_releases_descriptors() {
        let (mut ram, mut ch) = fixture();
        assert_eq!(transmit(&mut ram, 0, &mut ch, 64).unwrap(), b"SPI data");
        assert!(!ch.running);
        assert_eq!(ch.int_raw, 11);
        assert_eq!(ch.eof_desc, 32);
        assert_eq!(ram[19] & 0x80, 0);
        assert_eq!(ram[35] & 0x80, 0);
    }
    #[test]
    fn faults_do_not_release_prefix_or_report_success() {
        for (offset, value, bits) in [
            (32, 4 << 12 | 4, 64),
            (36, 255, 64),
            (40, 16, 96),
            (32, 1 << 31 | 1 << 30 | 4 << 12 | 4, 96),
            (32, 1 << 31 | 8 << 12 | 4, 64),
        ] {
            let (mut ram, mut ch) = fixture();
            ram[offset..offset + 4].copy_from_slice(&(value as u32).to_le_bytes());
            // The cycle case must continue past the second descriptor.
            if offset == 40 {
                ram[35] &= !0x40;
            }
            assert!(transmit(&mut ram, 0, &mut ch, bits).is_err());
            assert_eq!(ch.int_raw, 4);
            assert_eq!(ram[19] & 0x80, 0x80);
            assert!(!ch.running);
        }
    }
}
