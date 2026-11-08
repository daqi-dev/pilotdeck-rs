//! Product conversation display reconstructed from committed opaque Kernel
//! history versions. Current model context remains a separate native value.
use crate::runtime::project::RuntimeError;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
fn error(message: &str) -> RuntimeError {
    RuntimeError::new(format!("Kernel history archive projection: {message}"))
}
fn strong(message: &Value) -> Option<&str> {
    message["metadata"]["kernelMessageId"]
        .as_str()
        .filter(|id| !id.is_empty())
}
fn key(message: &Value) -> String {
    if let Some(id) = strong(message) {
        return format!("id:{id}");
    }
    let mut body = message.clone();
    if let Some(object) = body.as_object_mut() {
        object.remove("metadata");
    }
    format!("legacy:{:x}", Sha256::digest(body.to_string().as_bytes()))
}
fn visible(message: &Value) -> bool {
    message["metadata"]["synthetic"] != true
        && matches!(
            message["role"].as_str(),
            Some("user" | "assistant" | "tool")
        )
}
#[derive(Default)]
pub(crate) struct ArchiveProjection {
    pub(crate) revision: u64,
    display: Vec<Value>,
    previous: Vec<(String, Option<usize>)>,
    identities: BTreeMap<String, Option<usize>>,
    boundaries: BTreeSet<String>,
}
impl ArchiveProjection {
    pub(crate) fn display(&self) -> Value {
        json!(self.display)
    }
    pub(crate) fn seed(&mut self, history: &Value) -> Result<(), RuntimeError> {
        if self.revision != 0 || !self.display.is_empty() {
            return Ok(());
        }
        let messages = match history {
            Value::Null => &[][..],
            Value::Array(messages) => messages.as_slice(),
            _ => return Err(error("context seed is not messages")),
        };
        self.merge(messages)?;
        Ok(())
    }
    pub(crate) fn apply(&mut self, entry: &Value) -> Result<(), RuntimeError> {
        let revision = entry["revision"]
            .as_u64()
            .ok_or_else(|| error("revision missing"))?;
        if revision != self.revision + 1 {
            return Err(error("archive has a gap or repeated revision"));
        }
        let messages = match &entry["history"] {
            Value::Null => &[][..],
            Value::Array(messages) => messages.as_slice(),
            _ => return Err(error("history version is not messages")),
        };
        // Validate the whole version before publishing any display mutation.
        let mut ids = BTreeSet::new();
        for message in messages {
            if !message.is_object() {
                return Err(error("invalid canonical message"));
            }
            if let Some(id) = strong(message) {
                if !ids.insert(id) {
                    return Err(error(
                        "message identity repeated within one committed version",
                    ));
                }
            }
        }
        self.merge(messages)?;
        let boundary = &entry["checkpoint"]["compaction"];
        if let Some(id) = boundary["compactMetadata"]["compactionId"]
            .as_str()
            .filter(|id| !id.is_empty())
        {
            if self.boundaries.insert(id.into()) {
                self.display.push(json!({"role":"system","metadata":{"kernelProjectionBoundary":true,"kernelMessageId":format!("kernel-compaction:{id}")},"content":[{"type":"compact_boundary","payload":boundary["compactMetadata"]}]}));
            }
        }
        self.revision = revision;
        Ok(())
    }
    fn merge(&mut self, messages: &[Value]) -> Result<(), RuntimeError> {
        // Legacy rows match ordered occurrences in the immediately previous
        // context, never the full conversation by global text equality.
        let mut old: BTreeMap<String, VecDeque<Option<usize>>> = BTreeMap::new();
        for (key, index) in &self.previous {
            old.entry(key.clone()).or_default().push_back(*index);
        }
        let mut next = Vec::new();
        for message in messages {
            let identity = key(message);
            let prior = if strong(message).is_some() {
                self.identities.get(&identity).copied()
            } else {
                old.get_mut(&identity).and_then(VecDeque::pop_front)
            };
            let index = if let Some(prior) = prior {
                prior
            } else if visible(message) {
                let index = self.display.len();
                self.display.push(message.clone());
                Some(index)
            } else {
                None
            };
            if strong(message).is_some() {
                self.identities.insert(identity.clone(), index);
            }
            next.push((identity, index));
        }
        self.previous = next;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn kernel_archive_preserves_precompact_history_and_distinct_equal_messages() {
        let user = json!({"role":"user","metadata":{"kernelMessageId":"u1","turnId":"r1"},"content":[{"type":"text","text":"same"}]});
        let answer = json!({"role":"assistant","metadata":{"kernelMessageId":"a1","turnId":"r1"},"content":[{"type":"text","text":"full answer"}]});
        let summary = json!({"role":"user","metadata":{"synthetic":true,"kernelMessageId":"summary"},"content":[{"type":"text","text":"summary context"}]});
        let boundary = json!({"compaction":{"kind":"compact","compactMetadata":{"compactionId":"compact","preTokens":1000,"postTokens":100}}});
        let mut projection = ArchiveProjection::default();
        projection
            .apply(&json!({"revision":1,"history":[user,answer],"checkpoint":null}))
            .unwrap();
        let mut truncated = answer.clone();
        truncated["content"][0]["text"] = json!("truncated");
        projection
            .apply(&json!({"revision":2,"history":[summary,truncated],"checkpoint":boundary}))
            .unwrap();
        let mut user2 = user.clone();
        user2["metadata"]["kernelMessageId"] = json!("u2");
        user2["metadata"]["turnId"] = json!("r2");
        let mut answer2 = answer.clone();
        answer2["metadata"]["kernelMessageId"] = json!("a2");
        answer2["metadata"]["turnId"] = json!("r2");
        projection
            .apply(&json!({"revision":3,"history":[summary,user2,answer2],"checkpoint":boundary}))
            .unwrap();
        let display = projection.display();
        assert_eq!(display.as_array().unwrap().len(), 5);
        assert_eq!(display[1]["content"][0]["text"], "full answer");
        assert_eq!(display[2]["content"][0]["type"], "compact_boundary");
        assert_eq!(display[3]["metadata"]["kernelMessageId"], "u2");
        assert!(!display.to_string().contains("summary context"));
        projection
            .apply(&json!({"revision":4,"history":[summary,user2,answer2],"checkpoint":boundary}))
            .unwrap();
        assert_eq!(projection.display(), display);
        assert!(
            projection
                .apply(&json!({"revision":6,"history":[]}))
                .is_err()
        );
    }
    #[test]
    fn kernel_archive_legacy_occurrences_and_tool_pairs_are_retained() {
        let user = json!({"role":"user","content":[{"type":"text","text":"same"}]});
        let assistant = json!({"role":"assistant","content":[{"type":"tool_call","id":"tool","name":"read","input":{}}]});
        let result = json!({"role":"user","content":[{"type":"tool_result","toolCallId":"tool","content":[{"type":"text","text":"result"}]}]});
        let mut projection = ArchiveProjection::default();
        projection
            .apply(&json!({"revision":1,"history":[user,user,assistant,result]}))
            .unwrap();
        projection
            .apply(&json!({"revision":2,"history":[user,user,assistant,result,user]}))
            .unwrap();
        let display = projection.display();
        assert_eq!(display.as_array().unwrap().len(), 5);
        assert_eq!(display[2], assistant);
        assert_eq!(display[3], result);
        let bad = json!({"revision":3,"history":[{"role":"user","metadata":{"kernelMessageId":"duplicate"},"content":[]},{"role":"assistant","metadata":{"kernelMessageId":"duplicate"},"content":[]}]});
        assert!(projection.apply(&bad).is_err());
        assert_eq!(projection.revision, 2);
    }
}
