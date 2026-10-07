//! Client-side copies of T3 projections, kept current from stream items.
//!
//! Every V2 domain event carries the whole updated entity, so applying one is an upsert by id.
//! Entities stay as JSON: fields this build does not know pass through untouched.

use serde_json::{Map, Value};

/// Which projection array an event type upserts into. `None` for events that change no array.
fn collection_for(event_type: &str) -> Option<&'static str> {
    Some(match event_type {
        "run.created" | "run.updated" => "runs",
        "run-attempt.created" | "run-attempt.updated" => "attempts",
        "node.updated" => "nodes",
        "subagent.updated" => "subagents",
        "provider-session.attached" | "provider-session.updated" => "providerSessions",
        "provider-thread.updated" => "providerThreads",
        "provider-turn.updated" => "providerTurns",
        "runtime-request.updated" => "runtimeRequests",
        "message.updated" => "messages",
        "turn-item.updated" => "turnItems",
        "plan.updated" => "plans",
        "checkpoint-scope.created" => "checkpointScopes",
        "checkpoint.captured" => "checkpoints",
        "context-handoff.updated" => "contextHandoffs",
        "context-transfer.created" | "context-transfer.updated" => "contextTransfers",
        _ => return None,
    })
}

fn upsert(array: &mut Vec<Value>, entity: Value) {
    let id = entity.get("id").cloned();
    match array.iter_mut().find(|existing| id.is_some() && existing.get("id") == id.as_ref()) {
        Some(existing) => *existing = entity,
        None => array.push(entity),
    }
}

fn array_mut<'a>(object: &'a mut Map<String, Value>, key: &str) -> &'a mut Vec<Value> {
    let slot = object.entry(key).or_insert_with(|| Value::Array(Vec::new()));
    if !slot.is_array() {
        *slot = Value::Array(Vec::new());
    }
    slot.as_array_mut().expect("just made an array")
}

/// What a stream item did to local state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applied {
    Snapshot,
    Event(String),
    /// Already applied, from the overlap a resume replays.
    Duplicate,
    /// The server finished replaying history; later items are live.
    Synchronized,
    Ignored,
}

#[derive(Debug, Clone, Default)]
pub struct ThreadState {
    pub sequence: u64,
    pub projection: Map<String, Value>,
    pub synchronized: bool,
}

impl ThreadState {
    pub fn from_snapshot(snapshot: &Value) -> Option<ThreadState> {
        let mut state = ThreadState::default();
        state.replace(snapshot)?;
        Some(state)
    }

    fn replace(&mut self, snapshot: &Value) -> Option<()> {
        let projection = snapshot.get("projection")?.as_object()?.clone();
        projection.get("thread")?.get("id")?.as_str()?;
        self.sequence = snapshot.get("snapshotSequence").and_then(Value::as_u64).unwrap_or(0);
        self.projection = projection;
        Some(())
    }

    /// Applies one `orchestration.subscribeThread` item.
    pub fn apply(&mut self, item: &Value) -> Applied {
        match item.get("kind").and_then(Value::as_str) {
            Some("synchronized") => {
                self.synchronized = true;
                Applied::Synchronized
            }
            Some("snapshot") => match self.replace(item) {
                Some(()) => Applied::Snapshot,
                None => Applied::Ignored,
            },
            Some("event") => {
                let sequence = item.get("sequence").and_then(Value::as_u64).unwrap_or(0);
                if sequence <= self.sequence {
                    return Applied::Duplicate;
                }
                self.sequence = sequence;
                let Some(event) = item.get("event") else { return Applied::Ignored };
                let event_type = event.get("type").and_then(Value::as_str).unwrap_or_default().to_string();
                let Some(payload) = event.get("payload").cloned() else { return Applied::Event(event_type) };
                if event_type.starts_with("thread.") {
                    self.projection.insert("thread".into(), payload);
                } else if let Some(key) = collection_for(&event_type) {
                    // Creating the list here would hide every item the snapshot sent only in turnItems.
                    if key == "turnItems" && self.projection.contains_key("visibleTurnItems") {
                        self.update_visible_item(&payload);
                    }
                    upsert(array_mut(&mut self.projection, key), payload);
                }
                Applied::Event(event_type)
            }
            _ => Applied::Ignored,
        }
    }

    /// Keeps `visibleTurnItems`, the server's display order including inherited fork history, in step.
    fn update_visible_item(&mut self, item: &Value) {
        let thread_id = self.thread_id().to_string();
        let visible = array_mut(&mut self.projection, "visibleTurnItems");
        let id = item.get("id");
        if let Some(entry) = visible.iter_mut().find(|e| e.get("item").and_then(|i| i.get("id")) == id) {
            if let Some(object) = entry.as_object_mut() {
                object.insert("item".into(), item.clone());
            }
            return;
        }
        let position = visible.iter().filter_map(|e| e.get("position").and_then(Value::as_u64)).max().map_or(0, |p| p + 1);
        visible.push(serde_json::json!({
            "position": position,
            "visibility": "local",
            "sourceThreadId": thread_id,
            "sourceItemId": id.cloned().unwrap_or(Value::Null),
            "item": item,
        }));
    }

