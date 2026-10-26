use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    net::IpAddr,
    str::FromStr as _,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Context as _};
use async_trait::async_trait;
use axum::http::{HeaderMap, HeaderValue};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rumqttc::{AsyncClient, MqttOptions, QoS, SubAck, SubscribeReasonCode, Transport};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use tokio::{
    sync::{Mutex, OwnedSemaphorePermit, Semaphore},
    task::JoinSet,
};
use zeroize::Zeroizing;

use crate::apm::set_span_parent_from_headers;
use crate::cache::ICache;
use crate::cmd::Shutdown;
use crate::config::MqttTransportConfig;
use crate::model::payment_api::AuthorizePaymentRequest;
use crate::model::payment_api::CreateChallengeRequest;
use crate::model::payment_api::ReportOutboundResponseRequest;
use crate::model::payment_api::ResolvePriceRequest;
use crate::model::payment_api::SettlePaymentRequest;
use crate::model::payment_api::SubmitApprovalDecisionRequest;
use crate::model::payment_api::VerifyPaymentRequest;
use crate::model::PaymentEventDelivery;
use crate::payment::PaymentApi;
use crate::payment::PaymentEventDispatcherConfig;
use crate::payment::PaymentEventSink;
use crate::storage::ClientStorage;
use crate::storage::OutboxDeliveryClass;
use crate::storage::OutboxLeaseRequest;
use crate::storage::OutboxMessage;
use crate::storage::OutboxStorage;
use crate::storage::ScenarioStorage;
use crate::Error;

mod command;
mod connection;
mod delivery;
mod state;
mod wire;
use crate::model::mqtt::{
    ApprovalDecisionCommand, ApprovalRequiredEnvelope, OutboundResponseCommand, PaymentEventEnvelope, RequestEnvelope,
    ResponseEnvelope,
};
pub(crate) use command::*;
pub(crate) use connection::*;
pub(crate) use delivery::*;
pub(crate) use state::*;
pub(crate) use wire::*;
pub(crate) const AUTHORIZE_PATH: &str = "/api/v1/payments/authorize";

pub(crate) const COMMAND_SUFFIX: &str = "/commands/authorize";

pub(crate) const OUTBOUND_RESPONSE_SUFFIX: &str = "/commands/outbound-response";

pub(crate) const RESOLVE_OFFER_SUFFIX: &str = "/commands/resolve-offer";

pub(crate) const CREATE_CHALLENGE_SUFFIX: &str = "/commands/create-challenge";

pub(crate) const VERIFY_PAYMENT_SUFFIX: &str = "/commands/verify-payment";

pub(crate) const SETTLE_PAYMENT_SUFFIX: &str = "/commands/settle-payment";

pub(crate) const APPROVER_SEGMENT: &str = "approvers";

pub(crate) const DECISION_SEGMENT: &str = "decisions";

pub(crate) const MAX_ENVELOPE_BYTES: usize = 2 * 1024 * 1024;

pub(crate) const MAX_BODY_BYTES: usize = 1024 * 1024;

pub(crate) const MAX_HEADER_COUNT: usize = 16;

pub(crate) const MAX_HEADER_NAME_BYTES: usize = 64;

pub(crate) const MAX_HEADER_VALUE_BYTES: usize = 8 * 1024;

pub(crate) const MAX_IN_FLIGHT: usize = 128;

pub(crate) const MAX_IN_FLIGHT_PER_CLIENT: usize = 16;

pub(crate) const RESPONSE_CACHE_TTL: Duration = Duration::from_mins(5);

pub(crate) const OUTBOX_BATCH_SIZE: u32 = 64;

pub(crate) const OUTBOX_LEASE_TTL: Duration = Duration::from_secs(30);

pub(crate) const OUTBOX_POLL_INTERVAL: Duration = Duration::from_millis(250);

pub(crate) const MAX_SECRET_BYTES: usize = 16 * 1024;
#[cfg(test)]
#[path = "../../../tests/transport/mqtt/mod_test.rs"]
mod tests;
