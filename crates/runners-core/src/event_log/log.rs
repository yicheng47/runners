// Append-only NDJSON event log, one file per mission.
//
// Arch §5.4 (durability) requires:
//   - A single serialized write per event line.
//   - An exclusive lock covering both ID assignment and the write, so concurrent
//     processes (the app, multiple `runners` CLI invocations) interleave at
//     whole-line granularity and produce strictly monotonic ULIDs.
//   - Append-only semantics so the watcher can stream new lines by byte offset.
//
// Failure modes and what we do about them:
//
//   a) Two processes race to emit. The `flock` serializes them; before each
//      write we scan the file's tail to learn the current max ULID and raise
//      our generator's floor so our new ID is strictly greater. Without this,
//      a newer-clock process + older-clock process can emit out-of-order IDs
//      relative to the write order, breaking watermark-by-max-ULID replay.
//
//   b) `write_all` performs a partial write before erroring. We capture the
//      pre-write file length under the lock and truncate back to it if the
//      write returns early, so the log never contains a headless fragment
//      that the next append would glue to a new line.
//
//   c) Event serialization emits an embedded newline. We refuse to append,
//      rather than let a malformed line poison the log (a custom serde impl
//      somewhere upstream could conceivably do this).

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use chrono::Utc;
use fs2::FileExt;

use super::ulid::UlidGen;
use crate::error::{Error, Result};
use crate::model::{Event, EventDraft};

pub use super::path::EVENTS_FILENAME;

pub struct EventLog {
    path: PathBuf,
    ulid: UlidGen,
}

pub struct LogEntry {
    /// Byte offset in the file immediately *after* this entry's trailing newline.
    /// Pass as the `offset` arg to `read_from` to resume streaming where you left off.
    pub next_offset: u64,
    pub event: Event,
}

impl EventLog {
    /// Opens (creating if needed) the events file inside `mission_dir`.
    /// The directory is created recursively.
    pub fn open(mission_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(mission_dir)?;
        let path = mission_dir.join(EVENTS_FILENAME);
        // Touch so `size()` works before the first append.
        OpenOptions::new().create(true).append(true).open(&path)?;
        let this = Self {
            path,
            ulid: UlidGen::new(),
        };
        // Seed the generator from disk so restarts don't forget prior runs.
        if let Some(last_id) = this.last_id_on_disk()? {
            this.ulid.raise_floor_from_str(&last_id)?;
        }
        Ok(this)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the current file size in bytes. Useful to capture a starting
    /// offset before kicking off a watcher.
    pub fn size(&self) -> Result<u64> {
        Ok(std::fs::metadata(&self.path)?.len())
    }

    /// Builds an `Event` from `draft`, assigns `id` and `ts` under an exclusive
    /// file lock, appends a single NDJSON line, and returns the committed event.
    ///
    /// The `id` is generated *inside* the lock, after rebasing the generator's
    /// floor against the largest ULID currently on disk. This is what makes
    /// concurrent CLI processes safe: whichever of them gets the lock first
    /// also wins the lower ULID.
    pub fn append(&self, draft: EventDraft) -> Result<Event> {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&self.path)?;

        file.lock_exclusive()?;
        let result = self.append_locked(&file, draft);
        let unlock_res = file.unlock();
        let event = result?;
        unlock_res?;
        Ok(event)
    }

    fn append_locked(&self, file: &File, draft: EventDraft) -> Result<Event> {
        // Rebase floor from on-disk tail so any writer that committed while we
        // were waiting for the lock doesn't leave us with an older ULID.
        if let Some(last_id) = Self::last_id_in_file(file)? {
            self.ulid.raise_floor_from_str(&last_id)?;
        }

        let id = self.ulid.next()?;
        let ts = Utc::now();
        let event = Event {
            id,
            ts,
            crew_id: draft.crew_id,
            mission_id: draft.mission_id,
            kind: draft.kind,
            from: draft.from,
            to: draft.to,
            signal_type: draft.signal_type,
            payload: draft.payload,
        };

        let mut line = serde_json::to_vec(&event)?;
        if line.contains(&b'\n') {
            return Err(Error::msg(
                "event contains embedded newline; refusing to append",
            ));
        }
        line.push(b'\n');

        let pre_len = file.metadata()?.len();
        let write_res = (&*file).write_all(&line);
        if let Err(e) = write_res {
            // Partial-write rollback: truncate back to what we saw before the
            // write started. We still hold the lock, so no one else has grown
            // the file in the meantime.
            let _ = file.set_len(pre_len);
            return Err(e.into());
        }
        Ok(event)
    }

    /// Reads every whole NDJSON line from `offset` onward and returns parsed
    /// events with the byte offset just past each line. A dangling partial line
    /// at EOF (which shouldn't happen given the lock + rollback, but guard
    /// anyway) is silently skipped.
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

    fn last_id_on_disk(&self) -> Result<Option<String>> {
        let file = File::open(&self.path)?;
        Self::last_id_in_file(&file)
    }

