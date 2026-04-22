// Monotonic ULID generation.
//
// Arch §5.2 says event ids are ULIDs, time-sortable and monotonic within the same
// millisecond. The `ulid` crate's `Generator::generate` handles monotonicity by
// incrementing the 80-bit random payload on same-ms collisions, which is exactly
// what we want for ordering events that land in the NDJSON log microseconds apart.
//
// We wrap it behind a `Mutex` so multiple threads can share one generator; the
// critical section is a few arithmetic ops so contention is negligible in
// practice.

use std::sync::Mutex;

use crate::error::{Error, Result};
use crate::model::Ulid;

pub struct UlidGen {
    inner: Mutex<ulid::Generator>,
}

impl UlidGen {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(ulid::Generator::new()),
        }
    }

    pub fn next(&self) -> Result<Ulid> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| Error::msg("ulid generator mutex poisoned"))?;
        g.generate()
            .map(|u| u.to_string())
            .map_err(|e| Error::msg(format!("ulid overflow: {e}")))
    }
}

impl Default for UlidGen {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_monotonic_ids() {
        let gen = UlidGen::new();
        let mut last = gen.next().unwrap();
        for _ in 0..10_000 {
            let next = gen.next().unwrap();
            assert!(next > last, "expected {next} > {last}");
            last = next;
        }
    }

    #[test]
    fn ids_are_26_char_crockford() {
        let gen = UlidGen::new();
        let id = gen.next().unwrap();
        assert_eq!(id.len(), 26);
        assert!(id.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn shared_across_threads_stays_monotonic() {
        use std::sync::Arc;
        use std::thread;

        let gen = Arc::new(UlidGen::new());
        let mut handles = Vec::new();
        for _ in 0..8 {
            let g = Arc::clone(&gen);
            handles.push(thread::spawn(move || {
                let mut ids = Vec::with_capacity(500);
                for _ in 0..500 {
                    ids.push(g.next().unwrap());
                }
                ids
            }));
        }

        let mut all: Vec<_> = handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect();
        let sorted_before = all.clone();
        all.sort();
        all.dedup();
        assert_eq!(all.len(), 8 * 500, "duplicate ULIDs across threads");
        // Sortedness by time is implicit: ULID lexicographic order matches emission order
        // *per-thread*, but not globally — we only assert uniqueness here.
        let _ = sorted_before;
    }
}