    pub fn thread(&self) -> &Value {
        self.projection.get("thread").unwrap_or(&Value::Null)
    }

    pub fn thread_id(&self) -> &str {
        self.thread().get("id").and_then(Value::as_str).unwrap_or_default()
    }

    pub fn list(&self, key: &str) -> &[Value] {
        self.projection.get(key).and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Turn items in display order. Falls back to `turnItems` by ordinal when the server sent no visible list.
    pub fn items(&self) -> Vec<&Value> {
        let visible = self.list("visibleTurnItems");
        if !visible.is_empty() {
            let mut entries: Vec<&Value> = visible.iter().collect();
            entries.sort_by_key(|e| e.get("position").and_then(Value::as_u64).unwrap_or(0));
            return entries.into_iter().filter_map(|e| e.get("item")).collect();
        }
        let mut items: Vec<&Value> = self.list("turnItems").iter().collect();
        items.sort_by_key(|i| i.get("ordinal").and_then(Value::as_u64).unwrap_or(0));
        items
    }

    pub fn run(&self, run_id: &str) -> Option<&Value> {
        self.list("runs").iter().find(|r| r.get("id").and_then(Value::as_str) == Some(run_id))
    }

    /// The run working right now, if any.
    pub fn active_run(&self) -> Option<&Value> {
        self.list("runs").iter().filter(|r| is_active_status(status(r))).max_by_key(|r| ordinal(r))
    }

    pub fn latest_run(&self) -> Option<&Value> {
        self.list("runs").iter().max_by_key(|r| ordinal(r))
    }

    pub fn run_for_message(&self, message_id: &str) -> Option<&Value> {
        let runs = self.list("runs");
        if let Some(run) = runs.iter().find(|r| r.get("userMessageId").and_then(Value::as_str) == Some(message_id)) {
            return Some(run);
        }
        let run_id = self
            .list("messages")
            .iter()
            .find(|m| m.get("id").and_then(Value::as_str) == Some(message_id))
            .and_then(|m| m.get("runId"))
            .and_then(Value::as_str)?;
        self.run(run_id)
    }

    pub fn pending_requests(&self) -> Vec<&Value> {
        self.list("runtimeRequests").iter().filter(|r| r.get("status").and_then(Value::as_str) == Some("pending")).collect()
    }

    /// The turn item that shows a runtime request to the user, which carries its prompt and options.
    pub fn request_item(&self, request_id: &str) -> Option<&Value> {
        self.list("turnItems").iter().rev().find(|i| i.get("requestId").and_then(Value::as_str) == Some(request_id))
    }
}

pub fn status(entity: &Value) -> &str {
    entity.get("status").and_then(Value::as_str).unwrap_or_default()
}

fn ordinal(entity: &Value) -> u64 {
    entity.get("ordinal").and_then(Value::as_u64).unwrap_or(0)
}

pub fn is_active_status(status: &str) -> bool {
    matches!(status, "preparing" | "starting" | "running" | "waiting")
}

pub fn is_terminal_status(status: &str) -> bool {
    matches!(status, "completed" | "interrupted" | "failed" | "cancelled" | "rolled_back")
}

/// Projects and threads, kept current from `orchestration.subscribeShell`.
#[derive(Debug, Clone, Default)]
pub struct ShellState {
    pub sequence: u64,
    pub projects: Vec<Value>,
    pub threads: Vec<Value>,
    pub synchronized: bool,
}

impl ShellState {
    pub fn from_snapshot(snapshot: &Value) -> ShellState {
        let list = |key: &str| snapshot.get(key).and_then(Value::as_array).cloned().unwrap_or_default();
        ShellState {
            sequence: snapshot.get("snapshotSequence").and_then(Value::as_u64).unwrap_or(0),
            projects: list("projects").into_iter().filter(|p| p.get("deletedAt").is_none_or(Value::is_null)).collect(),
            threads: list("threads"),
            synchronized: false,
        }
    }

