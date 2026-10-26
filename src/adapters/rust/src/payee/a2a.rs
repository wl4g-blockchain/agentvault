use crate::{A2aCall, PayeeMiddleware, PaymentChallenge, ProtocolMcpPaymentRequired, Result, SettledIncomingPayment};
use a2a_types::{AgentCard, AgentExtension, Task, TaskState, TaskStatus};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::Arc};

/// Adapt the application's task store using upstream Task values; no SDK-owned task database.
#[async_trait]
pub trait A2aTaskStore: Send + Sync {
    async fn get(&self, id: &str) -> Result<Option<Task>>;
    async fn save(&self, task: &Task) -> Result<()>;
}
#[derive(Clone)]
pub struct A2aPaymentCall {
    pub call: A2aCall,
    pub task: Task,
    pub credential: Option<Value>,
    store: Arc<dyn A2aTaskStore>,
}
impl A2aPaymentCall {
    const SCOPE: [&'static str; 3] = [
        "agentvault.payment.run_id",
        "agentvault.payment.scenario_id",
        "agentvault.payment.price_id",
    ];
    async fn persist(&self, mut task: Task) -> Result<()> {
        if self.task.id != task.id || self.task.context_id != task.context_id {
            return Err(A2aCall::invalid("A2A response changed task/context"));
        }
        let target = task.metadata.get_or_insert_default();
        for key in Self::SCOPE {
            let value = self
                .task
                .metadata
                .as_ref()
                .and_then(|m| m.get(key))
                .cloned()
                .unwrap_or(Value::Null);
            if target.get(key).is_some_and(|v| v != &value) {
                return Err(A2aCall::invalid("handler changed payment scope"));
            }
            target.insert(key.into(), value);
        }
        self.store.save(&task).await
    }
}
impl<T> PayeeMiddleware<T> {
    pub fn a2a_agent_card(&self, card: &mut AgentCard) -> Result<()> {
        let mut value = serde_json::to_value(&*card).map_err(|e| A2aCall::invalid(&e.to_string()))?;
        let caps = &mut value["capabilities"];
        if caps["extensions"].is_null() {
            caps["extensions"] = json!([]);
        }
        let extensions = caps["extensions"]
            .as_array_mut()
            .ok_or_else(|| A2aCall::invalid("invalid AgentCard extensions"))?;
        for uri in self.protocols.a2a_extensions("").split(", ").filter(|s| !s.is_empty()) {
            if !extensions.iter().any(|e| e["uri"] == uri) {
                let extension: AgentExtension = serde_json::from_value(json!({"uri":uri,"required":true}))
                    .map_err(|e| A2aCall::invalid(&e.to_string()))?;
                extensions.push(serde_json::to_value(extension).map_err(|e| A2aCall::invalid(&e.to_string()))?);
            }
        }
        *card = serde_json::from_value(value).map_err(|e| A2aCall::invalid(&e.to_string()))?;
        Ok(())
    }
    pub fn a2a_extension(&self, protocol: &str) -> Result<&'static str> {
        Ok(self.protocols.get(protocol)?.a2a_extension())
    }
    pub async fn prepare_a2a(
        &self,
        body: &[u8],
        operation: &crate::PayeeOperation,
        store: Arc<dyn A2aTaskStore>,
        headers: &BTreeMap<String, String>,
    ) -> Result<Option<A2aPaymentCall>> {
        let Some(mut call) = A2aCall::parse(body, self.config().max_body_bytes)? else {
            return Ok(None);
        };
        if operation.run_id.is_empty() {
            return Err(A2aCall::invalid("A2A requires run_id"));
        }
        let adapter = self.protocols.get(&operation.protocol)?;
        let extension = adapter.a2a_extension();
        if !extension.is_empty()
            && !adapter
                .header(headers, "x-a2a-extensions")
                .unwrap_or_default()
                .split(',')
                .any(|s| s.trim() == extension)
        {
            return Err(A2aCall::invalid("A2A extension not activated"));
        }
        let credential = self
            .protocols
            .a2a_credential(&call.wire, self.config().max_body_bytes)?;
        if credential.as_ref().is_some_and(|(name, _)| name != &operation.protocol) {
            return Err(A2aCall::invalid("A2A credential protocol mismatch"));
        }
        self.protocols.a2a_strip_credentials(&mut call.wire)?;
        let message = call.params()?.message;
        let id = message.task_id.clone().unwrap_or_else(|| operation.run_id.clone());
        let mut task = if let Some(task) = store.get(&id).await? {
            task
        } else {
            if credential.is_some() || message.task_id.is_some() {
                return Err(A2aCall::invalid(
                    "A2A task must exist before credential or continuation",
                ));
            }
            Task {
                kind: "task".into(),
                id,
                context_id: message
                    .context_id
                    .clone()
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                status: TaskStatus {
                    state: TaskState::Submitted,
                    message: None,
                    timestamp: None,
                },
                history: vec![message],
                artifacts: vec![],
                metadata: None,
            }
        };
        let meta = task.metadata.get_or_insert_default();
        for (key, value) in
            A2aPaymentCall::SCOPE
                .into_iter()
                .zip([&operation.run_id, &operation.scenario_id, &operation.price_id])
        {
            if meta.get(key).is_some_and(|v| v != value) || (credential.is_some() && !meta.contains_key(key)) {
                return Err(A2aCall::invalid("A2A task belongs to another payment scope"));
            }
            meta.insert(key.into(), json!(value));
        }
        call.bind(&task)?;
        store.save(&task).await?;
        Ok(Some(A2aPaymentCall {
            call,
            task,
            credential: credential.map(|(_, v)| v),
            store,
        }))
    }
    pub async fn a2a_challenge(
        &self,
        call: &A2aPaymentCall,
        challenge: &PaymentChallenge,
        rejected: bool,
    ) -> Result<ProtocolMcpPaymentRequired> {
        let wire = self
            .protocols
            .get(&challenge.protocol)?
            .a2a_payment_required(challenge, &call.task, rejected)?;
        if let ProtocolMcpPaymentRequired::Result(result) = &wire {
            call.persist(A2aCall::task(&json!({"result":result}))?).await?;
        }
        Ok(wire)
    }
    pub async fn a2a_receipt(
        &self,
        call: &A2aPaymentCall,
        mut message: Value,
        challenge: &PaymentChallenge,
        payment: &SettledIncomingPayment,
    ) -> Result<Value> {
        A2aCall::response(&message, &call.call.wire["id"])?;
        if self
            .protocols
            .a2a_evidence(&message, self.config().max_body_bytes)?
            .is_some()
        {
            return Err(A2aCall::invalid("handler supplied payment receipt"));
        }
        if let Some(error) = message.as_object_mut().and_then(|m| m.remove("error")) {
            let mut task = serde_json::to_value(&call.task).map_err(|e| A2aCall::invalid(&e.to_string()))?;
            task["status"] = json!({"state":"failed","message":{"kind":"message","role":"agent","messageId":uuid::Uuid::new_v4().to_string(),"taskId":call.task.id,"contextId":call.task.context_id,"parts":[{"kind":"data","data":{"jsonrpc_error":error}}]}});
            message["result"] = task;
        }
        let mut bound = call.call.clone();
        bound.bind(&A2aCall::task(&message)?)?;
        self.protocols
            .get(&payment.receipt.protocol)?
            .a2a_inject_receipt(&mut message, challenge, payment)?;
        call.persist(A2aCall::task(&message)?).await?;
        Ok(message)
    }
}
#[cfg(test)]
#[path = "../../tests/payee/a2a_test.rs"]
mod tests;
