use esp_periph::spi_mem::SpiMem;

fn transfer(s: &mut SpiMem, ram: &mut [u8], command: u16, address: u32, write: bool, len: u32) {
    s.write(0x34, 1);
    s.write(0x18, 1 << 31 | 1 << 30 | if write { 1 << 27 } else { 1 << 28 });
    s.write(0x1c, 23 << 26);
    s.write(0x20, command as u32 | 7 << 28);
    s.write(0x4, address);
    s.write(if write { 0x24 } else { 0x28 }, len * 8 - 1);
    s.write(0, 1 << 18);
    s.execute(&mut [0xff; 256], ram);
}

#[test]
fn quad_capacity_id_and_absent_device() {
    let mut s = SpiMem::new(true);
    for (size, eid) in [(0, 0xff), (2 << 20, 2), (4 << 20, 0x22), (8 << 20, 0x42)] {
        let mut ram = vec![0; size];
        transfer(&mut s, &mut ram, 0x9f, 0, false, 6);
        if size == 0 { assert_eq!(s.w[0], u32::MAX); }
        else { assert_eq!(s.w[0], eid << 16 | 0x5d0d); }
    }
    let mut absent = [];
    transfer(&mut s, &mut absent, 0x4040, 0, false, 4);
    assert_eq!(s.w[0], u32::MAX, "absent octal RAM must not identify itself");
}

#[test]
fn quad_probe_and_tuning_commands_write_real_ram_without_flash_semantics() {
    let mut s = SpiMem::new(true);
    let mut ram = vec![0; 2 << 20];
    for (write, read) in [(0x02, 0x03), (0x38, 0xeb), (0x02, 0x0b)] {
        s.w[0] = 0x5a33ff81;
        transfer(&mut s, &mut ram, write, 100, true, 4);
        assert_eq!(&ram[100..104], &[0x81, 0xff, 0x33, 0x5a]);
        s.w[0] = 0;
        transfer(&mut s, &mut ram, read, 100, false, 4);
        assert_eq!(s.w[0], 0x5a33ff81);
        s.w[0] = 0xffff_ffff;
        transfer(&mut s, &mut ram, write, 100, true, 4);
        assert_eq!(&ram[100..104], &[0xff; 4], "PSRAM overwrites zero bits without erase");
    }
    assert!(s.dirty.iter().all(|&(kind, _, _)| kind == esp_periph::spi_mem::DirtyMem::Psram));
}
