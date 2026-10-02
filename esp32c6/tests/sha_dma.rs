mod sha_dma_tests {
    use esp32c6::bus::{SocBus, SRAM_LOW};
    use emu_core::Bus;

    fn fixture() -> SocBus {
        let mut bus = SocBus::new(4 << 20, [0; 6]);
        let input = SRAM_LOW + 0x100;
        let mut block = [0u8; 64];
        block[..4].copy_from_slice(&[b'a', b'b', b'c', 0x80]);
        block[63] = 24;
        for (i, byte) in block.into_iter().enumerate() { bus.write8(input + i as u32, byte).unwrap(); }
        for (desc, len, buf, next, eof) in [(SRAM_LOW, 16, input, SRAM_LOW + 16, 0), (SRAM_LOW + 16, 48, input + 16, 0, 1)] {
            bus.write32(desc, (1 << 31) | (eof << 30) | (len << 12) | len).unwrap();
            bus.write32(desc + 4, buf).unwrap();
            bus.write32(desc + 8, next).unwrap();
        }
        bus.write32(0x60080100, 7).unwrap();
        bus.write32(0x600800e0, (1 << 21) | (SRAM_LOW & 0xfffff)).unwrap();
        bus.write32(0x60089000, 2).unwrap();
        bus.write32(0x6008900c, 1).unwrap();
        bus.write32(0x6008901c, 1).unwrap();
        bus
    }

    #[test]
    fn hashes_sha256_across_dma_descriptors_and_returns_ownership() {
        let mut bus = fixture();
        bus.tick(1);
        let digest: Vec<_> = (0..8).flat_map(|n| bus.read32(0x60089040 + n * 4).unwrap().to_le_bytes()).collect();
        assert_eq!(digest.iter().map(|b| format!("{b:02x}")).collect::<String>(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(bus.read32(0x60089018).unwrap(), 0);
        assert_eq!(bus.read32(SRAM_LOW).unwrap() >> 31, 0);
        assert_eq!(bus.read32(SRAM_LOW + 16).unwrap() >> 31, 0);
        assert_eq!(bus.periph.gdma.gdma.out[0].eof_desc, SRAM_LOW + 16);
        assert_eq!(bus.periph.gdma.gdma.out[0].int_raw & 3, 3);
    }

    #[test]
    fn dma_continue_preserves_the_previous_hash_state() {
        let mut bus = fixture();
        let input = SRAM_LOW + 0x100;
        let mut padded = [0u8; 128];
        padded[..80].fill(b'a'); padded[80] = 0x80;
        padded[120..].copy_from_slice(&640u64.to_be_bytes());
        for (n, block) in padded.as_chunks::<64>().0.iter().enumerate() {
            for (i, byte) in block.iter().enumerate() { bus.write8(input + i as u32, *byte).unwrap(); }
            bus.write32(SRAM_LOW, (3 << 30) | (64 << 12) | 64).unwrap();
            bus.write32(SRAM_LOW + 8, 0).unwrap();
            bus.write32(0x600800e0, (1 << 21) | (SRAM_LOW & 0xfffff)).unwrap();
            bus.write32(if n == 0 { 0x6008901c } else { 0x60089020 }, 1).unwrap();
            bus.tick(1);
            assert!(!bus.periph.sha.busy);
        }
        let digest: Vec<_> = (0..8).flat_map(|n| bus.read32(0x60089040 + n * 4).unwrap().to_le_bytes()).collect();
        assert_eq!(digest.iter().map(|b| format!("{b:02x}")).collect::<String>(), "0f45e858fbc4176cdf4e411f88281edefc390ae5afe7df0f44cd9297f0a64580");
        assert_eq!(bus.periph.sha.blocks, 2);
    }

    #[test]
    fn cyclic_or_unowned_dma_does_not_hash_fabricated_bytes() {
        for unowned in [false, true] {
            let mut bus = fixture();
            if unowned { let word = bus.read32(SRAM_LOW + 16).unwrap(); bus.write32(SRAM_LOW + 16, word & !(1 << 31)).unwrap(); }
            else { bus.write32(SRAM_LOW + 8, SRAM_LOW).unwrap(); }
            bus.tick(1);
            assert_eq!(bus.periph.sha.blocks, 0);
            assert_eq!(bus.read32(0x60089018).unwrap(), 1);
            assert_eq!(bus.read32(SRAM_LOW).unwrap() >> 31, 1);
            assert_eq!(bus.periph.gdma.gdma.out[0].int_raw & 3, 0);
        }
    }
}
