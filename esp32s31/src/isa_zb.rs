//! S31-local RV32 subset of ratified Bitmanip v1.0.0 (Zba/Zbb/Zbs).
//! https://github.com/riscv/riscv-bitmanip/releases/tag/1.0.0
//! Only mnemonics present in the pinned second-stage/application inventory.
//! Not wired into the probe: callers must perform interrupt/fetch handling first,
//! then call this executor instead of the shared step (never count both).
use riscv_rv32::Cpu;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Sh1add,
    Sh2add,
    Sh3add,
    Andn,
    Orn,
    Maxu,
    Minu,
    Ctz,
    Cpop,
    OrcB,
    Rev8,
    Rori,
    ZextH,
    Bclr,
    Bclri,
    Bext,
    Bexti,
    Binvi,
    Bset,
    Bseti,
}

impl Op {
    pub fn mnemonic(self) -> &'static str {
        match self {
            Self::Sh1add => "sh1add",
            Self::Sh2add => "sh2add",
            Self::Sh3add => "sh3add",
            Self::Andn => "andn",
            Self::Orn => "orn",
            Self::Maxu => "maxu",
            Self::Minu => "minu",
            Self::Ctz => "ctz",
            Self::Cpop => "cpop",
            Self::OrcB => "orc.b",
            Self::Rev8 => "rev8",
            Self::Rori => "rori",
            Self::ZextH => "zext.h",
            Self::Bclr => "bclr",
            Self::Bclri => "bclri",
            Self::Bext => "bext",
            Self::Bexti => "bexti",
            Self::Binvi => "binvi",
            Self::Bset => "bset",
            Self::Bseti => "bseti",
        }
    }
}

/// Operands are private so only validated encodings can reach exec.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtInsn {
    pub pc: u32,
    pub op: Op,
    pub length: u8,
    rd: u8,
    rs1: u8,
    rs2: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtFault {
    PcMismatch { decoded: u32, current: u32 },
}

pub fn try_decode(pc: u32, bytes: [u8; 4]) -> Option<ExtInsn> {
    use Op::*;
    let raw = u32::from_le_bytes(bytes);
    let rs2 = ((raw >> 20) & 31) as u8;
    // Exact funct7/funct3/opcode checks; unary encodings also constrain rs2.
    let op = match (raw & 0x7f, (raw >> 12) & 7, raw >> 25) {
        (0x33, 2, 0x10) => Sh1add,
        (0x33, 4, 0x10) => Sh2add,
        (0x33, 6, 0x10) => Sh3add,
        (0x33, 7, 0x20) => Andn,
        (0x33, 6, 0x20) => Orn,
        (0x33, 7, 0x05) => Maxu,
        (0x33, 5, 0x05) => Minu,
        (0x33, 4, 0x04) if rs2 == 0 => ZextH,
        (0x13, 1, 0x30) if rs2 == 1 => Ctz,
        (0x13, 1, 0x30) if rs2 == 2 => Cpop,
        (0x13, 5, 0x14) if rs2 == 7 => OrcB,
        (0x13, 5, 0x34) if rs2 == 24 => Rev8,
        (0x13, 5, 0x30) => Rori,
        (0x33, 1, 0x24) => Bclr,
        (0x13, 1, 0x24) => Bclri,
        (0x33, 5, 0x24) => Bext,
        (0x13, 5, 0x24) => Bexti,
        (0x13, 1, 0x34) => Binvi,
        (0x33, 1, 0x14) => Bset,
        (0x13, 1, 0x14) => Bseti,
        _ => return None,
    };
    Some(ExtInsn {
        pc,
        op,
        length: 4,
        rd: ((raw >> 7) & 31) as u8,
        rs1: ((raw >> 15) & 31) as u8,
        rs2,
    })
}

