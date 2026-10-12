use std::{path::PathBuf, time::Duration};

use async_trait::async_trait;
use tokio::{
    io::{AsyncWriteExt as _, BufReader},
    net::UnixStream,
    time::timeout,
};

use crate::{
    framing::read_bounded_line,
    model::sign::{PaymentSignRequest, PaymentSignResponse},
    signer::IPaymentSigner,
    Error, Result,
};

/// Unix-socket implementation suitable for an agent colocated with `walletd`.
#[derive(Clone, Debug)]
pub struct LocalPaymentSigner {
    socket_path: PathBuf,
    timeout: Duration,
    max_response_bytes: usize,
}

impl LocalPaymentSigner {
    #[must_use]
    pub fn new(socket_path: impl Into<PathBuf>, timeout: Duration, max_response_bytes: usize) -> Self {
        Self {
            socket_path: socket_path.into(),
            timeout,
            max_response_bytes,
        }
    }

    async fn execute_inner(&self, request: &PaymentSignRequest) -> Result<PaymentSignResponse> {
        let mut stream = UnixStream::connect(&self.socket_path).await?;
        let mut encoded = serde_json::to_vec(request)?;
        encoded.push(b'\n');
        stream.write_all(&encoded).await?;
        stream.shutdown().await?;

        let mut reader = BufReader::new(stream);
        let response = read_bounded_line(&mut reader, self.max_response_bytes, "local signer response").await?;
        if response.is_empty() {
            return Err(Error::InvalidRequest(
                "local signer returned an empty response".to_owned(),
            ));
        }
        let response: PaymentSignResponse = serde_json::from_slice(&response)?;
        if response.request_id != request.request_id {
            return Err(Error::InvalidRequest(
                "local signer response request_id differs".to_owned(),
            ));
        }
        Ok(response)
    }
}

#[async_trait]
impl IPaymentSigner for LocalPaymentSigner {
    async fn execute(&self, request: PaymentSignRequest) -> Result<PaymentSignResponse> {
        timeout(self.timeout, self.execute_inner(&request))
            .await
            .map_err(|_| Error::Timeout("local payment signer".to_owned()))?
    }
}
