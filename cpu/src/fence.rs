// memory fence operations
//
// notes:
// - fences constrain reordering and participate in happens-before rules
// - on x86/x86_64, acquire/release/acqrel fences compile to no cpu barrier (but still compiler fence)
// - fence synchronization: release fence A syncs with acquire fence B if atomic ops X and Y exist
//   on same object where A is sequenced-before X, Y is sequenced-before B, and Y observes X

use core::sync::atomic::{fence, Ordering};

// acquire fence - prevents subsequent ops from reordering before fence
#[inline(always)]
pub fn fence_acquire() {
    fence(Ordering::Acquire);
}

// release fence - prevents prior ops from reordering after fence
#[inline(always)]
pub fn fence_release() {
    fence(Ordering::Release);
}

// acquire-release fence - prevents reordering in both directions
#[inline(always)]
pub fn fence_acq_rel() {
    fence(Ordering::AcqRel);
}

// cpu spin-loop hint - use inside tight spin-wait loops
// not a memory fence, reduces power and improves smt/ht friendliness
// maps to PAUSE on x86/x86_64
#[inline(always)]
pub fn cpu_pause() {
    core::hint::spin_loop();
}
