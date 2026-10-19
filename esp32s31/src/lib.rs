//! Strict, single-HP-core ESP32-S31 reset probe. Not native-ROM-to-application support.
//! No guessed MMIO defaults, CLIC, MMU, cache timing, FPU, or secondary core.
use emu_core::{Bus, Core, Fault, StepKind};
use riscv_rv32::{
    decode::{decode, Op},
    Cpu,
};

pub const CHIP: &str = "esp32s31";
// ESP-IDF 9a97f6c54ec638111ce55cd36581b3c192f15207,
// components/soc/esp32s31/include/soc/soc.h:138-217. All ranges half-open.
pub const RESET: u32 = 0x2f80_0000;
pub const ROM: (u32, u32) = (RESET, 0x2f85_0000);
pub const HP_SRAM: (u32, u32) = (0x2f00_0000, 0x2f08_0000);
pub const LP_SRAM: (u32, u32) = (0x2e00_0000, 0x2e00_8000);
pub const FLASH: (u32, u32) = (0x4000_0000, 0x4400_0000);
pub const PSRAM: (u32, u32) = (0x5000_0000, 0x5400_0000);
// These are ADDITIVE offsets, not alias base addresses. Also see the pinned IDF
// components/esp_hw_support/port/esp32s31/cpu_region_protect.c:90-101.
pub const FLASH_NC_OFFSET: u32 = 0x6000_0000;
pub const PSRAM_NC_OFFSET: u32 = 0x7000_0000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region {
    Rom,
    HpSram,
    LpSram,
    Flash,
    Psram,
    Mmio,
}