    /// Reads the `id` field of the last complete JSON line in the file without
    /// loading the whole log. Scans backward from EOF for the preceding newline,
    /// then parses just that line.
    fn last_id_in_file(file: &File) -> Result<Option<String>> {
        let len = file.metadata()?.len();
        if len == 0 {
            return Ok(None);
        }

        // Walk back from EOF in small chunks looking for the *second-to-last*
        // '\n' (the boundary before the final line). For typical event sizes
        // (< 4 KB) a single 4 KB read is enough.
        let chunk_size: u64 = 4096;
        let mut end = len;
        let mut line_bytes: Vec<u8> = Vec::new();

        while end > 0 {
            let start = end.saturating_sub(chunk_size);
            let span = (end - start) as usize;
            let mut buf = vec![0u8; span];
            let mut f = file;
            f.seek(SeekFrom::Start(start))?;
            let mut read = 0;
            while read < span {
                let n = std::io::Read::read(&mut f, &mut buf[read..])?;
                if n == 0 {
                    break;
                }
                read += n;
            }
            buf.truncate(read);

            // Combine with any bytes we already stitched from a later chunk.
            buf.extend_from_slice(&line_bytes);
            line_bytes = buf;

            // Drop a trailing newline that marks the end of the last line.
            if line_bytes.last() == Some(&b'\n') {
                line_bytes.pop();
            }

            // Find the newline that precedes the final line. If present, that's
            // the boundary; everything after it is the last line.
            if let Some(pos) = line_bytes.iter().rposition(|&b| b == b'\n') {
                let last = &line_bytes[pos + 1..];
                return parse_id(last).map(Some);
            }
            // Otherwise scan further back.
            if start == 0 {
                // Entire file is one line.
                return parse_id(&line_bytes).map(Some);
            }
            end = start;
        }
        Ok(None)
    }
}

fn parse_id(line: &[u8]) -> Result<String> {
    if line.is_empty() {
        return Err(Error::msg("empty line at end of log"));
    }
    // Minimal parse: we only need the `id` field. `serde_json::from_slice`
    // over the whole envelope is fine — the line is at most a few KB.
    #[derive(serde::Deserialize)]
    struct Tail {
        id: String,
    }
    let t: Tail = serde_json::from_slice(line)?;
    Ok(t.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EventDraft, EventKind, SignalType};
    use std::sync::Arc;
    use std::thread;

    fn draft_signal(ty: &str) -> EventDraft {
        EventDraft {
            crew_id: "crew".into(),
            mission_id: "mission".into(),
            kind: EventKind::Signal,
            from: "coder".into(),
            to: None,
            signal_type: Some(SignalType::new(ty)),
            payload: serde_json::json!({}),
        }
    }

    fn draft_message(to: Option<&str>, text: &str) -> EventDraft {
        EventDraft {
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
        let a = log.append(draft_signal("ask_lead")).unwrap();
        let b = log
            .append(draft_message(Some("impl"), "do a thing"))
            .unwrap();

        let entries = log.read_from(0).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].event.id, a.id);
        assert_eq!(entries[1].event.id, b.id);
        assert_eq!(entries[1].event.kind, EventKind::Message);
        assert_eq!(entries[1].event.to.as_deref(), Some("impl"));

        // Resuming from the first entry's offset yields only the second.
        let after_first = log.read_from(entries[0].next_offset).unwrap();
        assert_eq!(after_first.len(), 1);
        assert_eq!(after_first[0].event.id, b.id);
    }

    #[test]
    fn concurrent_appends_never_interleave_and_stay_monotonic() {
        let dir = tempfile::tempdir().unwrap();
        let log = Arc::new(EventLog::open(dir.path()).unwrap());

        let threads = 8;
        let per_thread = 250;
        let mut handles = Vec::new();
        for _ in 0..threads {
            let log = Arc::clone(&log);
            handles.push(thread::spawn(move || {
                for _ in 0..per_thread {
                    log.append(draft_signal("ask_lead")).unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let entries = log.read_from(0).unwrap();
        assert_eq!(entries.len(), threads * per_thread);

        // ULIDs must appear in strictly ascending order in the file.
        let mut last = String::new();
        for e in &entries {
            assert!(e.event.id > last, "event {:?} not > {:?}", e.event.id, last);
            last.clone_from(&e.event.id);
        }
    }

    #[test]
    fn reopen_picks_up_floor_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let mid_id;
        {
            let log = EventLog::open(dir.path()).unwrap();
            log.append(draft_signal("ask_lead")).unwrap();
            mid_id = log.append(draft_signal("ask_lead")).unwrap().id;
        }
        // New process, fresh generator — must still produce ids > mid_id.
        let log = EventLog::open(dir.path()).unwrap();
        let next = log.append(draft_signal("ask_lead")).unwrap();
        assert!(
            next.id > mid_id,
            "next id {} not > prior-run id {}",
            next.id,
            mid_id
        );
    }

    #[test]
    fn read_from_handles_empty_log() {
        let dir = tempfile::tempdir().unwrap();
        let log = EventLog::open(dir.path()).unwrap();
        assert!(log.read_from(0).unwrap().is_empty());
    }

    #[test]
    fn payload_strings_with_newlines_are_escaped_not_literal() {
        let dir = tempfile::tempdir().unwrap();
        let log = EventLog::open(dir.path()).unwrap();
        let mut d = draft_message(None, "");
        d.payload = serde_json::json!({ "text": "line1\nline2" });
        log.append(d).unwrap();
        let entries = log.read_from(0).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].event.payload["text"], "line1\nline2");
    }

    #[test]
    fn last_id_scans_beyond_4kb_chunk() {
        let dir = tempfile::tempdir().unwrap();
        let log = EventLog::open(dir.path()).unwrap();
        // Write a line that guarantees the tail scan straddles the 4 KB window.
        let big = "x".repeat(10_000);
        let mut d = draft_message(None, "");
        d.payload = serde_json::json!({ "text": big });
        let wrote = log.append(d).unwrap();
        assert_eq!(
            EventLog::last_id_in_file(&File::open(log.path()).unwrap())
                .unwrap()
                .unwrap(),
            wrote.id
        );
    }
}
