//! Compatibility exports for the original wire-contract module.

pub use crate::model::sign::{
    validate_identifier, ClientPolicy, PaymentSignRequest as SignRequest, PaymentSignResponse as SignResponse,
    ProtocolError, EIP712_SECP256K1, PROTOCOL_VERSION,
};
