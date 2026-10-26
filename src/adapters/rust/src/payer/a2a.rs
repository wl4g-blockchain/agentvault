use crate::{
    A2aCall, AuthorizedPayment, PayerHttpRequest, PayerHttpResponse, PayerMiddleware, PayerRequestContext, Result,
    VaultTransport,
};
use serde_json::Value;
use std::collections::BTreeMap;

impl<T> PayerMiddleware<T> {
    #[must_use]
    pub fn a2a_extensions(&self, existing: &str) -> String {
        self.protocols.a2a_extensions(existing)
    }
    pub fn a2a_credential(
        &self,
        payment: &AuthorizedPayment,
        call: &mut A2aCall,
        challenge: &Value,
        origin: &str,
    ) -> Result<Vec<u8>> {
        let adapter = self.protocols.get(&payment.credential.protocol)?;
        if payment.intent.protocol != adapter.name() || adapter.credential(&payment.credential)?.bound_origin != origin
        {
            return Err(A2aCall::invalid("A2A credential origin/protocol mismatch"));
        }
        if self
            .protocols
            .a2a_credential(&call.wire, self.config().max_challenge_bytes)?
            .is_some()
        {
            return Err(A2aCall::invalid("A2A request already has credential"));
        }
        A2aCall::response(challenge, &call.wire["id"])?;
        if challenge.get("result").is_some() {
            call.bind(&A2aCall::task(challenge)?)?;
        }
        adapter.a2a_apply_credential(&mut call.wire, &payment.credential)?;
        call.body()
    }
}
impl<T: VaultTransport> PayerMiddleware<T> {
    pub async fn authorize_a2a(
        &self,
        request: &PayerHttpRequest,
        response: &PayerHttpResponse,
        context: &PayerRequestContext,
    ) -> Result<Option<AuthorizedPayment>> {
        let Some(call) = A2aCall::parse(&request.body, self.config().max_challenge_bytes)? else {
            return Ok(None);
        };
        let message: Value = serde_json::from_slice(&response.body).map_err(|e| A2aCall::invalid(&e.to_string()))?;
        A2aCall::response(&message, &call.wire["id"])?;
        let Some((adapter, challenge)) = self
            .protocols
            .a2a_challenge(&message, self.config().max_challenge_bytes)?
        else {
            return Ok(None);
        };
        if adapter.header(&response.headers, "payment-required").is_some()
            || adapter.header(&response.headers, "www-authenticate").is_some()
        {
            return Err(A2aCall::invalid("A2A challenge conflicts with HTTP authentication"));
        }
        let extension = adapter.a2a_extension();
        if !extension.is_empty()
            && !adapter
                .header(&response.headers, "x-a2a-extensions")
                .unwrap_or_default()
                .split(',')
                .any(|s| s.trim() == extension)
        {
            return Err(A2aCall::invalid("A2A extension was not acknowledged"));
        }
        let mut headers = challenge.headers;
        headers.insert("payment-protocol".into(), adapter.name().into());
        if let Some(expiry) = adapter.header(&response.headers, "payment-expires-at") {
            headers.insert("payment-expires-at".into(), expiry.into());
        }
        self.authorize(
            request,
            &PayerHttpResponse {
                status: 402,
                headers,
                body: challenge.body,
            },
            context,
        )
        .await
    }
    pub async fn report_a2a(
        &self,
        context: &PayerRequestContext,
        payment: &AuthorizedPayment,
        call: &mut A2aCall,
        response: &PayerHttpResponse,
    ) -> Result<()> {
        let message: Value = serde_json::from_slice(&response.body).map_err(|e| A2aCall::invalid(&e.to_string()))?;
        A2aCall::response(&message, &call.wire["id"])?;
        if message.get("result").is_some() {
            call.bind(&A2aCall::task(&message)?)?;
        }
        let Some((name, evidence)) = self
            .protocols
            .a2a_evidence(&message, self.config().max_challenge_bytes)?
        else {
            return Err(A2aCall::invalid("A2A receipt missing; payment remains pending"));
        };
        if name != payment.credential.protocol {
            return Err(A2aCall::invalid("A2A receipt protocol mismatch"));
        }
        if self
            .protocols
            .get(&name)?
            .header(&response.headers, &evidence.header_name)
            .is_some_and(|v| v != evidence.header_value)
        {
            return Err(A2aCall::invalid("A2A/HTTP receipt conflict"));
        }
        self.report(
            context,
            payment,
            &PayerHttpResponse {
                status: 200,
                headers: BTreeMap::from([(evidence.header_name, evidence.header_value)]),
                body: vec![],
            },
        )
        .await
    }
}
#[cfg(test)]
#[path = "../../tests/payer/a2a_test.rs"]
mod tests;