    pub fn apply(&mut self, item: &Value) -> Applied {
        let kind = item.get("kind").and_then(Value::as_str).unwrap_or_default();
        match kind {
            "synchronized" => {
                self.synchronized = true;
                return Applied::Synchronized;
            }
            "snapshot" => {
                let Some(snapshot) = item.get("snapshot") else { return Applied::Ignored };
                let synchronized = self.synchronized;
                *self = ShellState::from_snapshot(snapshot);
                self.synchronized = synchronized;
                return Applied::Snapshot;
            }
            _ => {}
        }
        let sequence = item.get("sequence").and_then(Value::as_u64).unwrap_or(0);
        if sequence <= self.sequence {
            return Applied::Duplicate;
        }
        self.sequence = sequence;
        let id_of = |v: &Value| v.get("id").and_then(Value::as_str).map(str::to_string);
        match kind {
            "project.updated" => {
                if let Some(project) = item.get("project").cloned() {
                    upsert(&mut self.projects, project);
                }
            }
            "project.removed" => {
                let id = item.get("projectId").and_then(Value::as_str);
                self.projects.retain(|p| p.get("id").and_then(Value::as_str) != id);
            }
            "thread.updated" => {
                let Some(thread) = item.get("thread").cloned() else { return Applied::Ignored };
                if item.get("location").and_then(Value::as_str) == Some("archive") {
                    let id = id_of(&thread);
                    self.threads.retain(|t| id_of(t) != id);
                } else {
                    upsert(&mut self.threads, thread);
                }
            }
            "thread.removed" => {
                let id = item.get("threadId").and_then(Value::as_str);
                self.threads.retain(|t| t.get("id").and_then(Value::as_str) != id);
            }
            _ => return Applied::Ignored,
        }
        Applied::Event(kind.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn snapshot() -> Value {
        json!({
            "kind": "snapshot",
            "snapshotSequence": 10,
            "projection": {
                "thread": {"id": "t1", "title": "Test"},
                "runs": [{"id": "r1", "ordinal": 1, "status": "completed", "userMessageId": "m1"}],
                "runtimeRequests": [],
                "turnItems": [{"id": "i1", "type": "user_message", "ordinal": 0, "text": "hi"}],
                "visibleTurnItems": [{"position": 0, "item": {"id": "i1", "type": "user_message", "ordinal": 0, "text": "hi"}}]
            }
        })
    }

    fn event(sequence: u64, event_type: &str, payload: Value) -> Value {
        json!({"kind": "event", "sequence": sequence, "event": {"type": event_type, "payload": payload}})
    }

    #[test]
    fn applies_events_as_upserts_and_skips_replayed_sequences() {
        let mut state = ThreadState::default();
        assert_eq!(state.apply(&snapshot()), Applied::Snapshot);
        let delta = |text: &str| json!({"id": "i2", "type": "assistant_message", "ordinal": 1, "text": text, "streaming": true});

        assert_eq!(state.apply(&event(11, "turn-item.updated", delta("Hel"))), Applied::Event("turn-item.updated".into()));
        assert_eq!(state.apply(&event(12, "turn-item.updated", delta("Hello"))), Applied::Event("turn-item.updated".into()));
        // A reconnect replays from the last sequence the client confirmed; overlap must not double-apply.
        assert_eq!(state.apply(&event(12, "turn-item.updated", delta("stale"))), Applied::Duplicate);

        let items = state.items();
        assert_eq!(items.len(), 2);
        assert_eq!(items[1]["text"], "Hello");
        assert_eq!(state.list("turnItems").len(), 2);
    }

    #[test]
    fn tracks_runs_and_pending_requests() {
        let mut state = ThreadState::from_snapshot(&snapshot()).unwrap();
        state.apply(&event(11, "run.created", json!({"id": "r2", "ordinal": 2, "status": "running", "userMessageId": "m2"})));
        state.apply(&event(12, "runtime-request.updated", json!({"id": "q1", "status": "pending", "kind": "command"})));
        assert_eq!(state.active_run().unwrap()["id"], "r2");
        assert_eq!(state.run_for_message("m2").unwrap()["id"], "r2");
        assert_eq!(state.pending_requests().len(), 1);

        state.apply(&event(13, "runtime-request.updated", json!({"id": "q1", "status": "resolved", "kind": "command"})));
        state.apply(&event(14, "run.updated", json!({"id": "r2", "ordinal": 2, "status": "completed", "userMessageId": "m2"})));
        assert!(state.pending_requests().is_empty());
        assert!(state.active_run().is_none());
    }

    #[test]
    fn thread_events_replace_the_thread_and_unknown_events_still_advance() {
        let mut state = ThreadState::from_snapshot(&snapshot()).unwrap();
        state.apply(&event(11, "thread.metadata-updated", json!({"id": "t1", "title": "Renamed"})));
        assert_eq!(state.thread()["title"], "Renamed");
        assert_eq!(state.apply(&event(12, "brand-new.event", json!({}))), Applied::Event("brand-new.event".into()));
        assert_eq!(state.sequence, 12);
    }

    #[test]
    fn shell_moves_archived_threads_out_of_the_active_list() {
        let mut shell = ShellState::from_snapshot(&json!({
            "snapshotSequence": 5,
            "projects": [{"id": "p1"}, {"id": "p2", "deletedAt": "2026-01-01T00:00:00Z"}],
            "threads": [{"id": "t1", "title": "One"}]
        }));
        assert_eq!(shell.projects.len(), 1);
        shell.apply(&json!({"kind": "thread.updated", "sequence": 6, "location": "active", "thread": {"id": "t2"}}));
        shell.apply(&json!({"kind": "thread.updated", "sequence": 7, "location": "archive", "thread": {"id": "t1"}}));
        assert_eq!(shell.apply(&json!({"kind": "thread.removed", "sequence": 7, "location": "active", "threadId": "t2"})), Applied::Duplicate);
        let ids: Vec<_> = shell.threads.iter().map(|t| t["id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["t2"]);
    }
}
