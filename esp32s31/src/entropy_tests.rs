//! M1g tests: independent sequence oracle, access-time fold and fail-closed controls.
use crate::{AccessFault, Probe, S31Bus, StopReason, HP_SRAM};
use emu_core::{Bus, Fault};

#[test]
#[ignore = "set S31_ROM and S31_ROM_DATA to the pinned ROM inputs"]
fn external_stack_guard_uses_two_entropy_loads_without_bypass() {
    let mut p = Probe::new();
    p.bus
        .load_rom(&std::fs::read(std::env::var_os("S31_ROM").expect("set S31_ROM")).unwrap())
        .unwrap();
    p.bus
        .load_rom_data(
            &std::fs::read(std::env::var_os("S31_ROM_DATA").expect("set S31_ROM_DATA")).unwrap(),
        )
        .unwrap();
    let mut state = 0x2545_f491;
    let mut samples = Vec::new();
    for _ in 0..10_000 {
        let destination = match p.cpu.pc {
            0x2f80d052 => Some(9),
            0x2f80d080 => Some(19),
            _ => None,
        };
        let expected = destination.map(|_| reference(&mut state, p.cpu.retired_count as u32));
        let s = p.run(1);
        if let (Some(rd), Some(value)) = (destination, expected) {
            assert_eq!(s.reason, StopReason::Budget);
            assert_eq!(p.cpu.x[rd], value);
            samples.push(value);
        }
        if s.reason != StopReason::Budget {
            assert_eq!(s.pc, 0x2f812022);
            assert_eq!(s.retired, 7463);
            assert_eq!(samples.len(), 2);
            assert_eq!(p.bus.read32(0x2f07fa38), Ok(samples[0] ^ samples[1]));
            assert_eq!(p.bus.read32(0x20814000).unwrap() & (3 << 30), 0);
            return;
        }
    }
    panic!("ROM did not reach the explicit PMU stop");
}

