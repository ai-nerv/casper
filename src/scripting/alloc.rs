//! The counters monty's `max_memory` reads, fed in-process.
//!
//! monty checks `LIVE_MEMORY - BASELINE_MEMORY` at its checkpoints and leaves feeding them to the
//! host. This allocator charges what a thread holding a [`SandboxScope`] allocates, and refunds
//! what it frees, so a breach is a Python `MemoryError` and never the end of casper. Outside a
//! scope an allocation costs one thread-local read.
//!
//! The counters are process-wide: overlapping runs share one budget, and only the first scope
//! rebases it. Tool results are made on other threads and freed inside the scope, so the count is
//! a guardrail against runaway scripts rather than an exact measure.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::Relaxed;

use monty_types::{BASELINE_MEMORY, LIVE_MEMORY};

#[global_allocator]
static ALLOC: Counting = Counting;

thread_local! {
    static IN_SANDBOX: Cell<bool> = const { Cell::new(false) };
}

struct Counting;

fn in_sandbox() -> bool {
    IN_SANDBOX.try_with(Cell::get).unwrap_or(false)
}

fn charge(size: usize) {
    if in_sandbox() {
        LIVE_MEMORY.fetch_add(size, Relaxed);
    }
}

/// Saturating: a block made outside the scope may be freed inside it.
fn refund(size: usize) {
    if in_sandbox() {
        let _ = LIVE_MEMORY.fetch_update(Relaxed, Relaxed, |v| Some(v.saturating_sub(size)));
    }
}

// SAFETY: every method hands its arguments to `System` unchanged and returns what `System`
// returned. The counting beside it touches no pointer, so this upholds exactly what `System`
// upholds.
#[allow(unsafe_code)]
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        charge(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        charge(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        refund(layout.size());
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        match new_size.checked_sub(layout.size()) {
            Some(grown) => charge(grown),
            None => refund(layout.size() - new_size),
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

static ACTIVE_SCOPES: AtomicUsize = AtomicUsize::new(0);

/// This thread's allocations are the interpreter's while the scope lives.
pub(super) struct SandboxScope;

impl SandboxScope {
    pub(super) fn enter() -> Self {
        if ACTIVE_SCOPES.fetch_add(1, Relaxed) == 0 {
            BASELINE_MEMORY.store(LIVE_MEMORY.load(Relaxed), Relaxed);
        }
        IN_SANDBOX.set(true);
        Self
    }
}

impl Drop for SandboxScope {
    fn drop(&mut self) {
        IN_SANDBOX.set(false);
        ACTIVE_SCOPES.fetch_sub(1, Relaxed);
    }
}
