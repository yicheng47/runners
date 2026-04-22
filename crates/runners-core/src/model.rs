// Event envelope and supporting types, shared between the app and the CLI.
//
// SQLite row types (Crew, Runner, Mission, Session) live in the app — the CLI
// never touches the DB. Only the on-the-wire Event shape lives here.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub type Timestamp = DateTime<Utc>;
pub type Ulid = String;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SignalType(pub String);

impl SignalType {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for SignalType {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for SignalType {
    fn from(s: String) -> Self {
        Self(s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EventKind {
    Signal,
    Message,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: Ulid,
    pub ts: Timestamp,
    pub crew_id: String,
    pub mission_id: String,
    pub kind: EventKind,
    pub from: String,
    pub to: Option<String>,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub signal_type: Option<SignalType>,
    #[serde(default)]
    pub payload: serde_json::Value,
}

/// All of `Event`'s fields except `id` and `ts`. `EventLog::append` takes a
/// draft and assigns `id` + `ts` inside the flock so cross-process appends
/// stay monotonic.
#[derive(Debug, Clone)]
pub struct EventDraft {
    pub crew_id: String,
    pub mission_id: String,
    pub kind: EventKind,
    pub from: String,
    pub to: Option<String>,
    pub signal_type: Option<SignalType>,
    pub payload: serde_json::Value,
}

impl EventDraft {
    pub fn signal(
        crew_id: impl Into<String>,
        mission_id: impl Into<String>,
        from: impl Into<String>,
        signal_type: impl Into<SignalType>,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            crew_id: crew_id.into(),
            mission_id: mission_id.into(),
            kind: EventKind::Signal,
            from: from.into(),
            to: None,
            signal_type: Some(signal_type.into()),
            payload,
        }
    }

    pub fn message(
        crew_id: impl Into<String>,
        mission_id: impl Into<String>,
        from: impl Into<String>,
        to: Option<String>,
        text: impl Into<String>,
    ) -> Self {
        Self {
            crew_id: crew_id.into(),
            mission_id: mission_id.into(),
            kind: EventKind::Message,
            from: from.into(),
            to,
            signal_type: None,
            payload: serde_json::json!({ "text": text.into() }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_event_roundtrips_as_documented_envelope() {
        let json = serde_json::json!({
            "id": "01HG3K1YRG7RQ3N9ABCDEFGHJK",
            "ts": "2026-04-21T12:34:56.123Z",
            "crew_id": "01HGCREW",
            "mission_id": "01HGMSN",
            "kind": "signal",
            "from": "coder",
            "to": null,
            "type": "ask_lead",
            "payload": { "question": "?", "context": "..." }
        });

        let evt: Event = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(evt.kind, EventKind::Signal);
        assert_eq!(evt.signal_type.as_ref().unwrap().as_str(), "ask_lead");
        assert_eq!(evt.to, None);

        let back = serde_json::to_value(&evt).unwrap();
        assert_eq!(back, json);
    }

    #[test]
    fn message_event_omits_type_when_serialized() {
        let evt = Event {
            id: "01HGMSG".into(),
            ts: Utc::now(),
            crew_id: "c".into(),
            mission_id: "m".into(),
            kind: EventKind::Message,
            from: "lead".into(),
            to: Some("impl".into()),
            signal_type: None,
            payload: serde_json::json!({ "text": "hi" }),
        };
        let v = serde_json::to_value(&evt).unwrap();
        assert!(v.get("type").is_none(), "messages must omit `type`");
        assert_eq!(v["to"], "impl");
    }
}
