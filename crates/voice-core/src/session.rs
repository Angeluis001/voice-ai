//! Session documents on disk: `<result_dir>/<call_id>-session.json`.
//!
//! Live transcripts, assistant text, turn completion and async intent reasoning all
//! update the same file concurrently. Every update goes through a per-file lock and an
//! in-memory copy, so a stale read can never wipe earlier turns. Files are written to
//! a temporary name and renamed, so readers (the console) never see partial JSON.
use crate::{
    slots::{empty_slots, merge_slots},
    util::{js_num, now_iso},
    Doc,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

/// Identity used when a document has to be created or repaired.
#[derive(Debug, Clone, Default)]
pub struct SessionMeta {
    pub call_id: String,
    pub target_number: String,
}

#[derive(Debug, Clone, Default)]
pub struct SessionExtras {
    pub recording_path: String,
    pub channel_uuid: String,
}

#[derive(Default)]
pub struct SessionStore {
    result_dir: PathBuf,
    cache: Mutex<HashMap<PathBuf, Doc>>,
    locks: Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>,
}

pub fn empty_session_doc(call_id: &str, target_number: &str, extras: &SessionExtras) -> Doc {
    let call_id = if call_id.is_empty() {
        "unknown"
    } else {
        call_id
    };
    let target = if target_number.is_empty() {
        "9198"
    } else {
        target_number
    };
    let now = now_iso();
    let Value::Object(doc) = json!({
        "conversation_id": call_id,
        "target_number": target,
        "prompt_profile": "sop_enquiries_realtime",
        "status": "active",
        "mode": "realtime",
        "started_at": now,
        "updated_at": now,
        "latest_phase": "active",
        "latest_transcript": "",
        "latest_response": "",
        "latest_intent": "",
        "slots": Value::Object(empty_slots()),
        "turns": [],
        "recording_path": extras.recording_path,
        "channel_uuid": extras.channel_uuid,
    }) else {
        unreachable!()
    };
    doc
}

fn sort_turns(doc: &mut Doc) {
    if let Some(Value::Array(turns)) = doc.get_mut("turns") {
        turns.sort_by(|a, b| {
            js_num(a.get("turn"))
                .partial_cmp(&js_num(b.get("turn")))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }
}

/// Insert a turn, or shallow-merge it into the existing turn with the same number.
pub fn upsert_turn(doc: &mut Doc, turn: Doc) {
    let number = js_num(turn.get("turn"));
    let turns = doc.entry("turns").or_insert_with(|| Value::Array(vec![]));
    if !turns.is_array() {
        *turns = Value::Array(vec![]);
    }
    let list = turns.as_array_mut().expect("turns is an array");
    match list.iter_mut().find(|t| js_num(t.get("turn")) == number) {
        Some(Value::Object(existing)) => {
            for (k, v) in turn {
                existing.insert(k, v);
            }
        }
        Some(other) => *other = Value::Object(turn),
        None => list.push(Value::Object(turn)),
    }
    sort_turns(doc);
}

/// Turns of a document as objects (non-object entries are skipped).
pub fn turns(doc: &Doc) -> Vec<&Doc> {
    doc.get("turns")
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(Value::as_object).collect())
        .unwrap_or_default()
}

impl SessionStore {
    pub fn new(result_dir: impl Into<PathBuf>) -> Self {
        Self {
            result_dir: result_dir.into(),
            ..Default::default()
        }
    }

    pub fn path_for(&self, call_id: &str) -> PathBuf {
        self.result_dir.join(format!("{call_id}-session.json"))
    }

    fn lock_for(&self, path: &Path) -> Arc<tokio::sync::Mutex<()>> {
        self.locks
            .lock()
            .unwrap()
            .entry(path.to_path_buf())
            .or_default()
            .clone()
    }

    fn cached(&self, path: &Path) -> Option<Doc> {
        self.cache.lock().unwrap().get(path).cloned()
    }

    /// Drop the in-memory copy once a call has ended.
    pub fn forget(&self, path: &Path) {
        self.cache.lock().unwrap().remove(path);
        self.locks.lock().unwrap().remove(path);
    }

    /// Read a session file, retrying briefly when it is empty or half-written.
    pub async fn load(path: &Path) -> Option<Doc> {
        for attempt in 0..4u64 {
            match tokio::fs::read_to_string(path).await {
                Ok(raw) if !raw.trim().is_empty() => match serde_json::from_str::<Value>(&raw) {
                    Ok(Value::Object(doc)) => return Some(doc),
                    Ok(_) => return None,
                    Err(err) if attempt == 3 => {
                        tracing::warn!(phase = "session_load_error", path = %path.display(), error = %err);
                        return None;
                    }
                    Err(_) => {}
                },
                Ok(_) if attempt == 3 => return None,
                Ok(_) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
                Err(err) if attempt == 3 => {
                    tracing::warn!(phase = "session_load_error", path = %path.display(), error = %err);
                    return None;
                }
                Err(_) => {}
            }
            tokio::time::sleep(Duration::from_millis(15 * (attempt + 1))).await;
        }
        None
    }

    async fn save(&self, path: &Path, doc: &mut Doc) -> std::io::Result<()> {
        doc.insert("updated_at".into(), Value::String(now_iso()));
        let payload = serde_json::to_string_pretty(&Value::Object(doc.clone()))
            .map_err(std::io::Error::other)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let tmp = PathBuf::from(format!("{}.{}.tmp", path.display(), std::process::id()));
        tokio::fs::write(&tmp, payload).await?;
        // On Windows a reader holding the file open can make the rename fail briefly.
        let mut last_err = None;
        for attempt in 0..5u64 {
            match tokio::fs::rename(&tmp, path).await {
                Ok(()) => {
                    last_err = None;
                    break;
                }
                Err(err) => {
                    last_err = Some(err);
                    tokio::time::sleep(Duration::from_millis(10 * (attempt + 1))).await;
                }
            }
        }
        if let Some(err) = last_err {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(err);
        }
        self.cache
            .lock()
            .unwrap()
            .insert(path.to_path_buf(), doc.clone());
        Ok(())
    }

    /// Open the session for a call: reuse an existing document or create a new one.
    pub async fn ensure(
        &self,
        call_id: &str,
        target_number: &str,
        extras: &SessionExtras,
    ) -> std::io::Result<(PathBuf, Doc)> {
        let path = self.path_for(call_id);
        let lock = self.lock_for(&path);
        let _guard = lock.lock().await;
        let existing = match self.cached(&path) {
            Some(doc) => Some(doc),
            None => Self::load(&path).await,
        };
        if let Some(mut doc) =
            existing.filter(|d| d.get("turns").map(Value::is_array).unwrap_or(false))
        {
            let id = crate::util::field(&doc, "conversation_id");
            if id.is_empty() || id == "unknown" {
                doc.insert("conversation_id".into(), json!(call_id));
            }
            doc.insert(
                "slots".into(),
                Value::Object(merge_slots(None, doc.get("slots"))),
            );
            for (key, default) in [
                ("mode", "realtime"),
                ("prompt_profile", "sop_enquiries_realtime"),
                ("target_number", target_number),
            ] {
                if crate::util::field(&doc, key).is_empty() {
                    doc.insert(key.into(), json!(default));
                }
            }
            if !extras.recording_path.is_empty() {
                doc.insert("recording_path".into(), json!(extras.recording_path));
            }
            if !extras.channel_uuid.is_empty() {
                doc.insert("channel_uuid".into(), json!(extras.channel_uuid));
            }
            self.cache.lock().unwrap().insert(path.clone(), doc.clone());
            return Ok((path, doc));
        }
        let mut doc = empty_session_doc(call_id, target_number, extras);
        self.save(&path, &mut doc).await?;
        Ok((path, doc))
    }

    /// Serialized read-modify-write of a session document.
    pub async fn patch<F>(
        &self,
        path: &Path,
        meta: &SessionMeta,
        mutator: F,
    ) -> std::io::Result<Doc>
    where
        F: FnOnce(&mut Doc),
    {
        let lock = self.lock_for(path);
        let _guard = lock.lock().await;
        let mut doc = match self.cached(path) {
            Some(doc) => doc,
            None => match Self::load(path).await {
                Some(doc) => doc,
                None => empty_session_doc(
                    &meta.call_id,
                    &meta.target_number,
                    &SessionExtras::default(),
                ),
            },
        };
        if !doc.get("turns").map(Value::is_array).unwrap_or(false) {
            doc.insert("turns".into(), json!([]));
        }
        // Never let a fallback wipe identity after a real session started.
        let id = crate::util::field(&doc, "conversation_id");
        if !meta.call_id.is_empty() && (id.is_empty() || id == "unknown") {
            doc.insert("conversation_id".into(), json!(meta.call_id));
        }
        if !meta.target_number.is_empty() && crate::util::field(&doc, "target_number").is_empty() {
            doc.insert("target_number".into(), json!(meta.target_number));
        }
        doc.insert(
            "slots".into(),
            Value::Object(merge_slots(None, doc.get("slots"))),
        );
        mutator(&mut doc);
        sort_turns(&mut doc);
        self.save(path, &mut doc).await?;
        Ok(doc)
    }

    /// Current document: in-memory copy if the call is live, otherwise from disk.
    pub async fn read(&self, path: &Path) -> Option<Doc> {
        match self.cached(path) {
            Some(doc) => Some(doc),
            None => Self::load(path).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(n: i64, text: &str) -> Doc {
        json!({"turn": n, "transcript": text})
            .as_object()
            .unwrap()
            .clone()
    }

    #[test]
    fn upsert_merges_and_sorts() {
        let mut doc = empty_session_doc("c1", "9198", &SessionExtras::default());
        upsert_turn(&mut doc, turn(2, "two"));
        upsert_turn(&mut doc, turn(1, "one"));
        let mut update = turn(2, "two!");
        update.insert("intent".into(), json!("ask_fees"));
        upsert_turn(&mut doc, update);
        let list = turns(&doc);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0]["turn"], 1);
        assert_eq!(list[1]["transcript"], "two!");
        assert_eq!(list[1]["intent"], "ask_fees");
    }

    #[tokio::test]
    async fn ensure_creates_then_reuses() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path());
        let extras = SessionExtras {
            recording_path: "/m/x-call.wav".into(),
            channel_uuid: "x".into(),
        };
        let (path, doc) = store.ensure("call-1", "9198", &extras).await.unwrap();
        assert_eq!(path.file_name().unwrap(), "call-1-session.json");
        assert_eq!(doc["conversation_id"], "call-1");
        assert_eq!(doc["prompt_profile"], "sop_enquiries_realtime");
        assert_eq!(doc["channel_uuid"], "x");

        store
            .patch(&path, &SessionMeta::default(), |d| {
                upsert_turn(d, turn(0, ""))
            })
            .await
            .unwrap();
        store.forget(&path);
        let (_, again) = store.ensure("call-1", "9198", &extras).await.unwrap();
        assert_eq!(turns(&again).len(), 1, "existing turns are kept");
    }

    #[tokio::test]
    async fn concurrent_patches_keep_every_turn() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::new(dir.path()));
        let (path, _) = store
            .ensure("c", "9198", &SessionExtras::default())
            .await
            .unwrap();
        let mut tasks = vec![];
        for n in 1..=25 {
            let (store, path) = (store.clone(), path.clone());
            tasks.push(tokio::spawn(async move {
                store
                    .patch(&path, &SessionMeta::default(), |d| {
                        upsert_turn(d, turn(n, "t"))
                    })
                    .await
                    .unwrap();
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }
        store.forget(&path);
        let on_disk = SessionStore::load(&path).await.unwrap();
        let numbers: Vec<i64> = turns(&on_disk)
            .iter()
            .map(|t| t["turn"].as_i64().unwrap())
            .collect();
        assert_eq!(numbers, (1..=25).collect::<Vec<_>>());
        assert!(on_disk["updated_at"].as_str().unwrap().ends_with('Z'));
    }

    #[tokio::test]
    async fn patch_repairs_unknown_identity() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path());
        let path = store.path_for("late");
        let meta = SessionMeta {
            call_id: "late".into(),
            target_number: "9199".into(),
        };
        let doc = store.patch(&path, &meta, |_| {}).await.unwrap();
        assert_eq!(doc["conversation_id"], "late");
        assert_eq!(doc["target_number"], "9199");
    }

    #[tokio::test]
    async fn missing_file_loads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(SessionStore::load(&dir.path().join("nope.json"))
            .await
            .is_none());
    }
}
