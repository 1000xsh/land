//! shared state between MPSC producers and consumer.

use crate::ringbuffer::RingBuffer;
use land_cpu::{CachePadded, Cursor, fence_acquire, fence_release};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

/// number of bits per availability word (u64 = 64 bits).
pub(super) const BITS_PER_WORD: usize = 64;

/// shared state between producers and consumer.
pub(super) struct Shared<T> {
    /// the ring buffer storing events.
    pub(super) buffer: RingBuffer<T>,
    /// claim cursor - next sequence to be claimed by a producer.
    pub(super) claim_cursor: Cursor,
    /// consumer's processed sequence (what's been consumed).
    pub(super) consumer_cursor: Cursor,
    /// availability bitmap tracking which sequences have been published.
    /// sized to match capacity: one bit per slot in the ring buffer.
    /// consumer scans this directly to find available sequences.
    pub(super) availability: Box<[CachePadded<AtomicU64>]>,
    /// number of words in the availability bitmap.
    pub(super) availability_words: usize,
    /// number of active producers.
    pub(super) producer_count: CachePadded<AtomicUsize>,
    /// channel closed flag.
    pub(super) closed: CachePadded<AtomicBool>,
    /// buffer capacity (power of 2).
    pub(super) capacity: usize,
}

impl<T> Drop for Shared<T> {
    fn drop(&mut self) {
        // drop any unconsumed events to prevent memory leaks for types with Drop.
        // scan the bitmap to find published but unconsumed sequences.
        let consumer_seq = self.consumer_cursor.value();
        let claim_seq = self.claim_cursor.value();

        // only drop if there might be unconsumed events
        if claim_seq > consumer_seq {
            for seq in (consumer_seq + 1)..=claim_seq {
                // only drop if this sequence was actually published
                if self.is_published(seq) {
                    // safety: this slot was written and published but not yet consumed.
                    // we have exclusive access since we are in Drop.
                    unsafe {
                        let ptr = self.buffer.get_ptr_mut(seq);
                        core::ptr::drop_in_place(ptr);
                    }
                }
            }
        }
    }
}

impl<T> Shared<T> {
    /// calculate word index for a sequence.
    #[inline]
    pub(super) fn word_index(&self, sequence: i64) -> usize {
        ((sequence as usize) / BITS_PER_WORD) % self.availability_words
    }

    /// calculate bit index within a word for a sequence.
    #[inline]
    pub(super) fn bit_index(&self, sequence: i64) -> usize {
        (sequence as usize) % BITS_PER_WORD
    }

    /// check if a sequence is published.
    /// uses Relaxed load, caller must fence before reading data.
    #[inline]
    pub(super) fn is_published(&self, sequence: i64) -> bool {
        let word_idx = self.word_index(sequence);
        let bit_idx = self.bit_index(sequence);
        // use Relaxed - fence_acquire() called after this returns true
        let word = self.availability[word_idx].load(Ordering::Relaxed);
        let expected_bit = ((sequence as usize) / self.capacity) & 1;
        let actual_bit = ((word >> bit_idx) & 1) as usize;
        actual_bit == expected_bit
    }

    /// mark a sequence as published.
    #[inline]
    pub(super) fn mark_published(&self, sequence: i64) {
        let word_idx = self.word_index(sequence);
        let bit_idx = self.bit_index(sequence);
        let mask = 1u64 << bit_idx;

        // architecture-aware fence + Relaxed for better performance on x86
        // on x86 TSO: fence_release() is just a compiler fence (~0 cycles)
        // on ARM: fence_release() is a real fence (required for correctness)
        fence_release();
        self.availability[word_idx].fetch_xor(mask, Ordering::Relaxed);
    }

    /// mark multiple consecutive sequences as published using batched XOR.
    ///
    /// this is more efficient than multiple `mark_published` calls because
    /// it accumulates bits for the same word and uses a single atomic XOR.
    ///
    /// # note
    ///
    /// uses Relaxed ordering because callers must issue a `fence(Release)` before
    /// calling this method to ensure data visibility. this saves ~5-10 cycles
    /// per atomic operation compared to using Release ordering.
    #[inline]
    pub(super) fn mark_published_batch(&self, start: i64, count: usize) {
        if count == 0 {
            return;
        }

        let mut flip_mask = 0u64;
        let mut current_word_idx = self.word_index(start);

        for i in 0..count {
            let seq = start + i as i64;
            let word_idx = self.word_index(seq);
            let bit_idx = self.bit_index(seq);

            // if we are moving to a different word, flush accumulated bits
            if word_idx != current_word_idx {
                if flip_mask != 0 {
                    // relaxed is safe: caller issued fence(Release) before this call
                    self.availability[current_word_idx].fetch_xor(flip_mask, Ordering::Relaxed);
                }
                flip_mask = 0;
                current_word_idx = word_idx;
            }

            flip_mask |= 1u64 << bit_idx;
        }

        // flush remaining bits
        if flip_mask != 0 {
            // relaxed is safe: caller issued fence(Release) before this call
            self.availability[current_word_idx].fetch_xor(flip_mask, Ordering::Relaxed);
        }
    }

    /// calculate the expected bit pattern for a sequence's round.
    ///
    /// returns 0 for even rounds, !0 (all 1s) for odd rounds.
    /// round is based on capacity (ring buffer wrap), not bitmap size.
    #[inline]
    fn expected_pattern(&self, sequence: i64) -> u64 {
        let round = (sequence as usize) / self.capacity;
        if round & 1 == 0 { 0 } else { !0u64 }
    }

    /// find the highest contiguously published sequence from a starting point.
    ///
    /// uses word-level bit scanning for efficiency - processes up to 64 sequences
    /// per iteration instead of one at a time.
    ///
    /// fixme: uses relaxed loads for scanning, single acquire fence at end.
    /// reduces O(words) acquire operations to O(1).
    pub(super) fn highest_published(&self, from: i64) -> i64 {
        let claim = self.claim_cursor.value_relaxed();
        if from > claim {
            return from - 1; // nothing claimed yet
        }

        let mut seq = from;
        let mut word_idx = self.word_index(seq);
        let mut bit_idx = self.bit_index(seq);

        let result = loop {
            if seq > claim {
                break seq - 1;
            }

            // relaxed for scanning, fence after loop
            let word = self.availability[word_idx].load(Ordering::Relaxed);
            let expected = self.expected_pattern(seq);

            // XOR with expected: published bits become 0, unpublished become 1
            let mismatches = word ^ expected;

            // mask off bits before our starting position in this word
            let mask = !0u64 << bit_idx;
            let relevant_mismatches = mismatches & mask;

            if relevant_mismatches != 0 {
                // found an unpublished sequence - use trailing_zeros to find it
                let first_mismatch_bit = relevant_mismatches.trailing_zeros() as usize;
                let advance = first_mismatch_bit - bit_idx;
                break seq + advance as i64 - 1;
            }

            // all bits in this word (from bit_idx onwards) are published
            // advance to the next word
            let bits_checked = BITS_PER_WORD - bit_idx;
            seq += bits_checked as i64;
            word_idx = (word_idx + 1) % self.availability_words;
            bit_idx = 0;
        };

        // single acquire fence ensures we see all buffer writes for published sequences
        fence_acquire();
        result
    }
}
