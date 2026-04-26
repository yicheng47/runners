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

/// One malformed line surfaced by `read_from_lossy`. The C7 watcher logs
/// these and skips past them so a single bad line can't freeze the bus.
#[derive(Debug, Clone)]
pub struct SkipReport {
    /// Byte offset where the skipped line started.
    pub offset: u64,
    /// Byte offset just past the skipped line's terminating newline. Pass
    /// as the next `offset` to resume after the corruption.
    pub next_offset: u64,
    pub error: String,
}

impl EventLog {
    /// Opens (creating if needed) the events file inside `mission_dir`.
    /// The directory is created recursively.
    ///
    /// If the previous process died mid-write, the file may end with a
    /// non-newline tail (bytes of a partial JSON event). We take the flock,
    /// truncate that tail off, then seed the ULID generator from the last
    /// *complete* line. This ensures a crash can't glue a stale half-event
    /// onto the next append.
    pub fn open(mission_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(mission_dir)?;
        let path = mission_dir.join(EVENTS_FILENAME);

        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&path)?;

        file.lock_exclusive()?;
        let repair_then_read = || -> Result<Option<String>> {
            Self::repair_tail(&file)?;
            Self::last_id_in_file(&file)
        };
        let last_id = repair_then_read();
        let unlock_res = file.unlock();
        let last_id = last_id?;
        unlock_res?;

        let this = Self {
            path,
            ulid: UlidGen::new(),
        };
        if let Some(id) = last_id {
            this.ulid.raise_floor_from_str(&id)?;
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
        // Repair any dangling non-newline tail from a prior crash *before*
        // reading the last id — otherwise `last_id_in_file` would treat the
        // partial fragment as the last event and we'd both seed a bogus floor
        // and glue the new event onto it.
        Self::repair_tail(file)?;
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

    /// Like `read_from`, but skips lines that fail to parse instead of
    /// aborting. Returns successfully-parsed entries and a separate vec of
    /// skip reports (offset + reason) so the caller can log a warning. Used
    /// by the C7 watcher: a single corrupted line — say from a buggy CLI
    /// release someone shipped — must not poison the bus and freeze every
    /// downstream subscriber on the same offset forever.
    ///
    /// IO errors (file truncated, read failed) still propagate — those are
    /// not per-line corruption and re-trying makes sense.
    pub fn read_from_lossy(&self, offset: u64) -> Result<(Vec<LogEntry>, Vec<SkipReport>)> {
        let mut file = File::open(&self.path)?;
        file.seek(SeekFrom::Start(offset))?;
        let mut reader = BufReader::new(file);

        let mut out = Vec::new();
        let mut skipped = Vec::new();
        let mut pos = offset;
        let mut buf: Vec<u8> = Vec::new();
        loop {
            let line_start = pos;
            buf.clear();
            // Use byte-level reads. `read_line` would Err on the first
            // non-UTF-8 byte and that error would propagate out of `tick`,
            // freezing the bus on the same offset forever. NDJSON expects
            // UTF-8, but a buggy writer that emits raw bytes must surface
            // as a SkipReport — not as a poison pill that hides every
            // event after it.
            let n = reader.read_until(b'\n', &mut buf)?;
            if n == 0 {
                break;
            }
            pos += n as u64;
            // A line without a trailing '\n' is incomplete (writer crashed
            // or torn write). Don't advance past it — the next tick should
            // re-attempt once the writer finishes the line.
            if buf.last() != Some(&b'\n') {
                break;
            }
            // Trim the terminator (and an optional preceding '\r') without
            // requiring valid UTF-8.
            let mut end = buf.len() - 1;
            if end > 0 && buf[end - 1] == b'\r' {
                end -= 1;
            }
            let line = &buf[..end];
            if line.is_empty() {
                continue;
            }
            match serde_json::from_slice::<Event>(line) {
                Ok(event) => {
                    // Validate the id is a real ULID — `Event.id` is just a
                    // String at the type level, so serde will happily accept
                    // `"id":"zzzz"`. Letting it through would corrupt the
                    // bus's lex-sorted inbox state: a junk id like "zzzz"
                    // sorts after every real ULID, so a later legitimate
                    // event would silently look "older" and get treated as
                    // already-read by `up_to` watermarks.
                    if event.id.parse::<ulid::Ulid>().is_err() {
                        skipped.push(SkipReport {
                            offset: line_start,
                            next_offset: pos,
                            error: format!("event id {:?} is not a valid ULID", event.id),
                        });
                        continue;
                    }
                    out.push(LogEntry {
                        next_offset: pos,
                        event,
                    });
                }
                Err(e) => {
                    skipped.push(SkipReport {
                        offset: line_start,
                        next_offset: pos,
                        error: e.to_string(),
                    });
                }
            }
        }
        Ok((out, skipped))
    }

    /// Truncate off any non-newline-terminated bytes at the end of the file.
    ///
    /// A well-behaved writer always finishes with `\n`. A crashed writer can
    /// leave JSON bytes without their trailing newline — at which point
    /// `last_id_in_file` would misread those bytes as the last event, and the
    /// next `append` would glue its line onto them and produce malformed
    /// NDJSON that breaks replay.
    ///
    /// Caller must hold the exclusive flock on the file.
    fn repair_tail(file: &File) -> Result<()> {
        let len = file.metadata()?.len();
        if len == 0 {
            return Ok(());
        }
        // Cheap check: is the last byte a newline?
        let mut last_byte = [0u8; 1];
        {
            let mut f = file;
            f.seek(SeekFrom::Start(len - 1))?;
            let n = std::io::Read::read(&mut f, &mut last_byte)?;
            if n == 1 && last_byte[0] == b'\n' {
                return Ok(());
            }
        }
        // Find the highest position of `\n`. Truncate to `pos + 1` so we keep
        // every complete line and drop the dangling fragment. If no `\n`
        // exists, the entire file is one incomplete line — truncate to 0.
        let chunk_size: u64 = 4096;
        let mut end = len;
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
            if let Some(pos) = buf.iter().rposition(|&b| b == b'\n') {
                // Offset of that `\n` in the whole file is `start + pos`.
                let keep = start + pos as u64 + 1;
                file.set_len(keep)?;
                return Ok(());
            }
            if start == 0 {
                break;
            }
            end = start;
        }
        // Whole file was one unterminated fragment.
        file.set_len(0)?;
        Ok(())
    }

