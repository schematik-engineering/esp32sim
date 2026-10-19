use super::*;
use emu_core::FlatRam;
const BOOT: &str = include_str!("../tests/fixtures/zc/bootloader.tsv");
const APP: &str = include_str!("../tests/fixtures/zc/app.tsv");
fn rows(text: &str) -> impl Iterator<Item = (u32, u32, &str)> {
    text.lines().map(|line| {
        let p: Vec<_> = line.split('\t').collect();
        (
            u32::from_str_radix(p[0], 16).unwrap(),
            u32::from_str_radix(p[1], 16).unwrap(),
            p[2],
        )
    })
}
fn decode(raw: u16) -> ExtInsn {
    try_decode(0, u32::from(raw).to_le_bytes()).unwrap()
}
fn cpu() -> Cpu {
    let mut c = Cpu::new();
    c.pc = 0;
    for r in 1..32 {
        c.x[r] = 0x8765_0000 + r as u32 * 0x123;
    }
    c
}
fn counts(c: &Cpu) -> (u64, u64, u64) {
    (c.insn_count, c.cycle_count, c.retired_count)
}
fn finish(c: &Cpu) {
    assert_eq!(c.pc, 2);
    assert_eq!(counts(c), (1, 1, 1));
    assert_eq!(c.x[0], 0);
}
#[test]
fn every_pinned_linear_site_matches_objdump() {
    let mut names = std::collections::BTreeSet::new();
    for (text, count) in [(BOOT, 203), (APP, 1708)] {
        assert_eq!(rows(text).count(), count);
        for (pc, raw, name) in rows(text) {
            let i = try_decode(pc, raw.to_le_bytes()).unwrap();
            assert_eq!(i.op.mnemonic(), name, "{pc:08x}: {raw:04x}");
            assert_eq!((i.pc, i.length), (pc, 2));
            names.insert(name);
        }
    }
    assert_eq!(names.len(), 15);
    let (_, raw, _) = rows(BOOT).find(|r| r.0 == 0x2f06a2ba).unwrap();
    assert_eq!(raw, 0xb84e);
    let i = decode(raw as u16);
    assert_eq!((i.op, i.count, i.imm), (Op::Push, 1, 64));
}