/// Virtual aperture classification only; does not imply a configured MMU or backing.
pub fn locate(addr: u32) -> Option<(Region, usize)> {
    for (region, (lo, hi)) in [
        (Region::Rom, ROM),
        (Region::HpSram, HP_SRAM),
        (Region::LpSram, LP_SRAM),
        (Region::Flash, FLASH),
        (Region::Psram, PSRAM),
        (
            Region::Flash,
            (FLASH.0 + FLASH_NC_OFFSET, FLASH.1 + FLASH_NC_OFFSET),
        ),
        (
            Region::Psram,
            (PSRAM.0 + PSRAM_NC_OFFSET, PSRAM.1 + PSRAM_NC_OFFSET),
        ),
    ] {
        if (lo..hi).contains(&addr) {
            return Some((region, (addr - lo) as usize));
        }
    }
    // Apertures only: every access faults until a defined device is implemented.
    if (0x1000_0000..0x2000_0000).contains(&addr) || (0x2010_0000..0x2e00_0000).contains(&addr) {
        return Some((Region::Mmio, 0));
    }
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccessFault {
    pub address: u32,
    pub width: usize,
    pub write: bool,
    pub fault: Fault,
}

pub struct S31Bus {
    rom: Vec<u8>,
    rom_present: Vec<bool>,
    hp: Vec<u8>,
    lp: Vec<u8>,
    /// First fault only, including width and direction; unknown MMIO never reads as zero.
    pub fault: Option<AccessFault>,
}

impl Default for S31Bus {
    fn default() -> Self {
        Self::new()
    }
}
impl S31Bus {
    pub fn new() -> Self {
        Self {
            rom: vec![0; (ROM.1 - ROM.0) as usize],
            rom_present: vec![false; (ROM.1 - ROM.0) as usize],
            hp: vec![0; (HP_SRAM.1 - HP_SRAM.0) as usize],
            lp: vec![0; (LP_SRAM.1 - LP_SRAM.0) as usize],
            fault: None,
        }
    }

    /// Load only ROM-addressed PT_LOAD contents. No host .eh_frame at zero, no
    /// direct-app loading, and no preinitialization of RAM sections: ROM does that.
    pub fn load_rom(&mut self, bytes: &[u8]) -> Result<(), String> {
        if bytes.get(18..20) != Some(&243u16.to_le_bytes()) {
            return Err("need RISC-V ELF".into());
        }
        let elf = esp_soc::elf::parse(bytes)?;
        if elf.entry != RESET {
            return Err("S31 ROM entry must be 0x2f800000".into());
        }
        let mut image = vec![0; self.rom.len()];
        let mut present = vec![false; image.len()];
        for seg in &elf.segments {
            if seg.data.is_empty() || seg.vaddr == 0 {
                continue;
            }
            let off = seg.vaddr.checked_sub(ROM.0).ok_or("non-ROM load segment")? as usize;
            off.checked_add(seg.memsz as usize)
                .filter(|&end| end <= image.len())
                .ok_or("ROM segment exceeds aperture")?;
            let data_end = off + seg.data.len();
            image[off..data_end].copy_from_slice(&seg.data);
            // A missing ROM file byte isn't writable zero-filled RAM.
            present[off..data_end].fill(true);
        }
        if !present[..4].iter().all(|&b| b) {
            return Err("ROM reset instruction missing".into());
        }
        self.rom = image;
        self.rom_present = present;
        self.fault = None;
        Ok(())
    }

    fn fail<T>(
        &mut self,
        address: u32,
        width: usize,
        write: bool,
        fault: Fault,
    ) -> Result<T, Fault> {
        self.fault.get_or_insert(AccessFault {
            address,
            width,
            write,
            fault,
        });
        Err(fault)
    }

    fn span(&mut self, addr: u32, n: usize, write: bool) -> Result<(Region, usize), Fault> {
        let Some((region, off)) = locate(addr) else {
            return self.fail(addr, n, write, Fault::Unmapped);
        };
        let Some(last) = addr.checked_add(n as u32 - 1) else {
            return self.fail(addr, n, write, Fault::Unmapped);
        };
        if locate(last) != Some((region, off + n - 1)) {
            return self.fail(addr, n, write, Fault::Unmapped);
        }
        if write && region == Region::Rom {
            return self.fail(addr, n, write, Fault::Prohibited);
        }
        match region {
            Region::Rom if self.rom_present[off..off + n].iter().all(|&b| b) => Ok((region, off)),
            Region::HpSram | Region::LpSram => Ok((region, off)),
            // Cache windows are declared but deliberately NOT identity-mapped to
            // flash storage. Their MMU/reset configuration is not implemented.
            _ => self.fail(addr, n, write, Fault::Unmapped),
        }
    }
    fn read<const N: usize>(&mut self, addr: u32) -> Result<[u8; N], Fault> {
        let (r, o) = self.span(addr, N, false)?;
        let mem = match r {
            Region::Rom => &self.rom,
            Region::HpSram => &self.hp,
            Region::LpSram => &self.lp,
            _ => unreachable!(),
        };
        Ok(mem[o..o + N].try_into().unwrap())
    }
    fn write(&mut self, addr: u32, bytes: &[u8]) -> Result<(), Fault> {
        let (r, o) = self.span(addr, bytes.len(), true)?;
        let mem = match r {
            Region::HpSram => &mut self.hp,
            Region::LpSram => &mut self.lp,
            _ => unreachable!(),
        };
        mem[o..o + bytes.len()].copy_from_slice(bytes);
        Ok(())
    }
}

impl Bus for S31Bus {
    fn read8(&mut self, a: u32) -> Result<u8, Fault> {
        Ok(self.read::<1>(a)?[0])
    }
    fn read16(&mut self, a: u32) -> Result<u16, Fault> {
        Ok(u16::from_le_bytes(self.read(a)?))
    }
    fn read32(&mut self, a: u32) -> Result<u32, Fault> {
        Ok(u32::from_le_bytes(self.read(a)?))
    }
    fn write8(&mut self, a: u32, v: u8) -> Result<(), Fault> {
        self.write(a, &[v])
    }
    fn write16(&mut self, a: u32, v: u16) -> Result<(), Fault> {
        self.write(a, &v.to_le_bytes())
    }
    fn write32(&mut self, a: u32, v: u32) -> Result<(), Fault> {
        self.write(a, &v.to_le_bytes())
    }
    fn fetch(&mut self, pc: u32) -> Result<[u8; 4], Fault> {
        // Do not fault a valid compressed instruction at an aperture's final halfword.
        let lo = self.read::<2>(pc)?;
        if lo[0] & 3 != 3 {
            return Ok([lo[0], lo[1], 0, 0]);
        }
        self.read(pc)
    }
    fn note_code_page(&mut self, _: u32) {}
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StopReason {
    Access(AccessFault),
    Opcode,
    Csr(u32),
    Trap(emu_core::Trap),
    Waiting,
    Budget,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stop {
    pub pc: u32,
    pub raw: u32,
    pub retired: u64,
    pub reason: StopReason,
}

pub struct Probe {
    pub cpu: Cpu,
    pub bus: S31Bus,
}
impl Default for Probe {
    fn default() -> Self {
        Self::new()
    }
}
impl Probe {
    pub fn new() -> Self {
        let mut cpu = Cpu::new_rv32imac();
        cpu.pc = RESET;
        Self {
            cpu,
            bus: S31Bus::new(),
        }
    }
    pub fn run(&mut self, budget: u64) -> Stop {
        for _ in 0..budget {
            let pc = self.cpu.pc;
            let bytes = match self.bus.fetch(pc) {
                Ok(b) => b,
                Err(_) => {
                    return Stop {
                        pc,
                        raw: 0,
                        retired: self.cpu.retired_count,
                        reason: StopReason::Access(self.bus.fault.unwrap()),
                    }
                }
            };
            let insn = decode(pc, bytes);
            let stop = |reason, retired| Stop {
                pc,
                raw: insn.raw,
                retired,
                reason,
            };
            if insn.is_illegal() {
                return stop(StopReason::Opcode, self.cpu.retired_count);
            }
            if matches!(
                insn.op,
                Op::Csrrw | Op::Csrrs | Op::Csrrc | Op::Csrrwi | Op::Csrrsi | Op::Csrrci
            ) {
                // Only the reset-prefix CSRs have audited semantics for this probe.
                // Do not silently inherit the C3/C6 generic unknown-CSR readback.
                if !matches!(insn.imm as u32, 0x300 | 0xf14) {
                    return stop(StopReason::Csr(insn.imm as u32), self.cpu.retired_count);
                }
            }
            let outcome = self.cpu.step(&mut self.bus);
            if let Some(f) = self.bus.fault {
                return stop(StopReason::Access(f), self.cpu.retired_count);
            }
            if let StepKind::TrapBefore(t) | StepKind::TrapDuring(t) = outcome.kind {
                return stop(StopReason::Trap(t), self.cpu.retired_count);
            }
            if self.cpu.waiting {
                return stop(StopReason::Waiting, self.cpu.retired_count);
            }
        }
        Stop {
            pc: self.cpu.pc,
            raw: 0,
            retired: self.cpu.retired_count,
            reason: StopReason::Budget,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn map_pins_every_aperture_and_boundary() {
        for (region, lo, hi) in [
            (Region::Rom, 0x2f800000, 0x2f850000),
            (Region::HpSram, 0x2f000000, 0x2f080000),
            (Region::LpSram, 0x2e000000, 0x2e008000),
            (Region::Flash, 0x40000000, 0x44000000),
            (Region::Psram, 0x50000000, 0x54000000),
            (Region::Flash, 0xa0000000, 0xa4000000),
            (Region::Psram, 0xc0000000, 0xc4000000),
        ] {
            assert_eq!(locate(lo), Some((region, 0)));
            assert_eq!(locate(hi - 1), Some((region, (hi - lo - 1) as usize)));
            assert_eq!(locate(hi), None);
            assert_ne!(locate(lo - 1).map(|x| x.0), Some(region));
        }
        for a in [
            0x10000000, 0x1fffffff, 0x20100000, 0x2c000000, 0x2c01ffff, 0x2dffffff,
        ] {
            assert_eq!(locate(a), Some((Region::Mmio, 0)));
        }
        for a in [0, 0x20000000, 0x60000000, 0x70000000, 0xffffffff] {
            assert_eq!(locate(a), None);
        }
        assert_eq!(CHIP, "esp32s31");
        assert_eq!(RESET, 0x2f800000);
    }
    #[test]
    fn ram_widths_edges_and_no_cross_boundary_write() {
        let mut b = S31Bus::new();
        for (lo, hi) in [(0x2f000000, 0x2f080000), (0x2e000000, 0x2e008000)] {
            assert_eq!(b.read32(lo).unwrap(), 0);
            b.write32(lo, 0x12345678).unwrap();
            assert_eq!(b.read8(lo + 1).unwrap(), 0x56);
            b.write16(hi - 2, 0xabcd).unwrap();
            assert_eq!(b.read16(hi - 2).unwrap(), 0xabcd);
            assert!(b.write32(hi - 2, 0).is_err());
            assert_eq!(b.read16(hi - 2).unwrap(), 0xabcd);
        }
    }
    #[test]
    fn holes_mmio_windows_and_rom_writes_fail_closed() {
        let mut b = S31Bus::new();
        assert_eq!(b.write8(RESET, 1), Err(Fault::Prohibited));
        b.fault = None;
        assert!(b.fetch(RESET).is_err());
        b.fault = None;
        assert_eq!(b.read32(0x20701030), Err(Fault::Unmapped));
        assert_eq!(
            b.fault,
            Some(AccessFault {
                address: 0x20701030,
                width: 4,
                write: false,
                fault: Fault::Unmapped
            })
        );
        for a in [0x40000000, 0x50000000, 0xa0000000, 0xc0000000] {
            assert!(b.read32(a).is_err());
        }
    }
    #[test]
    fn compressed_fetch_at_end_and_strict_opcode_stop() {
        let mut p = Probe::new();
        p.bus.rom[..2].copy_from_slice(&[0, 0]);
        p.bus.rom_present[..2].fill(true);
        assert_eq!(
            p.run(10),
            Stop {
                pc: RESET,
                raw: 0,
                retired: 0,
                reason: StopReason::Opcode
            }
        );
        p.cpu.pc = HP_SRAM.1 - 2;
        p.bus.write16(HP_SRAM.1 - 2, 0x0001).unwrap();
        assert_eq!(p.bus.fetch(HP_SRAM.1 - 2).unwrap(), [1, 0, 0, 0]);
    }
    #[test]
    fn unknown_csr_does_not_use_generic_readback() {
        let mut p = Probe::new();
        p.cpu.pc = HP_SRAM.0;
        p.bus.write32(HP_SRAM.0, 0x3063d073).unwrap();
        assert_eq!(p.run(10).reason, StopReason::Csr(0x306));
        assert_eq!(p.cpu.retired_count, 0);
    }
    fn rom_fixture() -> Vec<u8> {
        // Minimal ELF32 with two ROM PT_LOADs and host metadata at address zero.
        // Reset-prefix bytes transcribed from the pinned public ROM, not a ROM blob.
        let code: [u32; 6] = [
            0x30001073, 0xf14022f3, 0x02029363, 0x20701337, 0x03030313, 0x00032303,
        ];
        let mut elf = vec![0u8; 52 + 3 * 32 + 4 + 24 + 4];
        elf[..7].copy_from_slice(b"\x7fELF\x01\x01\x01");
        elf[18..20].copy_from_slice(&243u16.to_le_bytes());
        elf[24..28].copy_from_slice(&0x2f800000u32.to_le_bytes());
        elf[28..32].copy_from_slice(&52u32.to_le_bytes());
        elf[42..44].copy_from_slice(&32u16.to_le_bytes());
        elf[44..46].copy_from_slice(&3u16.to_le_bytes());
        for (i, addr, offset, size) in [
            (0, 0x2f800000u32, 148u32, 4u32),
            (1, 0x2f801710, 152, 24),
            (2, 0, 176, 4),
        ] {
            let p = 52 + i * 32;
            for (o, v) in [
                (0, 1u32),
                (4, offset),
                (8, addr),
                (12, addr),
                (16, size),
                (20, size),
                (24, 5),
            ] {
                elf[p + o..p + o + 4].copy_from_slice(&v.to_le_bytes());
            }
        }
        elf[148..152].copy_from_slice(&0x7100106fu32.to_le_bytes());
        for (i, word) in code.iter().enumerate() {
            elf[152 + i * 4..156 + i * 4].copy_from_slice(&word.to_le_bytes());
        }
        elf
    }
    #[test]
    fn loader_and_reset_prefix_stop_without_external_rom() {
        let elf = rom_fixture();
        let mut p = Probe::new();
        p.bus.load_rom(&elf).unwrap();
        assert_eq!(
            p.run(1000),
            Stop {
                pc: 0x2f801724,
                raw: 0x00032303,
                retired: 6,
                reason: StopReason::Access(AccessFault {
                    address: 0x20701030,
                    width: 4,
                    write: false,
                    fault: Fault::Unmapped
                })
            }
        );
        assert!(p.bus.read32(0).is_err());
        assert!(p.bus.read32(0x2f800004).is_err()); // ELF hole, not executable zeros
        assert_eq!(p.bus.read32(0x2f800000).unwrap(), 0x7100106f);
        assert_eq!(p.bus.write32(0x2f800000, 0), Err(Fault::Prohibited));
    }
    #[test]
    fn invalid_rom_is_rejected_without_overwriting_loaded_image() {
        let elf = rom_fixture();
        let mut b = S31Bus::new();
        b.load_rom(&elf).unwrap();
        for (offset, value) in [(18, 0u32), (24, 0x40000000), (60, 0x2f84ffff)] {
            let mut bad = elf.clone();
            bad[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            assert!(b.load_rom(&bad).is_err());
            assert_eq!(b.read32(0x2f800000).unwrap(), 0x7100106f);
        }
        assert!(b.load_rom(b"not ELF").is_err());
    }
    #[test]
    #[ignore = "set S31_ROM to the pinned public esp32s31_rev0_rom.elf"]
    fn external_pinned_rom_first_stop() {
        let rom = std::fs::read(std::env::var_os("S31_ROM").expect("set S31_ROM")).unwrap();
        let mut p = Probe::new();
        p.bus.load_rom(&rom).unwrap();
        assert_eq!(p.bus.read32(0).err(), Some(Fault::Unmapped));
        p.bus.fault = None;
        assert_eq!(
            p.run(1000),
            Stop {
                pc: 0x2f801724,
                raw: 0x00032303,
                retired: 6,
                reason: StopReason::Access(AccessFault {
                    address: 0x20701030,
                    width: 4,
                    write: false,
                    fault: Fault::Unmapped
                })
            }
        );
    }
}
