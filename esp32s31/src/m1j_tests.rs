use crate::{AccessFault, Probe, S31Bus, StopReason};
use emu_core::{Bus, Fault};

#[test]
fn pad_hold_uart_config_resets_retention_and_rejections() {
    let mut b = S31Bus::new();
    for (a, reset, mask) in [
        (0x20700270, 0, 0x000c0000),
        (0x2038a088, 0x03000000, 0x03000000),
        (0x2038a014, 694, 0x00f00fff),
    ] {
        assert_eq!(b.read32(a), Ok(reset));
        for v in [0, mask, reset] {
            b.write32(a, v).unwrap();
            assert_eq!(b.read32(a), Ok(v));
        }
        for bit in 0..32 {
            let v = 1 << bit;
            if v & mask != 0 {
                b.write32(a, v).unwrap();
                assert_eq!(b.read32(a), Ok(v));
                b.write32(a, reset).unwrap();
            } else {
                b.fault = None;
                assert_eq!(b.write32(a, v | mask), Err(Fault::Prohibited));
                assert_eq!(
                    b.fault,
                    Some(AccessFault {
                        address: a,
                        width: 4,
                        write: true,
                        fault: Fault::Prohibited
                    })
                );
                assert_eq!(b.read32(a), Ok(reset));
            }
        }
        for d in 0..4 {
            assert_eq!(b.read8(a + d), Err(Fault::Unmapped));
            assert_eq!(b.read16(a + d), Err(Fault::Unmapped));
            assert_eq!(b.write8(a + d, 0), Err(Fault::Unmapped));
            assert_eq!(b.write16(a + d, 0), Err(Fault::Unmapped));
            assert_eq!(b.fetch(a + d), Err(Fault::Unmapped));
            if d != 0 {
                assert_eq!(b.read32(a + d), Err(Fault::Unmapped));
                assert_eq!(b.write32(a + d, 0), Err(Fault::Unmapped));
            }
        }
        for neighbor in [a - 4, a + 4] {
            assert_eq!(b.read32(neighbor), Err(Fault::Unmapped));
            assert_eq!(b.write32(neighbor, 0), Err(Fault::Unmapped));
        }
    }
}

#[test]
#[ignore = "set S31_ROM and S31_ROM_DATA to pinned public ELF and M1c dump"]
fn external_native_rom_configures_gpio58_gpio59_and_uart_divider() {
    let mut p = Probe::new();
    p.bus
        .load_rom(&std::fs::read(std::env::var_os("S31_ROM").expect("set S31_ROM")).unwrap())
        .unwrap();
    p.bus
        .load_rom_data(
            &std::fs::read(std::env::var_os("S31_ROM_DATA").expect("set S31_ROM_DATA")).unwrap(),
        )
        .unwrap();
    let stop = p.run(1_000_000);
    assert_eq!(stop.pc, 0x2f83ee28);
    assert_eq!(stop.raw, 0x0000411c);
    assert_eq!(stop.retired, 7598);
    assert_eq!(
        stop.reason,
        StopReason::Access(AccessFault {
            address: 0x2038a098,
            width: 4,
            write: false,
            fault: Fault::Unmapped
        })
    );
    assert_eq!(p.bus.read32(0x205820e8), Ok(0x900));
    assert_eq!(p.bus.read32(0x205820ec), Ok(0xb00));
    assert_eq!(p.bus.read32(0x20700270), Ok(0));
    assert_eq!(p.bus.read32(0x2038a088), Ok(0x03000000));
    assert_eq!(p.bus.read32(0x2038a014), Ok(0x0030015b));
}
