#![allow(clippy::integer_arithmetic)]

use {
    solana_program_runtime::invoke_context::{Alloc, AllocErr},
    std::alloc::Layout,
};

#[derive(Debug)]
pub struct BpfAllocator {
    #[allow(dead_code)]
    start: u64,
    len: u64,
    pos: u64,
}

impl BpfAllocator {
    pub fn new(len: u64, virtual_address: u64) -> Self {
        Self {
            start: virtual_address,
            len,
            pos: 0,
        }
    }
}

impl Alloc for BpfAllocator {
    fn alloc(&mut self, layout: Layout) -> Result<u64, AllocErr> {
        let bytes_to_align = (self.pos as *const u8).align_offset(layout.align()) as u64;
        if self
            .pos
            .saturating_add(layout.size() as u64)
            .saturating_add(bytes_to_align)
            <= self.len
        {
            self.pos += bytes_to_align;
            let addr = self.start + self.pos;
            self.pos += layout.size() as u64;
            Ok(addr)
        } else {
            Err(AllocErr)
        }
    }

    fn dealloc(&mut self, _addr: u64, _layout: Layout) {
        // It's a bump allocator, free not supported
    }
}
