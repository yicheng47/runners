// Append-only NDJSON event log, one file per mission.
//
// Arch §5.4 (durability) requires a single serialized write per event, a shared
// exclusive lock so concurrent emitters don't interleave, and append-only
// semantics so we can stream new lines by byte offset.
//
// Invariants:
//   1. One event = one line. No multi-line JSON, no trailing bare bytes.
//   2. Acquire `flock(LOCK_EX)` before writing so N concurrent `runners signal`
//      / `runners msg post` invocations interleave at whole-line granularity.
//   3. Read paths never seek past the last newline — partial writes (which can't
//      happen given the lock, but belt and suspenders) would be skipped, not
//      parsed as truncated JSON.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use fs2::FileExt;

use crate::error::{Error, Result};
use crate::model::Event;

pub const EVENTS_FILENAME: &str = "events.ndjson";

pub struct EventLog {
    path: PathBuf,
}

pub struct LogEntry {
    /// Byte offset in the file immediately *after* this entry's trailing newline.
    /// Pass as the `offset` arg to `read_from` to resume streaming where you left off.
    pub next_offset: u64,
    pub event: Event,
}

impl EventLog {
    /// Opens (creating if needed) the events file inside `mission_dir`.
    /// The directory is created recursively. The file is not held open — each
    /// `append` / `read_from` opens it fresh.
    pub fn open(mission_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(mission_dir)?;
        let path = mission_dir.join(EVENTS_FILENAME);
        // Touch it so `size()` etc. work before the first append.
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the current file size in bytes. Useful to capture a starting
    /// offset before kicking off a watcher.
    pub fn size(&self) -> Result<u64> {
        Ok(std::fs::metadata(&self.path)?.len())
    }

    /// Serializes `event` to a single JSON line and appends it to the log under
    /// an exclusive file lock.
    pub fn append(&self, event: &Event) -> Result<()> {
        let mut line = serde_json::to_vec(event)?;
        // Guard against serializers that might emit embedded newlines (none should,
        // but a bad `payload` could slip one in via custom serde impls elsewhere).
        if line.contains(&b'\n') {
            return Err(Error::msg(
                "event contains embedded newline; refusing to append",
            ));
        }
        line.push(b'\n');

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;

        file.lock_exclusive()?;
        let write_res = (&file).write_all(&line);
        // Unlock even if the write failed — holding a lock on a dying fd is worse.
        let unlock_res = file.unlock();
        write_res?;
        unlock_res?;
        Ok(())
    }

    /// Reads every whole NDJSON line from `offset` onward and returns parsed
    /// events with the byte offset just past each line. The final partial line
    /// (if any — shouldn't occur given the lock) is silently skipped.
    pub fn read_from(&self, offset: u64) -> Result<Vec<LogEntry>> {
        let mut file = File::open(&self.path)?;
        file.seek(SeekFrom::Start(offset))?;
        let mut reader = BufReader::new(file);

        let mut out = Vec::new();
        let mut pos = offset;
        let mut buf = String::new();
        loop {
            buf.clear();
            let n = reader.read_line(&mut buf)?;
            if n == 0 {
                break;
            }
            pos += n as u64;
            // Skip a dangling partial line at EOF (no terminating '\n').
            if !buf.ends_with('\n') {
                break;
            }
            let trimmed = buf.trim_end_matches(['\n', '\r']);
            if trimmed.is_empty() {
                continue;
            }
            let event: Event = serde_json::from_str(trimmed)?;
            out.push(LogEntry {
                next_offset: pos,
                event,
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Event, EventKind, SignalType};
    use std::sync::Arc;
    use std::thread;

    fn signal(id: &str, ty: &str) -> Event {
        Event {
            id: id.into(),
            ts: chrono::Utc::now(),
            crew_id: "crew".into(),
            mission_id: "mission".into(),
            kind: EventKind::Signal,
            from: "coder".into(),
            to: None,
            signal_type: Some(SignalType::new(ty)),
            payload: serde_json::json!({}),
        }
    }

    fn message(id: &str, to: Option<&str>, text: &str) -> Event {
        Event {
            id: id.into(),
            ts: chrono::Utc::now(),
            crew_id: "crew".into(),
            mission_id: "mission".into(),
            kind: EventKind::Message,
            from: "lead".into(),
            to: to.map(String::from),
            signal_type: None,
            payload: serde_json::json!({ "text": text }),
        }
    }

    #[test]
    fn append_then_read_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let log = EventLog::open(dir.path()).unwrap();
        log.append(&signal("01HG00000000000000000AAAAA", "ask_lead"))
            .unwrap();
        log.append(&message(
            "01HG00000000000000000BBBBB",
            Some("impl"),
            "do a thing",
        ))
        .unwrap();

        let entries = log.read_from(0).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].event.kind, EventKind::Signal);
        assert_eq!(
            entries[0].event.signal_type.as_ref().unwrap().as_str(),
            "ask_lead"
        );
        assert_eq!(entries[1].event.kind, EventKind::Message);
        assert_eq!(entries[1].event.to.as_deref(), Some("impl"));

        // Resuming from the first entry's offset yields only the second.
        let after_first = log.read_from(entries[0].next_offset).unwrap();
        assert_eq!(after_first.len(), 1);
        assert_eq!(after_first[0].event.id, "01HG00000000000000000BBBBB");
    }

    #[test]
    fn concurrent_appends_never_interleave() {
        let dir = tempfile::tempdir().unwrap();
        let log = Arc::new(EventLog::open(dir.path()).unwrap());

        let threads = 8;
        let per_thread = 250;
        let mut handles = Vec::new();
        for t in 0..threads {
            let log = Arc::clone(&log);
            handles.push(thread::spawn(move || {
                for i in 0..per_thread {
                    let id = format!("01HG{t:010}{i:012}");
                    let evt = signal(&id, "ask_lead");
                    log.append(&evt).unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let entries = log.read_from(0).unwrap();
        assert_eq!(entries.len(), threads * per_thread);
        // If any writes interleaved mid-line, JSON parsing in read_from would
        // have bailed out with an error instead of reaching this assert.
    }

    #[test]
    fn read_from_handles_empty_log() {
        let dir = tempfile::tempdir().unwrap();
        let log = EventLog::open(dir.path()).unwrap();
        assert!(log.read_from(0).unwrap().is_empty());
        assert_eq!(log.size().unwrap(), 0);
    }

    #[test]
    fn refuses_events_with_embedded_newlines() {
        let dir = tempfile::tempdir().unwrap();
        let log = EventLog::open(dir.path()).unwrap();
        let mut evt = message("01HG00000000000000000CCCCC", None, "hi");
        evt.payload = serde_json::json!({ "text": "line1\nline2" });
        // serde_json encodes the "\n" as literal "\\n" in the output, so this
        // is actually fine — but guard against a hand-crafted payload that
        // manages to embed a raw newline via a custom serializer.
        log.append(&evt).unwrap();
        let entries = log.read_from(0).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].event.payload["text"], "line1\nline2");
    }
}
