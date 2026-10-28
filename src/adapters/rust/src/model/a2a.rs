use crate::{Error, Result};
use a2a_types::{MessageRole, MessageSendParams, Task};
use serde_json::{json, Map, Value};

/// Keeps original application metadata, validating through the upstream A2A 0.3 models.
#[derive(Clone, Debug)]
pub struct A2aCall {
    pub wire: Value,
}
impl A2aCall {
    pub fn parse(body: &[u8], limit: usize) -> Result<Option<Self>> {
        if body.len() > limit {
            return Err(Self::invalid("A2A request exceeds limit"));
        }
        let Ok(wire) = serde_json::from_slice::<Value>(body) else {
            return Ok(None);
        };
        if wire["jsonrpc"] != "2.0" {
            return Ok(None);
        }
        let method = wire["method"].as_str().unwrap_or_default();
        if ["message/stream", "SendMessage", "SendStreamingMessage"].contains(&method) {
            return Err(Self::invalid("paid A2A requires non-streaming v0.3 message/send"));
        }
        if method != "message/send" {
            return Ok(None);
        }
        if !(wire["id"].is_string() || wire["id"].is_number()) {
            return Err(Self::invalid("A2A request requires id"));
        }
        let call = Self { wire };
        call.params()?;
        Ok(Some(call))
    }
    pub fn params(&self) -> Result<MessageSendParams> {
        let params: MessageSendParams = serde_json::from_value(self.wire["params"].clone())
            .map_err(|e| Self::invalid(&format!("invalid A2A params: {e}")))?;
        if params.message.kind != "message"
            || params.message.role != MessageRole::User
            || params.message.message_id.is_empty()
            || params.message.parts.is_empty()
        {
            return Err(Self::invalid("invalid A2A message"));
        }
        Ok(params)
    }
    pub fn bind(&mut self, task: &Task) -> Result<()> {
        let message = self.params()?.message;
        if message.task_id.as_ref().is_some_and(|id| id != &task.id)
            || message.context_id.as_ref().is_some_and(|id| id != &task.context_id)
        {
            return Err(Self::invalid("A2A task/context mismatch"));
        }
        self.wire["params"]["message"]["taskId"] = json!(task.id);
        self.wire["params"]["message"]["contextId"] = json!(task.context_id);
        Ok(())
    }
    pub fn body(&self) -> Result<Vec<u8>> {
        let mut wire = self.wire.clone();
        wire.sort_all_objects();
        serde_json::to_vec(&wire).map_err(|e| Self::invalid(&e.to_string()))
    }
    pub fn task(message: &Value) -> Result<Task> {
        let task: Task = serde_json::from_value(message["result"].clone())
            .map_err(|e| Self::invalid(&format!("invalid A2A Task: {e}")))?;
        if task.kind != "task" || task.id.is_empty() || task.context_id.is_empty() {
            return Err(Self::invalid("invalid A2A Task identity"));
        }
        Ok(task)
    }
    pub fn response(message: &Value, id: &Value) -> Result<()> {
        if message["jsonrpc"] != "2.0"
            || &message["id"] != id
            || message.get("result").is_some() == message.get("error").is_some()
        {
            return Err(Self::invalid("A2A response does not match request"));
        }
        Ok(())
    }
    pub fn metadata(message: &mut Value, root: bool) -> Result<&mut Map<String, Value>> {
        let owner = if root {
            message
        } else {
            &mut message["params"]["message"]
        };
        let owner = owner
            .as_object_mut()
            .ok_or_else(|| Self::invalid("A2A metadata owner must be object"))?;
        owner
            .entry(if root { "_meta" } else { "metadata" })
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or_else(|| Self::invalid("A2A metadata must be object"))
    }
    #[must_use]
    pub fn invalid(message: &str) -> Error {
        Error::InvalidArgument(message.to_owned())
    }
}
#[cfg(test)]
#[path = "../../tests/model/a2a_test.rs"]
mod tests;
