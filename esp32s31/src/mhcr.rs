//! S31-local retained branch-prediction hints; no architectural prediction effect.
//! IDF 9a97f6c54ec638111ce55cd36581b3c192f15207 rv_utils.h:52-57,557-570
//! defines MHCR=0x7c1, RS=bit4, BFE=bit5, BTB=bit12, including S31.
//! Sibling-sourced reset: ESP32-C5 TRM v1.1 p122, register 2.97: all bits zero.
//! https://documentation.espressif.com/esp32-c5_technical_reference_manual_en.pdf
//! SHA256 b2e7944dec0ee8e933042de69eb418da28a959d75afddde660775e9395739240.
//! This is not an independently verified S31 silicon reset value.
use riscv_rv32::{
    decode::{Insn, Op},
    Cpu,
};

#[derive(Default)]
pub(crate) struct Mhcr {
    value: u32,
}
impl Mhcr {
    pub(crate) fn execute(&mut self, insn: &Insn, cpu: &mut Cpu) -> bool {
        if insn.imm as u32 != 0x7c1
            || !matches!(
                insn.op,
                Op::Csrrw | Op::Csrrs | Op::Csrrc | Op::Csrrwi | Op::Csrrsi | Op::Csrrci
            )
        {
            return false;
        }
        let source = if matches!(insn.op, Op::Csrrwi | Op::Csrrsi | Op::Csrrci) {
            insn.rs1 as u32
        } else if insn.rs1 == 0 {
            0
        } else {
            cpu.x[insn.rs1 as usize]
        };
        let old = self.value;
        if matches!(insn.op, Op::Csrrw | Op::Csrrwi) || insn.rs1 != 0 {
            self.value = match insn.op {
                Op::Csrrs | Op::Csrrsi => old | source,
                Op::Csrrc | Op::Csrrci => old & !source,
                _ => source,
            };
        }
        if insn.rd != 0 {
            cpu.x[insn.rd as usize] = old;
        }
        cpu.x[0] = 0;
        cpu.pc = cpu.pc.wrapping_add(4);
        cpu.insn_count += 1;
        cpu.retired_count += 1;
        cpu.cycle_count += 1;
        true
    }
}

#[cfg(test)]
mod tests {

    use crate::{Probe, StopReason, HP_SRAM};
    use emu_core::Bus;
    #[test]
    fn mhcr_mapping_retention_all_forms_and_counters() {
        let mut p = Probe::new();
        p.cpu.pc = HP_SRAM.0;
        p.cpu.x[1] = 0x1030;
        p.cpu.x[0] = 0xdeadbeef;
        assert_eq!(p.mhcr.value, 0);
        for (i, (funct, source, rd, old, new)) in [
            (2, 1, 1, 0, 0x1030), // ROM's aliased set
            (2, 0, 2, 0x1030, 0x1030),
            (1, 0, 0, 0, 0), // x0 is zero even if storage was dirty
            (5, 31, 2, 0, 31),
            (7, 3, 2, 31, 28),
            (6, 2, 2, 28, 30),
            (3, 1, 2, 30, 30), // x1 now contains old=0, nonzero index
            (1, 2, 2, 30, 30), // alias exchange
            (3, 2, 0, 0, 0),
            (5, 0, 0, 0, 0),
        ]
        .into_iter()
        .enumerate()
        {
            let pc = p.cpu.pc;
            p.bus
                .write32(
                    pc,
                    (0x7c1 << 20) | (source << 15) | (funct << 12) | (rd << 7) | 0x73,
                )
                .unwrap();
            assert_eq!(p.run(1).reason, StopReason::Budget);
            assert_eq!(p.mhcr.value, new);
            if rd != 0 {
                assert_eq!(p.cpu.x[rd as usize], old);
            }
            assert_eq!(p.cpu.x[0], 0);
            assert_eq!(p.cpu.pc, pc + 4);
            assert_eq!(
                (p.cpu.insn_count, p.cpu.retired_count, p.cpu.cycle_count),
                (i as u64 + 1, i as u64 + 1, i as u64 + 1)
            );
            assert!(!p.cpu.csr_other.contains_key(&0x7c1));
        }
        for csr in [0x7c0, 0x7c2] {
            p.bus.write32(p.cpu.pc, (csr << 20) | 0x1073).unwrap();
            assert_eq!(p.run(1).reason, StopReason::Csr(csr));
        }
    }
}
