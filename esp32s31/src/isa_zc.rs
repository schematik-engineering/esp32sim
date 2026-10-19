//! S31-local RV32 Zcb/Zcmp subset, ratified Zc v1.0.
//! https://github.com/riscvarchive/riscv-code-size-reduction/releases/tag/v1.0
//! Inventory bounded: no Zcmt, RV64 c.zext.w, cm.popret or cm.popretz.
//! Not wired into Probe::run. Caller owns interrupt/fetch arbitration.
use emu_core::{Bus, Fault};
use riscv_rv32::Cpu;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Lbu,
    Lhu,
    Lh,
    Sb,
    Sh,
    ZextB,
    SextB,
    ZextH,
    SextH,
    Not,
    Mul,
    Push,
    Pop,
    Mvsa01,
    Mva01s,
}
impl Op {
    pub fn mnemonic(self) -> &'static str {
        match self {
            Self::Lbu => "c.lbu",
            Self::Lhu => "c.lhu",
            Self::Lh => "c.lh",
            Self::Sb => "c.sb",
            Self::Sh => "c.sh",
            Self::ZextB => "c.zext.b",
            Self::SextB => "c.sext.b",
            Self::ZextH => "c.zext.h",
            Self::SextH => "c.sext.h",
            Self::Not => "c.not",
            Self::Mul => "c.mul",
            Self::Push => "cm.push",
            Self::Pop => "cm.pop",
            Self::Mvsa01 => "cm.mvsa01",
            Self::Mva01s => "cm.mva01s",
        }
    }
}

/// Private operands: exec only accepts validated encodings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtInsn {
    pub pc: u32,
    pub op: Op,
    pub length: u8,
    r1: u8,
    r2: u8,
    imm: u32,
    count: u8,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtFault {
    PcMismatch {
        decoded: u32,
        current: u32,
    },
    Memory {
        address: u32,
        width: u8,
        store: bool,
        fault: Fault,
    },
}
// UABI list in ascending architectural order; rlist=15 includes BOTH s10/s11.
const REGS: [u8; 13] = [1, 8, 9, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27];
fn sreg(n: u8) -> u8 {
    if n < 2 {
        n + 8
    } else {
        n + 16
    }
}

pub fn try_decode(pc: u32, bytes: [u8; 4]) -> Option<ExtInsn> {
    use Op::*;
    let raw = u16::from_le_bytes([bytes[0], bytes[1]]);
    let mut i = ExtInsn {
        pc,
        op: Lbu,
        length: 2,
        r1: ((raw >> 7) & 7) as u8 + 8,
        r2: ((raw >> 2) & 7) as u8 + 8,
        imm: 0,
        count: 0,
    };
    i.op = match raw & 0xfc03 {
        0x8000 => {
            i.imm = u32::from(((raw >> 5) & 1) * 2 + ((raw >> 6) & 1));
            Lbu
        }
        0x8800 => {
            i.imm = u32::from(((raw >> 5) & 1) * 2 + ((raw >> 6) & 1));
            Sb
        }
        _ => match raw & 0xfc43 {
            0x8400 => {
                i.imm = u32::from((raw >> 5) & 1) * 2;
                Lhu
            }
            0x8440 => {
                i.imm = u32::from((raw >> 5) & 1) * 2;
                Lh
            }
            0x8c00 => {
                i.imm = u32::from((raw >> 5) & 1) * 2;
                Sh
            }
            _ => match raw & 0xfc63 {
                0x9c41 => Mul,
                0x9c61 => match (raw >> 2) & 7 {
                    0 => ZextB,
                    1 => SextB,
                    2 => ZextH,
                    3 => SextH,
                    5 => Not,
                    _ => return None,
                },
                0xac22 | 0xac62 => {
                    i.r1 = sreg(((raw >> 7) & 7) as u8);
                    i.r2 = sreg(((raw >> 2) & 7) as u8);
                    if raw & 0x40 == 0 {
                        if i.r1 == i.r2 {
                            return None;
                        }
                        Mvsa01
                    } else {
                        Mva01s
                    }
                }
                _ => match raw & 0xff03 {
                    0xb802 | 0xba02 => {
                        let rlist = ((raw >> 4) & 15) as u8;
                        if rlist < 4 {
                            return None;
                        }
                        i.count = if rlist == 15 { 13 } else { rlist - 3 };
                        // Round register storage UP to 16, then add spimm * 16.
                        i.imm = (u32::from(i.count) * 4).div_ceil(16) * 16
                            + u32::from((raw >> 2) & 3) * 16;
                        if raw & 0x0200 == 0 {
                            Push
                        } else {
                            Pop
                        }
                    }
                    _ => return None,
                },
            },
        },
    };
    Some(i)
}