    /// Reads the `id` field of the most recent JSON line that parses
    /// cleanly. Skips malformed lines by walking further back so a single
    /// corrupt line doesn't poison the floor (which would block every
    /// subsequent `append`) — the C7 watcher's "bad line doesn't poison
    /// the bus" contract has to extend to the writer side too, otherwise
    /// open + append both fail and the bus has nothing to read.
    fn last_id_in_file(file: &File) -> Result<Option<String>> {
        let len = file.metadata()?.len();
        if len == 0 {
            return Ok(None);
        }

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

            buf.extend_from_slice(&line_bytes);
            line_bytes = buf;

            if line_bytes.last() == Some(&b'\n') {
                line_bytes.pop();
            }

            // Try parsing successively-earlier lines from the bytes we've
            // accumulated so far. If one parses, that's our floor. If none
            // do, fetch another chunk and retry — a malformed line at the
            // tail must not fail-fast the way it used to.
            loop {
                let candidate_start = match line_bytes.iter().rposition(|&b| b == b'\n') {
                    Some(pos) => pos + 1,
                    None if start == 0 => 0,
                    None => break, // need more bytes from earlier in the file
                };
                let candidate = &line_bytes[candidate_start..];
                if let Ok(id) = parse_id(candidate) {
                    return Ok(Some(id));
                }
                if candidate_start == 0 {
                    // We've exhausted this buffer's lines and we already
                    // reached the file head. No valid id anywhere.
                    return Ok(None);
                }
                // Drop the bad candidate (and the newline before it) and
                // retry against the line that precedes it.
                line_bytes.truncate(candidate_start - 1);
            }

            if start == 0 {
                return Ok(None);
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
    // Validate ULID shape, not just "string-typed `id` field exists". Without
    // this, a line like `{"id":"not-a-ulid"}` parses cleanly here but blows
    // up later when `EventLog::open` calls `raise_floor_from_str` — which
    // would prevent the bus from ever mounting on a corrupted log. Treat
    // shape failure the same as a JSON parse failure so `last_id_in_file`
    // walks back to the last *valid* line instead of bailing out.
    if t.id.parse::<ulid::Ulid>().is_err() {
        return Err(Error::msg(format!("id {:?} is not a valid ULID", t.id)));
    }
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
    fn open_truncates_dangling_tail_from_crashed_writer() {
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let committed = {
            let log = EventLog::open(dir.path()).unwrap();
            log.append(draft_signal("ask_lead")).unwrap().id
        };
        // Simulate a crashed writer: append some bytes of an event with no
        // trailing '\n'. In a real crash this would be a partial JSON line.
        let events_path = dir.path().join(EVENTS_FILENAME);
        {
            let mut f = OpenOptions::new().append(true).open(&events_path).unwrap();
            f.write_all(b"{\"id\":\"01CRASHED").unwrap();
        }

        // Open must cope: truncate the fragment and recover the last complete id.
        let log = EventLog::open(dir.path()).unwrap();
        let entries = log.read_from(0).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].event.id, committed);

        // The next append must not glue its line onto the fragment.
        let next = log.append(draft_signal("ask_lead")).unwrap();
        let entries = log.read_from(0).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].event.id, next.id);
    }

    #[test]
    fn open_handles_file_that_is_only_a_fragment() {
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let events_path = dir.path().join(EVENTS_FILENAME);
        {
            let mut f = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&events_path)
                .unwrap();
            f.write_all(b"{\"id\":\"not-terminated").unwrap();
        }

        // File is entirely an unterminated fragment — open must truncate to 0
        // and still work.
        let log = EventLog::open(dir.path()).unwrap();
        assert_eq!(log.size().unwrap(), 0);
        let evt = log.append(draft_signal("ask_lead")).unwrap();
        let entries = log.read_from(0).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].event.id, evt.id);
    }

    #[test]
    fn open_survives_tail_line_with_non_ulid_id() {
        // Regression for review finding: a complete JSON line whose `id`
        // field is the wrong shape (parses as a String but isn't a valid
        // ULID) used to make `EventLog::open` fail at
        // `raise_floor_from_str`. The bus could then never mount on a
        // corrupted log, defeating the whole point of `read_from_lossy`.
        // `last_id_in_file` must walk back to the last *valid* line.
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let committed = {
            let log = EventLog::open(dir.path()).unwrap();
            log.append(draft_signal("ask_lead")).unwrap().id
        };
        // Append a complete, parseable, but malformed-id line.
        {
            let mut f = OpenOptions::new()
                .append(true)
                .open(dir.path().join(EVENTS_FILENAME))
                .unwrap();
            f.write_all(b"{\"id\":\"not-a-ulid\"}\n").unwrap();
        }

        // Open must succeed: walk back past the bad line, recover the prior id.
        let log = EventLog::open(dir.path()).unwrap();

        // The next append must use a ULID strictly greater than the prior valid id.
        let next = log.append(draft_signal("ask_lead")).unwrap();
        assert!(
            next.id > committed,
            "next id {} not > prior valid id {}",
            next.id,
            committed
        );
    }

    #[test]
    fn read_from_lossy_skips_lines_whose_id_is_not_a_ulid() {
        // Regression: `Event.id` is just a `String` at the type level, so
        // serde happily accepts `"id":"zzzz"`. The bus stores ids in a
        // lex-sorted projection — a junk id like "zzzz" sorts past every
        // real ULID and would make later legitimate events look already-
        // read. Demote to SkipReport so this never reaches the bus.
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let log = EventLog::open(dir.path()).unwrap();
        let good_a = log.append(draft_signal("ask_lead")).unwrap();
        // Hand-write a complete JSON line whose id field is the wrong shape.
        {
            let mut f = OpenOptions::new()
                .append(true)
                .open(dir.path().join(EVENTS_FILENAME))
                .unwrap();
            f.write_all(
                b"{\"id\":\"zzzz\",\"ts\":\"2026-04-26T00:00:00Z\",\"crew_id\":\"c\",\
                  \"mission_id\":\"m\",\"kind\":\"signal\",\"from\":\"x\",\"to\":null,\
                  \"type\":\"ask_lead\",\"payload\":{}}\n",
            )
            .unwrap();
        }
        let good_b = log.append(draft_signal("ask_lead")).unwrap();

        let (entries, skipped) = log.read_from_lossy(0).unwrap();
        assert_eq!(entries.len(), 2, "two valid events must come through");
        assert_eq!(entries[0].event.id, good_a.id);
        assert_eq!(entries[1].event.id, good_b.id);
        assert_eq!(skipped.len(), 1, "the malformed-id line must be a skip");
        assert!(
            skipped[0].error.contains("not a valid ULID"),
            "skip should explain why; got {:?}",
            skipped[0].error
        );
    }

    #[test]
    fn read_from_lossy_skips_non_utf8_lines_too() {
        // Regression: `read_line` requires UTF-8 and Errs out on the first
        // non-UTF-8 line, which would propagate up through the bus's tick
        // and freeze the offset. `read_until` with byte-level parsing
        // demotes that to a SkipReport.
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let log = EventLog::open(dir.path()).unwrap();
        let good_a = log.append(draft_signal("ask_lead")).unwrap();
        // Hand-write a complete-but-invalid-UTF-8 line directly.
        {
            let mut f = OpenOptions::new()
                .append(true)
                .open(dir.path().join(EVENTS_FILENAME))
                .unwrap();
            f.write_all(b"\xff\xfe garbage\n").unwrap();
        }
        let good_b = log.append(draft_signal("ask_lead")).unwrap();

        let (entries, skipped) = log.read_from_lossy(0).unwrap();
        assert_eq!(entries.len(), 2, "both good events surface");
        assert_eq!(entries[0].event.id, good_a.id);
        assert_eq!(entries[1].event.id, good_b.id);
        assert_eq!(skipped.len(), 1, "exactly one skip report");
        assert!(
            skipped[0].next_offset > skipped[0].offset,
            "skip must advance past the bad bytes"
        );
    }

    #[test]
    fn read_from_lossy_skips_bad_lines_and_advances_past_them() {
        // Regression for the C7 watcher freeze: `read_from` aborts the whole
        // call on the first parse error, leaving the consumer's offset stuck
        // and re-reading the same bad bytes forever. `read_from_lossy` must
        // skip the bad line, surface a SkipReport, and let later good lines
        // through.
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let log = EventLog::open(dir.path()).unwrap();
        let good_a = log.append(draft_signal("ask_lead")).unwrap();

        // Hand-write a malformed-but-newline-terminated line directly into
        // the file. Append-only writers never produce these in practice, but
        // a buggy CLI release or a manual edit could.
        {
            let mut f = OpenOptions::new()
                .append(true)
                .open(dir.path().join(EVENTS_FILENAME))
                .unwrap();
            f.write_all(b"this is not json\n").unwrap();
        }

        let good_b = log.append(draft_signal("ask_lead")).unwrap();

        let (entries, skipped) = log.read_from_lossy(0).unwrap();
        assert_eq!(entries.len(), 2, "both good events must be returned");
        assert_eq!(entries[0].event.id, good_a.id);
        assert_eq!(entries[1].event.id, good_b.id);
        assert_eq!(skipped.len(), 1, "exactly one skip report");
        assert!(
            skipped[0].next_offset > skipped[0].offset,
            "skip must advance past the bad line"
        );
        // Resuming after the second good entry yields nothing — i.e. the
        // skip's bytes were truly past us.
        let (more, _) = log.read_from_lossy(entries[1].next_offset).unwrap();
        assert!(more.is_empty());
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