fn unary(name: &str) {
    let raw = rows(APP).chain(rows(BOOT)).find(|r| r.2 == name).unwrap().1 as u16;
    for rd in 8..16 {
        for a in [
            0,
            0x7f,
            0x80,
            0xff,
            0x7fff,
            0x8000,
            0xffff,
            0x8000_8001,
            u32::MAX,
        ] {
            let i = decode((raw & !0x380) | ((rd - 8) << 7));
            let mut c = cpu();
            c.x[rd as usize] = a;
            let mut expected = c.x;
            let signed = |v: u32, bits: u32| {
                if v & (1 << (bits - 1)) != 0 {
                    v | (u32::MAX << bits)
                } else {
                    v
                }
            };
            expected[rd as usize] = match name {
                "c.zext.b" => a % 256,
                "c.sext.b" => signed(a % 256, 8),
                "c.zext.h" => a % 65536,
                "c.sext.h" => signed(a % 65536, 16),
                "c.not" => u32::MAX - a,
                _ => unreachable!(),
            };
            exec(&i, &mut c, &mut FlatRam::new(0, 0)).unwrap();
            assert_eq!(c.x, expected, "{name} rd={rd} a={a:x}");
            finish(&c);
        }
    }
}
macro_rules! unary_cases { ($($test:ident => $name:literal),*) => { $(#[test] fn $test(){unary($name);})* }; }
unary_cases! {zext_b=>"c.zext.b",sext_b=>"c.sext.b",zext_h=>"c.zext.h",sext_h=>"c.sext.h",not=>"c.not"}
#[test]
fn mul() {
    for rd in 8..16 {
        for rs in 8..16 {
            for a in [0, u32::MAX, 0x8000_0000, 0x1234_5678] {
                let mut c = cpu();
                c.x[rd] = a;
                let b = c.x[rs];
                let mut expected = c.x;
                expected[rd] = ((u64::from(a) * u64::from(b)) & 0xffff_ffff) as u32;
                exec(
                    &decode(0x9c41 | ((rd as u16 - 8) << 7) | ((rs as u16 - 8) << 2)),
                    &mut c,
                    &mut FlatRam::new(0, 0),
                )
                .unwrap();
                assert_eq!(c.x, expected);
                finish(&c);
            }
        }
    }
}
fn memory(name: &str) {
    let base = rows(APP).chain(rows(BOOT)).find(|r| r.2 == name).unwrap().1 as u16;
    for rd in 8..16 {
        for rs in 8..16 {
            for offset in 0..if matches!(name, "c.lbu" | "c.sb") {
                4
            } else {
                2
            } {
                let byte = matches!(name, "c.lbu" | "c.sb");
                let imm = if byte { offset } else { offset * 2 };
                let bits = if byte {
                    ((offset & 1) << 6) | ((offset >> 1) << 5)
                } else {
                    offset << 5
                };
                let raw = (base & !0x3bc & if byte { !0x40 } else { u16::MAX })
                    | ((rs - 8) << 7)
                    | ((rd - 8) << 2)
                    | bits;
                let i = decode(raw);
                assert_eq!(i.imm, u32::from(imm));
                let mut c = cpu();
                c.x[rs as usize] = 0x1000;
                let mut ram = FlatRam::new(0x1000, 8);
                ram.mem = vec![0x81, 0xff, 0x80, 0x7f, 0xaa, 0xaa, 0xaa, 0xaa];
                let mut expected = c.x;
                let mut mem = ram.mem.clone();
                let n = usize::from(imm);
                let b = c.x[rd as usize];
                match name {
                    "c.lbu" => expected[rd as usize] = u32::from(mem[n]),
                    "c.lhu" => {
                        expected[rd as usize] = u32::from(mem[n]) + 256 * u32::from(mem[n + 1])
                    }
                    "c.lh" => {
                        let v = u32::from(mem[n]) + 256 * u32::from(mem[n + 1]);
                        expected[rd as usize] = if v >= 32768 { v | 0xffff0000 } else { v };
                    }
                    "c.sb" => mem[n] = b as u8,
                    "c.sh" => mem[n..n + 2].copy_from_slice(&(b as u16).to_le_bytes()),
                    _ => unreachable!(),
                }
                exec(&i, &mut c, &mut ram).unwrap();
                assert_eq!(c.x, expected, "{name}");
                assert_eq!(ram.mem, mem);
                finish(&c);
            }
        }
    }
}
macro_rules! memory_cases { ($($test:ident => $name:literal),*) => { $(#[test] fn $test(){memory($name);})* }; }
memory_cases! {lbu=>"c.lbu",lhu=>"c.lhu",lh=>"c.lh",sb=>"c.sb",sh=>"c.sh"}

// Independent spec table, not the executor's REGS or stack calculation.
const LIST: [usize; 13] = [1, 8, 9, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27];
fn stack(push: bool) {
    for rlist in 4..16 {
        for spimm in 0..4 {
            let count = if rlist == 15 { 13 } else { rlist - 3 };
            let adj = match rlist {
                4..=7 => 16,
                8..=11 => 32,
                12..=14 => 48,
                _ => 64,
            } + 16 * spimm;
            let raw =
                if push { 0xb802 } else { 0xba02 } | ((rlist as u16) << 4) | ((spimm as u16) << 2);
            let i = decode(raw);
            assert_eq!(usize::from(i.count), count);
            assert_eq!(i.imm, adj);
            let mut c = cpu();
            c.x[2] = if push { 0x1100 } else { 0x1100 - adj };
            let mut ram = FlatRam::new(0x1000, 256);
            let mut expected = c.x;
            let mut mem = ram.mem.clone();
            for (index, &reg) in LIST[..count].iter().enumerate() {
                let addr = 256 - 4 * (count - index);
                if push {
                    mem[addr..addr + 4].copy_from_slice(&c.x[reg].to_le_bytes());
                } else {
                    let value = 0xabc00000 + reg as u32;
                    ram.write32(0x1000 + addr as u32, value).unwrap();
                    expected[reg] = value;
                }
            }
            if !push {
                mem = ram.mem.clone();
            }
            expected[2] = if push { 0x1100 - adj } else { 0x1100 };
            exec(&i, &mut c, &mut ram).unwrap();
            assert_eq!(c.x, expected);
            assert_eq!(ram.mem, mem);
            finish(&c);
        }
    }
}
#[test]
fn push() {
    stack(true);
}
#[test]
fn pop() {
    stack(false);
}
fn moves(save: bool) {
    let regs = [8, 9, 18, 19, 20, 21, 22, 23];
    for (a, &r1) in regs.iter().enumerate() {
        for (b, &r2) in regs.iter().enumerate() {
            let raw = if save { 0xac22 } else { 0xac62 } | ((a as u16) << 7) | ((b as u16) << 2);
            if save && a == b {
                assert_eq!(try_decode(0, u32::from(raw).to_le_bytes()), None);
                continue;
            }
            let mut c = cpu();
            let mut expected = c.x;
            if save {
                expected[r1] = c.x[10];
                expected[r2] = c.x[11];
            } else {
                expected[10] = c.x[r1];
                expected[11] = c.x[r2];
            }
            exec(&decode(raw), &mut c, &mut FlatRam::new(0, 0)).unwrap();
            assert_eq!(c.x, expected);
            finish(&c);
        }
    }
}
#[test]
fn mvsa01() {
    moves(true);
}
#[test]
fn mva01s() {
    moves(false);
}

struct FaultBus {
    ram: FlatRam,
    fail: usize,
    accesses: Vec<(u32, u8, bool)>,
}
impl FaultBus {
    fn access(&mut self, a: u32, w: u8, s: bool) -> Result<(), Fault> {
        self.accesses.push((a, w, s));
        if self.accesses.len() == self.fail {
            Err(Fault::Prohibited)
        } else {
            Ok(())
        }
    }
}
impl Bus for FaultBus {
    fn note_code_page(&mut self, _: u32) {}
    fn fetch(&mut self, _: u32) -> Result<[u8; 4], Fault> {
        panic!("unexpected fetch")
    }
    fn read8(&mut self, a: u32) -> Result<u8, Fault> {
        self.access(a, 1, false)?;
        self.ram.read8(a)
    }
    fn read16(&mut self, a: u32) -> Result<u16, Fault> {
        self.access(a, 2, false)?;
        self.ram.read16(a)
    }
    fn read32(&mut self, a: u32) -> Result<u32, Fault> {
        self.access(a, 4, false)?;
        self.ram.read32(a)
    }
    fn write8(&mut self, a: u32, v: u8) -> Result<(), Fault> {
        self.access(a, 1, true)?;
        self.ram.write8(a, v)
    }
    fn write16(&mut self, a: u32, v: u16) -> Result<(), Fault> {
        self.access(a, 2, true)?;
        self.ram.write16(a, v)
    }
    fn write32(&mut self, a: u32, v: u32) -> Result<(), Fault> {
        self.access(a, 4, true)?;
        self.ram.write32(a, v)
    }
}
#[test]
fn mid_sequence_faults_preserve_registers_and_completed_stores() {
    for push in [true, false] {
        for fail in 1..=13 {
            let mut c = cpu();
            c.x[2] = if push { 0x1100 } else { 0x10c0 };
            let before = c.x;
            let mut bus = FaultBus {
                ram: FlatRam::new(0x1000, 256),
                fail,
                accesses: vec![],
            };
            bus.ram.mem.fill(0xaa);
            let mut mem = bus.ram.mem.clone();
            let i = decode(if push { 0xb8f2 } else { 0xbaf2 });
            let err = exec(&i, &mut c, &mut bus).unwrap_err();
            assert_eq!(
                err,
                ExtFault::Memory {
                    address: 0x1100 - 4 * fail as u32,
                    width: 4,
                    store: push,
                    fault: Fault::Prohibited
                }
            );
            assert_eq!(c.x, before);
            assert_eq!(c.pc, 0);
            assert_eq!(counts(&c), (0, 0, 0));
            assert_eq!(bus.accesses.len(), fail);
            if push {
                for k in 0..fail - 1 {
                    let offset = 252 - k * 4;
                    mem[offset..offset + 4].copy_from_slice(&before[LIST[12 - k]].to_le_bytes());
                }
            }
            assert_eq!(bus.ram.mem, mem);
            bus.fail = usize::MAX;
            exec(&i, &mut c, &mut bus).unwrap();
            finish(&c);
        }
    }
}
#[test]
fn zcb_faults_and_pc_mismatch_have_no_writeback() {
    for (raw, w, s) in [
        (0x8000, 1, false),
        (0x8400, 2, false),
        (0x8440, 2, false),
        (0x8800, 1, true),
        (0x8c00, 2, true),
    ] {
        let mut c = cpu();
        c.x[8] = 0x1000;
        let before = c.x;
        let mut bus = FaultBus {
            ram: FlatRam::new(0x1000, 4),
            fail: 1,
            accesses: vec![],
        };
        let i = decode(raw);
        assert_eq!(
            exec(&i, &mut c, &mut bus),
            Err(ExtFault::Memory {
                address: 0x1000,
                width: w,
                store: s,
                fault: Fault::Prohibited
            })
        );
        assert_eq!(c.x, before);
        assert_eq!(counts(&c), (0, 0, 0));
        assert_eq!(bus.ram.mem, vec![0; 4]);
        c.pc = 4;
        assert_eq!(
            exec(&i, &mut c, &mut bus),
            Err(ExtFault::PcMismatch {
                decoded: 0,
                current: 4
            })
        );
        assert_eq!(bus.accesses.len(), 1);
        assert_eq!(c.x, before);
    }
}
#[test]
fn unsupported_returns_reserved_zcmt_rv64_and_x0() {
    for list in 0..16 {
        for sp in 0..4 {
            for base in [0xb802u32, 0xba02, 0xbc02, 0xbe02] {
                let raw = base | (list << 4) | (sp << 2);
                if list < 4 || base >= 0xbc02 {
                    assert_eq!(try_decode(0, raw.to_le_bytes()), None, "{raw:x}");
                }
            }
        }
    }
    for raw in [
        0u32,
        0x0001,
        0x8c00 | 0x40,
        0x9c71,
        0x9c79,
        0x9c7d,
        0xa002,
        0xa082,
        0x8003,
    ] {
        assert_eq!(try_decode(0, raw.to_le_bytes()), None, "{raw:x}");
    }
    // All accepted Zc operands use x8-x15 or UABI saved registers; x0 is NOT legal.
    for raw in 0..=u16::MAX {
        if let Some(i) = try_decode(0, u32::from(raw).to_le_bytes()) {
            assert_ne!(i.r1, 0);
            assert_ne!(i.r2, 0);
        }
    }
}
#[test]
fn wrapping_addresses_pc_and_16_byte_adjustment_not_sp_rounding() {
    let mut c = cpu();
    c.pc = u32::MAX - 1;
    c.x[8] = u32::MAX;
    let mut ram = FlatRam::new(0, 4);
    ram.mem[0] = 0x80;
    let i = try_decode(c.pc, 0x8040u32.to_le_bytes()).unwrap();
    exec(&i, &mut c, &mut ram).unwrap();
    assert_eq!(c.x[8], 128);
    assert_eq!(c.pc, 0);
    c = cpu();
    c.x[2] = 0x1104;
    let mut ram = FlatRam::new(0x1000, 0x104);
    exec(&decode(0xb842), &mut c, &mut ram).unwrap();
    assert_eq!(c.x[2], 0x10f4);
    assert_eq!(ram.read32(0x1100).unwrap(), c.x[1]);
}

#[test]
fn exhaustive_decode_matches_spec_encoding_construction() {
    // Construct every supported encoding from Zc v1.0 sections 2.1-2.12,
    // 3.7-3.8 and 3.11-3.12; compare all 65536 halfwords including rejects.
    let mut expected = std::collections::BTreeMap::new();
    for a in 0..8u16 {
        for b in 0..8u16 {
            for (funct, name) in [(0, "c.lbu"), (2, "c.sb")] {
                for imm in 0..4 {
                    expected.insert(
                        (4 << 13) | (funct << 10) | (a << 7) | (imm << 5) | (b << 2),
                        name,
                    );
                }
            }
            for (funct, sign, name) in [(1, 0, "c.lhu"), (1, 1, "c.lh"), (3, 0, "c.sh")] {
                for imm in 0..2 {
                    expected.insert(
                        (4 << 13) | (funct << 10) | (a << 7) | (sign << 6) | (imm << 5) | (b << 2),
                        name,
                    );
                }
            }
            expected.insert(
                (4 << 13) | (7 << 10) | (a << 7) | (2 << 5) | (b << 2) | 1,
                "c.mul",
            );
            if a != b {
                expected.insert(
                    (5 << 13) | (3 << 10) | (a << 7) | (1 << 5) | (b << 2) | 2,
                    "cm.mvsa01",
                );
            }
            expected.insert(
                (5 << 13) | (3 << 10) | (a << 7) | (3 << 5) | (b << 2) | 2,
                "cm.mva01s",
            );
        }
        for (sub, name) in [
            (0, "c.zext.b"),
            (1, "c.sext.b"),
            (2, "c.zext.h"),
            (3, "c.sext.h"),
            (5, "c.not"),
        ] {
            expected.insert(
                (4 << 13) | (7 << 10) | (a << 7) | (3 << 5) | (sub << 2) | 1,
                name,
            );
        }
    }
    for (funct, name) in [(24, "cm.push"), (26, "cm.pop")] {
        for list in 4..16 {
            for sp in 0..4 {
                expected.insert((5 << 13) | (funct << 8) | (list << 4) | (sp << 2) | 2, name);
            }
        }
    }
    for raw in 0..=u16::MAX {
        let bytes = u32::from(raw).to_le_bytes();
        assert_eq!(
            try_decode(0, bytes).map(|i| i.op.mnemonic()),
            expected.get(&raw).copied(),
            "{raw:04x}"
        );
        let mut bytes = bytes;
        bytes[2] = 0xff;
        bytes[3] = 0xff;
        assert_eq!(
            try_decode(0, bytes).map(|i| i.op.mnemonic()),
            expected.get(&raw).copied(),
            "upper halfword {raw:04x}"
        );
    }
}