/// Fault policy: PC, registers and counters commit only after all accesses succeed.
/// Push stores highest numbered listed register at old_sp-4, descending thereafter;
/// completed stores remain visible on a later fault and may repeat on retry.
/// Pop stages loads (including ra); a fault exposes NO partial register writeback.
/// Bus read side effects cannot be undone. No attempt to roll back MMIO effects.
/// Stack adjustment is 16-byte granular, not a rounding of the incoming sp.
/// One successful instruction matches shared step_outcome's three counters.
pub fn exec(insn: &ExtInsn, cpu: &mut Cpu, bus: &mut impl Bus) -> Result<(), ExtFault> {
    use Op::*;
    if insn.pc != cpu.pc {
        return Err(ExtFault::PcMismatch {
            decoded: insn.pc,
            current: cpu.pc,
        });
    }
    let mut x = cpu.x;
    x[0] = 0;
    let a = x[usize::from(insn.r1)];
    let b = x[usize::from(insn.r2)];
    let address = a.wrapping_add(insn.imm);
    let fault = |width, store, fault| ExtFault::Memory {
        address,
        width,
        store,
        fault,
    };
    match insn.op {
        Lbu => {
            x[usize::from(insn.r2)] = u32::from(bus.read8(address).map_err(|e| fault(1, false, e))?)
        }
        Lhu => {
            x[usize::from(insn.r2)] =
                u32::from(bus.read16(address).map_err(|e| fault(2, false, e))?)
        }
        Lh => {
            x[usize::from(insn.r2)] =
                bus.read16(address).map_err(|e| fault(2, false, e))? as i16 as i32 as u32
        }
        Sb => bus
            .write8(address, b as u8)
            .map_err(|e| fault(1, true, e))?,
        Sh => bus
            .write16(address, b as u16)
            .map_err(|e| fault(2, true, e))?,
        ZextB => x[usize::from(insn.r1)] = a & 0xff,
        SextB => x[usize::from(insn.r1)] = a as i8 as i32 as u32,
        ZextH => x[usize::from(insn.r1)] = a & 0xffff,
        SextH => x[usize::from(insn.r1)] = a as i16 as i32 as u32,
        Not => x[usize::from(insn.r1)] = !a,
        Mul => x[usize::from(insn.r1)] = a.wrapping_mul(b),
        Mvsa01 => {
            x[usize::from(insn.r1)] = x[10];
            x[usize::from(insn.r2)] = x[11];
        }
        Mva01s => {
            x[10] = a;
            x[11] = b;
        }
        Push | Pop => {
            let top = if insn.op == Push {
                x[2]
            } else {
                x[2].wrapping_add(insn.imm)
            };
            for (offset, &reg) in REGS[..usize::from(insn.count)].iter().rev().enumerate() {
                let address = top.wrapping_sub((offset as u32 + 1) * 4);
                let fault = |fault| ExtFault::Memory {
                    address,
                    width: 4,
                    store: insn.op == Push,
                    fault,
                };
                if insn.op == Push {
                    bus.write32(address, x[usize::from(reg)]).map_err(fault)?;
                } else {
                    x[usize::from(reg)] = bus.read32(address).map_err(fault)?;
                }
            }
            x[2] = if insn.op == Push {
                x[2].wrapping_sub(insn.imm)
            } else {
                top
            };
        }
    }
    cpu.x = x;
    cpu.pc = cpu.pc.wrapping_add(2);
    cpu.insn_count += 1;
    cpu.cycle_count += 1;
    cpu.retired_count += 1;
    Ok(())
}

#[cfg(test)]
#[path = "isa_zc_tests.rs"]
mod tests;