/// One successfully fetched instruction: match shared step_outcome's three counters.
/// This is not a replacement for its interrupt or waiting-state arbitration.
pub fn exec(insn: &ExtInsn, cpu: &mut Cpu) -> Result<(), ExtFault> {
    use Op::*;
    if cpu.pc != insn.pc {
        return Err(ExtFault::PcMismatch {
            decoded: insn.pc,
            current: cpu.pc,
        });
    }
    let a = if insn.rs1 == 0 {
        0
    } else {
        cpu.x[usize::from(insn.rs1)]
    };
    let b = if insn.rs2 == 0 {
        0
    } else {
        cpu.x[usize::from(insn.rs2)]
    };
    let sh = b & 31;
    let imm = u32::from(insn.rs2);
    let value = match insn.op {
        Sh1add => (a << 1).wrapping_add(b),
        Sh2add => (a << 2).wrapping_add(b),
        Sh3add => (a << 3).wrapping_add(b),
        Andn => a & !b,
        Orn => a | !b,
        Maxu => a.max(b),
        Minu => a.min(b),
        Ctz => a.trailing_zeros(),
        Cpop => a.count_ones(),
        OrcB => u32::from_le_bytes(a.to_le_bytes().map(|v| if v == 0 { 0 } else { 255 })),
        Rev8 => a.swap_bytes(),
        Rori => a.rotate_right(imm),
        ZextH => a & 0xffff,
        Bclr => a & !(1 << sh),
        Bclri => a & !(1 << imm),
        Bext => (a >> sh) & 1,
        Bexti => (a >> imm) & 1,
        Binvi => a ^ (1 << imm),
        Bset => a | (1 << sh),
        Bseti => a | (1 << imm),
    };
    cpu.set(insn.rd, value);
    cpu.x[0] = 0;
    cpu.pc = cpu.pc.wrapping_add(4);
    cpu.insn_count += 1;
    cpu.cycle_count += 1;
    cpu.retired_count += 1;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    const BOOT: &str = include_str!("../tests/fixtures/zb/bootloader.tsv");
    const APP: &str = include_str!("../tests/fixtures/zb/app.tsv");

    fn rows(text: &str) -> impl Iterator<Item = (u32, u32, &str)> {
        text.lines().map(|line| {
            let parts: Vec<_> = line.split('\t').collect();
            (
                u32::from_str_radix(parts[0], 16).unwrap(),
                u32::from_str_radix(parts[1], 16).unwrap(),
                parts[2],
            )
        })
    }

    #[test]
    fn every_pinned_linear_site_matches_objdump() {
        for (text, count) in [(BOOT, 119), (APP, 1102)] {
            assert_eq!(rows(text).count(), count);
            for (pc, raw, name) in rows(text) {
                let insn = try_decode(pc, raw.to_le_bytes()).unwrap();
                assert_eq!(insn.op.mnemonic(), name, "{pc:08x}: {raw:08x}");
                assert_eq!((insn.pc, insn.length), (pc, 4));
            }
        }
    }

    // Independent, deliberately simple bit-level reference (not execution expressions).
    fn reference(name: &str, a: u32, b: u32) -> u32 {
        let bit = b % 32;
        match name {
            "sh1add" | "sh2add" | "sh3add" => {
                let multiplier = match name {
                    "sh1add" => 2,
                    "sh2add" => 4,
                    _ => 8,
                };
                ((u64::from(a) * multiplier + u64::from(b)) % (1u64 << 32)) as u32
            }
            "andn" => a & (u32::MAX - b),
            "orn" => a | (u32::MAX - b),
            "maxu" => {
                if u64::from(a) > u64::from(b) {
                    a
                } else {
                    b
                }
            }
            "minu" => {
                if u64::from(a) < u64::from(b) {
                    a
                } else {
                    b
                }
            }
            "ctz" => (0..32).find(|n| a & (1 << n) != 0).unwrap_or(32),
            "cpop" => (0..32).map(|n| (a >> n) & 1).sum(),
            "orc.b" => (0..4)
                .map(|n| {
                    if (a >> (8 * n)) & 255 != 0 {
                        255 << (8 * n)
                    } else {
                        0
                    }
                })
                .sum(),
            "rev8" => (0..4)
                .map(|n| ((a >> (8 * n)) & 255) << (8 * (3 - n)))
                .sum(),
            "rori" => (0..32).map(|n| ((a >> ((n + bit) % 32)) & 1) << n).sum(),
            "zext.h" => a % 65536,
            "bclr" | "bclri" => {
                if (a >> bit) & 1 != 0 {
                    a - (1 << bit)
                } else {
                    a
                }
            }
            "bext" | "bexti" => u32::from(a & (1 << bit) != 0),
            "binvi" => {
                if (a >> bit) & 1 != 0 {
                    a - (1 << bit)
                } else {
                    a + (1 << bit)
                }
            }
            "bset" | "bseti" => {
                if (a >> bit) & 1 == 0 {
                    a + (1 << bit)
                } else {
                    a
                }
            }
            _ => panic!("missing reference {name}"),
        }
    }

    fn check_mnemonic(name: &str) {
        let (pc, raw, _) = rows(BOOT).chain(rows(APP)).find(|r| r.2 == name).unwrap();
        let decoded = try_decode(pc, raw.to_le_bytes()).unwrap();
        assert_eq!(decoded.op.mnemonic(), name);
        let immediate = matches!(
            decoded.op,
            Op::Rori | Op::Bclri | Op::Bexti | Op::Binvi | Op::Bseti
        );
        for a in [
            0,
            u32::MAX,
            0x8000_0000,
            0x8000_8001,
            0x0010_0080,
            0x1234_5678,
        ] {
            for b in [0, 31, 32, 63, u32::MAX, 0x8000_0000, 0x1234_5678] {
                // Preserve funct encoding, replace register fields and immediate as appropriate.
                let mut raw = (raw & !0x000f_8f80) | (5 << 7) | (6 << 15);
                if immediate {
                    raw = (raw & !0x01f0_0000) | ((b & 31) << 20);
                } else if matches!(
                    decoded.op,
                    Op::Sh1add
                        | Op::Sh2add
                        | Op::Sh3add
                        | Op::Andn
                        | Op::Orn
                        | Op::Maxu
                        | Op::Minu
                        | Op::Bclr
                        | Op::Bext
                        | Op::Bset
                ) {
                    raw = (raw & !0x01f0_0000) | (7 << 20);
                }
                for rd in [0, 5, 6, 7] {
                    let raw = (raw & !0xf80) | (rd << 7);
                    let insn = try_decode(pc, raw.to_le_bytes()).unwrap();
                    let mut cpu = Cpu::new();
                    cpu.pc = pc;
                    cpu.x[6] = a;
                    cpu.x[7] = b;
                    cpu.insn_count = 8;
                    cpu.cycle_count = 12;
                    cpu.retired_count = 4;
                    let before = cpu.x;
                    exec(&insn, &mut cpu).unwrap();
                    let expected = reference(name, a, if immediate { b & 31 } else { b });
                    for (reg, previous) in before.into_iter().enumerate() {
                        assert_eq!(
                            cpu.x[reg],
                            if reg == rd as usize && rd != 0 {
                                expected
                            } else {
                                previous
                            },
                            "{name} a={a:x} b={b:x} rd={rd} reg={reg}"
                        );
                    }
                    assert_eq!(cpu.pc, pc.wrapping_add(4));
                    assert_eq!(
                        (cpu.insn_count, cpu.cycle_count, cpu.retired_count),
                        (9, 13, 5)
                    );
                }
            }
        }
    }

    fn decoded_bytes(op: Op) -> [u8; 4] {
        rows(BOOT)
            .chain(rows(APP))
            .find(|r| r.2 == op.mnemonic())
            .unwrap()
            .1
            .to_le_bytes()
    }

    macro_rules! cases { ($($test:ident => $name:literal),* $(,)?) => { $(#[test] fn $test() { check_mnemonic($name); })* }; }
    cases! {
        sh1add => "sh1add", sh2add => "sh2add", sh3add => "sh3add", andn => "andn", orn => "orn",
        maxu => "maxu", minu => "minu", ctz => "ctz", cpop => "cpop", orc_b => "orc.b", rev8 => "rev8",
        rori => "rori", zext_h => "zext.h", bclr => "bclr", bclri => "bclri", bext => "bext",
        bexti => "bexti", binvi => "binvi", bset => "bset", bseti => "bseti",
    }

    #[test]
    fn unsupported_and_reserved_encodings_fall_through() {
        // clz, sext.b, xnor, rol, register binv, RV64 rev8, RV64 rori shamt[5].
        for raw in [
            0x60001013u32,
            0x60401013,
            0x40004033,
            0x60001033,
            0x68001033,
            0x6b805013,
            0x62005013,
            0x08104033,
            0x28805013,
            0x69705013,
        ] {
            assert_eq!(try_decode(0, raw.to_le_bytes()), None, "{raw:08x}");
        }
        assert_eq!(try_decode(0, [0; 4]), None);
    }

    #[test]
    fn x0_source_pc_wrap_and_fault_atomicity() {
        let mut insn = try_decode(u32::MAX - 3, decoded_bytes(Op::Sh2add)).unwrap();
        insn.rs1 = 0;
        insn.rs2 = 0;
        insn.rd = 5;
        let mut cpu = Cpu::new();
        cpu.pc = insn.pc;
        cpu.x[0] = u32::MAX;
        exec(&insn, &mut cpu).unwrap();
        assert_eq!(cpu.x[5], 0);
        assert_eq!(cpu.pc, 0);
        let before = cpu.x;
        assert_eq!(
            exec(&insn, &mut cpu),
            Err(ExtFault::PcMismatch {
                decoded: insn.pc,
                current: 0
            })
        );
        assert_eq!(cpu.x, before);
        assert_eq!(
            (cpu.pc, cpu.insn_count, cpu.cycle_count, cpu.retired_count),
            (0, 1, 1, 1)
        );
    }
}