fn reference(state: &mut u32, now: u32) -> u32 {
    // Independent wide arithmetic formulation of xorshift(13,17,5).
    let mut wide = u64::from(*state);
    for (left, shift) in [(true, 13), (false, 17), (true, 5)] {
        wide = (wide ^ if left { wide << shift } else { wide >> shift }) & 0xffff_ffff;
    }
    *state = wide as u32;
    ((wide + u64::from(now)) & 0xffff_ffff) as u32
}
fn running(b: &mut S31Bus) {
    b.write32(0x208140fc, (1 << 28) | 2426672).unwrap();
    b.write32(0x2071003c, 1 << 30).unwrap();
    b.write32(0x20814000, (1 << 31) | (1 << 30)).unwrap();
}
#[test]
fn entropy_sequence_and_now_fold() {
    let mut b = S31Bus::new();
    running(&mut b);
    let mut state = 0x2545_f491;
    for now in [0, 1, 7049, u32::MAX, 0x8000_0000, 17, 17, 0] {
        b.boot_regs.entropy.now = now;
        assert_eq!(b.read32(0x20814044), Ok(reference(&mut state, now)));
    }
    let mut fresh = S31Bus::new();
    running(&mut fresh);
    let mut seed = 0x2545_f491;
    assert_eq!(fresh.read32(0x20814044), Ok(reference(&mut seed, 0)));
}
#[test]
fn entropy_gates_do_not_consume_and_invalid_accesses_stop() {
    for (address, value) in [
        (0x208140fc, 2426672),     // register clock off
        (0x2071003c, 0),           // RNG clock off
        (0x2071003c, 0xc000_0000), // held reset
        (0x20814000, 1 << 30),     // sampling off
    ] {
        let mut b = S31Bus::new();
        running(&mut b);
        b.write32(address, value).unwrap();
        assert_eq!(b.read32(0x20814044), Err(Fault::Prohibited));
        assert_eq!(
            b.fault,
            Some(AccessFault {
                address: 0x20814044,
                width: 4,
                write: false,
                fault: Fault::Prohibited,
            })
        );
        b.fault = None;
        running(&mut b);
        let mut state = 0x2545_f491;
        assert_eq!(b.read32(0x20814044), Ok(reference(&mut state, 0)));
    }
    let mut b = S31Bus::new();
    running(&mut b);
    assert_eq!(b.read8(0x20814044), Err(Fault::Unmapped));
    assert_eq!(b.read16(0x20814044), Err(Fault::Unmapped));
    assert_eq!(b.fetch(0x20814044), Err(Fault::Unmapped));
    assert_eq!(b.write32(0x20814044, 1), Err(Fault::Unmapped));
    for a in [0x20814043, 0x20814045, 0x2081403c, 0x20814040, 0x20814048] {
        assert_eq!(b.read32(a), Err(Fault::Unmapped));
    }
    // Invalid widths/directions/neighbors must not advance the generator either.
    let mut state = 0x2545_f491;
    assert_eq!(b.read32(0x20814044), Ok(reference(&mut state, 0)));
}
#[test]
fn entropy_probe_refreshes_low_retired_before_each_read() {
    let mut p = Probe::new();
    running(&mut p.bus);
    p.cpu.pc = HP_SRAM.0;
    p.cpu.x[1] = 0x20814044;
    // lw x2,0(x1); lw x3,0(x1). Two actual bus accesses through Cpu::step.
    p.bus.write32(HP_SRAM.0, 0x0000_a103).unwrap();
    p.bus.write32(HP_SRAM.0 + 4, 0x0000_a183).unwrap();
    p.cpu.retired_count = 0x1_ffff_ffff;
    p.cpu.cycle_count = 123;
    p.bus.boot_regs.entropy.now = 42;
    let mut state = 0x2545_f491;
    assert_eq!(p.run(1).reason, StopReason::Budget);
    assert_eq!(p.cpu.x[2], reference(&mut state, u32::MAX));
    assert_eq!(p.run(1).reason, StopReason::Budget);
    assert_eq!(p.cpu.x[3], reference(&mut state, 0));
    assert_eq!(p.cpu.retired_count, 0x2_0000_0001);
}
#[test]
fn swd_flag_clear_only_and_typed_stop() {
    let mut b = S31Bus::new();
    assert_eq!(b.read32(0x2080101c), Ok(300 << 20));
    b.write32(0x2080101c, (300 << 20) | (1 << 19)).unwrap();
    assert_eq!(b.read32(0x2080101c), Ok(300 << 20));
    // Synthetic asserted flag to make the clear rule observable without a reset.
    b.boot_regs.set_swd_flag_for_test();
    assert_eq!(b.read32(0x2080101c), Ok((300 << 20) | 1));
    b.write32(0x2080101c, (300 << 20) | 1 | (1 << 19)).unwrap();
    assert_eq!(b.read32(0x2080101c), Ok(300 << 20));
    for value in [
        0,
        300 << 20,
        0xffff_ffff,
        (300 << 20) | (1 << 31),
        (301 << 20) | (1 << 19),
    ] {
        b.fault = None;
        assert_eq!(b.write32(0x2080101c, value), Err(Fault::Prohibited));
        assert_eq!(b.read32(0x2080101c), Ok(300 << 20));
        assert_eq!(
            b.fault.unwrap(),
            AccessFault {
                address: 0x2080101c,
                width: 4,
                write: true,
                fault: Fault::Prohibited,
            }
        );
    }
}
#[test]
fn cache_shutter_mapping_retention_and_unsupported_fields() {
    let mut b = S31Bus::new();
    for address in [0x2c000000, 0x2c000004] {
        assert_eq!(b.read32(address), Ok(0));
        for value in [1, 2, 3, 0] {
            b.write32(address, value).unwrap();
            assert_eq!(b.read32(address), Ok(value));
        }
        for bit in 2..32 {
            assert_eq!(b.write32(address, 1 << bit), Err(Fault::Prohibited));
            assert_eq!(b.read32(address), Ok(0));
        }
    }
    b.write32(0x2c000000, 1).unwrap();
    b.write32(0x2c000004, 2).unwrap();
    assert_eq!(b.read32(0x2c000000), Ok(1));
    assert_eq!(b.read32(0x2c000004), Ok(2));
    // Shutters do not enable a flash/cache backing or block uncached SRAM/ROM.
    b.write32(HP_SRAM.0, 0x1234).unwrap();
    assert_eq!(b.read32(HP_SRAM.0), Ok(0x1234));
    assert_eq!(b.read32(0x40000000), Err(Fault::Unmapped));
}
#[test]
fn new_controls_fail_closed_on_widths_and_neighbors() {
    for address in [0x2080101c, 0x2c000000, 0x2c000004] {
        let mut b = S31Bus::new();
        assert_eq!(b.read8(address), Err(Fault::Unmapped));
        assert_eq!(b.read16(address), Err(Fault::Unmapped));
        assert_eq!(b.write8(address, 0), Err(Fault::Unmapped));
        assert_eq!(b.write16(address, 0), Err(Fault::Unmapped));
        assert_eq!(b.fetch(address), Err(Fault::Unmapped));
        assert_eq!(b.read32(address + 1), Err(Fault::Unmapped));
    }
    for address in [0x20801018, 0x20801020, 0x2c000008, 0x20704110] {
        let mut b = S31Bus::new();
        assert_eq!(b.read32(address), Err(Fault::Unmapped));
        assert_eq!(b.write32(address, 0), Err(Fault::Unmapped));
    }
}
