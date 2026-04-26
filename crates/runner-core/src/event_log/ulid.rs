// Monotonic ULID generation, safe across processes when rebased from disk.
//
// The in-process mutex here gives thread-safe monotonicity. But arch §5.2 also
// demands ordering-by-ID across *processes* — two concurrent `runner signal`
// invocations, each with its own `UlidGen`, must not emit ULIDs out-of-order
// relative to the file append order, or watermark-by-max-ULID (§5.5.1) silently
// drops events.
//
// Cross-process monotonicity is handled by `EventLog::append`, which holds an
// exclusive `flock` during both ID assignment and the write. Before generating
// an ID it calls `raise_floor` with the largest ULID currently on disk, which
// bumps this generator's internal "last" value. That guarantees the next ID is
// strictly greater than anything already committed, regardless of which process
// wrote it.

use std::sync::Mutex;
use std::time::SystemTime;

use crate::error::{Error, Result};
use crate::model::Ulid as UlidString;

pub struct UlidGen {
    inner: Mutex<u128>,
}

impl UlidGen {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(0),
        }
    }

    /// Ensure the next generated ULID is strictly greater than `floor`.
    /// Safe to call repeatedly; only advances the internal cursor.
    pub fn raise_floor(&self, floor: u128) -> Result<()> {
        let mut last = self
            .inner
            .lock()
            .map_err(|_| Error::msg("ulid generator mutex poisoned"))?;
        if floor > *last {
            *last = floor;
        }
        Ok(())
    }

    pub fn raise_floor_from_str(&self, floor: &str) -> Result<()> {
        let u: ulid::Ulid = floor
            .parse()
            .map_err(|e| Error::msg(format!("invalid ulid floor {floor:?}: {e}")))?;
        self.raise_floor(u128::from(u))
    }

    /// Generate a ULID that is:
    ///   - at least as recent as the wall clock, and
    ///   - strictly greater than every previously-observed ULID (this generator's
    ///     own history *and* any floor raised via `raise_floor`).
    pub fn next(&self) -> Result<UlidString> {
        let mut last = self
            .inner
            .lock()
            .map_err(|_| Error::msg("ulid generator mutex poisoned"))?;

        let now = ulid::Ulid::from_datetime(SystemTime::now());
        let now_u: u128 = now.into();

        let candidate = if now_u > *last {
            now_u
        } else {
            // Same ms (or clock went backward): bump the random portion of the
            // previous ULID. `Ulid::increment()` handles overflow to the next ms.
            let prev = ulid::Ulid(*last);
            prev.increment()
                .ok_or_else(|| Error::msg("ulid random portion exhausted"))?
                .into()
        };

        *last = candidate;
        Ok(ulid::Ulid(candidate).to_string())
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
    fn raise_floor_forces_strict_greater() {
        let gen = UlidGen::new();
        let base = gen.next().unwrap();
        // Pretend another process wrote something far in the future.
        let future_u: u128 =
            ulid::Ulid::from_datetime(SystemTime::now() + std::time::Duration::from_secs(3600))
                .into();
        gen.raise_floor(future_u).unwrap();
        let after = gen.next().unwrap();
        assert!(
            after > base,
            "next() must return > prior output; got {after} vs {base}"
        );
        let after_u: u128 = ulid::Ulid::from_string(&after).unwrap().into();
        assert!(
            after_u > future_u,
            "next() must return > floor; got {after_u:x} vs {future_u:x}"
        );
    }

    #[test]
    fn raise_floor_from_str_parses_crockford() {
        let gen = UlidGen::new();
        // All-zero ULID: floor is minimum, should be a no-op relative to now.
        gen.raise_floor_from_str("00000000000000000000000000")
            .unwrap();
        let id = gen.next().unwrap();
        assert_ne!(id, "00000000000000000000000000");
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
        all.sort();
        all.dedup();
        assert_eq!(all.len(), 8 * 500, "duplicate ULIDs across threads");
    }
}
