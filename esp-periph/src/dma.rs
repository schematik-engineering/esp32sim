//! Checked DMA descriptors shared by controller-local and GDMA engines.
use emu_core::bus::Fault;
use crate::gdma::DmaDesc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DmaDescriptorWord {
    Control,
    Buffer,
    Next,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DmaDescriptorFault {
    Read { descriptor: u32, word: DmaDescriptorWord, fault: Fault },
    BufferRead { descriptor: u32, address: u32, fault: Fault },
    Writeback { descriptor: u32, fault: Fault },
    NotOwned { descriptor: u32 },
    Cycle { descriptor: u32 },
    StepBudgetExceeded { budget: usize },
    PayloadTooShort { expected: usize, actual: usize },
}

/// Bound descriptor visits within one pump, including zero-progress rings.
pub struct DescriptorWalk { remaining: usize, budget: usize }
impl DescriptorWalk {
    pub fn new(budget: usize) -> Self { Self { remaining: budget, budget } }
    #[inline]
    pub fn remaining(&self) -> usize { self.remaining }
    #[inline]
    pub fn read(&mut self, read: impl FnOnce(u32) -> Result<(u32, DmaDesc), DmaDescriptorFault>, addr: u32) -> Result<(u32, DmaDesc), DmaDescriptorFault> {
        if self.remaining == 0 { return Err(DmaDescriptorFault::StepBudgetExceeded { budget: self.budget }); }
        self.remaining -= 1;
        read(addr)
    }
}

/// The caller supplies memory-only access where its engine forbids MMIO.
#[inline]
pub fn read_descriptor(mut read: impl FnMut(u32) -> Result<u32, Fault>, addr: u32) -> Result<(u32, DmaDesc), DmaDescriptorFault> {
    let mut word = |offset, word| read(addr.wrapping_add(offset))
        .map_err(|fault| DmaDescriptorFault::Read { descriptor: addr, word, fault });
    let dw0 = word(0, DmaDescriptorWord::Control)?;
    let (buf, next) = (word(4, DmaDescriptorWord::Buffer)?, word(8, DmaDescriptorWord::Next)?);
    Ok((dw0, DmaDesc { addr, size: dw0 & 0xfff, length: (dw0 >> 12) & 0xfff, eof: dw0 & (1 << 30) != 0, owner_dma: dw0 & (1 << 31) != 0, buf, next }))
}

/// Walk a finite descriptor transaction. Streaming rings keep using DescriptorWalk.
/// The visitor returns false once it has consumed the requested payload.
pub fn walk_chain<B>(
    bus: &mut B,
    mut desc: u32,
    budget: usize,
    mut read: impl FnMut(&mut B, u32) -> Result<(u32, DmaDesc), DmaDescriptorFault>,
    mut visit: impl FnMut(&mut B, u32, DmaDesc) -> Result<bool, DmaDescriptorFault>,
) -> Result<(), DmaDescriptorFault> {
    let mut seen = std::collections::HashSet::new();
    let mut walk = DescriptorWalk::new(budget);
    while desc != 0 {
        if !seen.insert(desc) { return Err(DmaDescriptorFault::Cycle { descriptor: desc }); }
        let (control, d) = walk.read(|addr| read(bus, addr), desc)?;
        let (next, eof) = (d.next, d.eof);
        if !visit(bus, control, d)? || eof { break; }
        desc = next;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finite_walk_rejects_cycles_and_exhausted_budgets() {
        let read = |_: &mut (), addr| Ok((0, DmaDesc {
            addr, size: 1, length: 1, eof: false, owner_dma: true, buf: 0, next: addr,
        }));
        assert_eq!(walk_chain(&mut (), 4, 2, read, |_, _, _| Ok(true)), Err(DmaDescriptorFault::Cycle { descriptor: 4 }));
        assert_eq!(walk_chain(&mut (), 4, 0, read, |_, _, _| Ok(true)), Err(DmaDescriptorFault::StepBudgetExceeded { budget: 0 }));
        assert_eq!(walk_chain(&mut (), 4, 1, read, |_, _, _| Ok(false)), Ok(()));
    }
}
