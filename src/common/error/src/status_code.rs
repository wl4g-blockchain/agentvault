// SPDX-License-Identifier: GNU GENERAL PUBLIC LICENSE Version 3
//
// Copyright (c) 2027 James Wong. This file is part of James Wong.
// is free software: you can redistribute it and/or modify it under
// the terms of the GNU General Public License as published by the
// Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// James Wong is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with James Wong.  If not, see <https://www.gnu.org/licenses/>.
//
// IMPORTANT: Any software that fully or partially contains or uses materials
// covered by this license must also be released under the GNU GPL license.
// This includes modifications and derived works.

use std::fmt;
use strum::AsRefStr;
use strum::EnumIter;
use strum::EnumString;
use strum::FromRepr;

/// Common status code for public API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, AsRefStr, EnumIter, FromRepr)]
pub enum StatusCode {
    // ====== Begin of common status code ==============
    /// Success.
    Success = 0,

    /// Unknown error.
    Unknown = 1000,
    /// Unsupported operation.
    Unsupported = 1001,
    /// Unexpected error, maybe there is a BUG.
    Unexpected = 1002,
    /// Internal server error.
    Internal = 1003,
    /// Invalid arguments.
    InvalidArguments = 1004,
    /// The task is cancelled.
    Cancelled = 1005,
    // ====== End of common status code ================

    // ====== Begin of SQL related status code =========
    /// SQL Syntax error.
    InvalidSyntax = 2000,
    // ====== End of SQL related status code ===========

    // ====== Begin of query related status code =======
    /// Fail to create a plan for the query.
    PlanQuery = 3000,
    /// The query engine fail to execute query.
    EngineExecuteQuery = 3001,
    // ====== End of query related status code =========

    // ====== Begin of storage related status code =====
    /// Storage is temporarily unable to handle the request
    StorageUnavailable = 5000,
    /// Request is outdated, e.g., version mismatch
    RequestOutdated = 5001,
    // ====== End of storage related status code =======

    // ====== Begin of server related status code =====
    /// Runtime resources exhausted, like creating threads failed.
    RuntimeResourcesExhausted = 6000,

    /// Rate limit exceeded
    RateLimited = 6001,
    // ====== End of server related status code =======

    // ====== Begin of auth related status code =====
    /// User not exist
    UserNotFound = 7000,
    /// Unsupported password type
    UnsupportedPasswordType = 7001,
    /// Username and password does not match
    UserPasswordMismatch = 7002,
    /// Not found http authorization header
    AuthHeaderNotFound = 7003,
    /// Invalid http authorization header
    InvalidAuthHeader = 7004,
    /// Illegal request to connect catalog-schema
    AccessDenied = 7005,
    /// User is not authorized to perform the operation
    PermissionDenied = 7006,
    // ====== End of auth related status code =====
}

impl StatusCode {
    /// Returns `true` if `code` is success.
    #[must_use]
    pub fn is_success(code: u32) -> bool {
        Self::Success as u32 == code
    }

    /// Returns `true` if the error with this code is retryable.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            StatusCode::StorageUnavailable
            | StatusCode::RuntimeResourcesExhausted
            | StatusCode::Internal
            | StatusCode::Success
            | StatusCode::Unknown
            | StatusCode::Unsupported
            | StatusCode::Unexpected
            | StatusCode::InvalidArguments
            | StatusCode::Cancelled
            | StatusCode::InvalidSyntax
            | StatusCode::PlanQuery
            | StatusCode::EngineExecuteQuery
            | StatusCode::RateLimited
            | StatusCode::UserNotFound
            | StatusCode::UnsupportedPasswordType
            | StatusCode::UserPasswordMismatch
            | StatusCode::AuthHeaderNotFound
            | StatusCode::InvalidAuthHeader
            | StatusCode::AccessDenied
            | StatusCode::PermissionDenied
            | StatusCode::RequestOutdated => false,
        }
    }

    /// Returns `true` if we should print an error log for an error with
    /// this status code.
    #[must_use]
    pub fn should_log_error(&self) -> bool {
        match self {
            StatusCode::Unknown
            | StatusCode::Unexpected
            | StatusCode::Internal
            | StatusCode::Cancelled
            | StatusCode::PlanQuery
            | StatusCode::EngineExecuteQuery
            | StatusCode::StorageUnavailable
            | StatusCode::RuntimeResourcesExhausted => true,
            StatusCode::Success
            | StatusCode::Unsupported
            | StatusCode::InvalidArguments
            | StatusCode::InvalidSyntax
            | StatusCode::RateLimited
            | StatusCode::UserNotFound
            | StatusCode::UnsupportedPasswordType
            | StatusCode::UserPasswordMismatch
            | StatusCode::AuthHeaderNotFound
            | StatusCode::InvalidAuthHeader
            | StatusCode::AccessDenied
            | StatusCode::PermissionDenied
            | StatusCode::RequestOutdated => false,
        }
    }

    #[must_use]
    pub fn from_u32(value: u32) -> Option<Self> {
        StatusCode::from_repr(value as usize)
    }
}

impl fmt::Display for StatusCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The current debug format is suitable to display.
        write!(f, "{self:?}")
    }
}

#[cfg(test)]
#[path = "../tests/unit/status_code_test.rs"]
mod tests;
