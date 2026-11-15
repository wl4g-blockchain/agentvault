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

use crate::status_code::StatusCode;
use snafu;
use std::any::Any;
use std::sync::Arc;

/// Extension to [`Error`](std::error::Error) in std.
pub trait ErrorExt: StackError {
    /// Map this error to [`StatusCode`].
    fn status_code(&self) -> StatusCode {
        StatusCode::Unknown
    }

    // TODO(ruihang): remove this default implementation
    /// Get the location of this error, None if the location is unavailable.
    /// Add `_opt` suffix to avoid confusing with similar method in `std::error::Error`
    fn location_opt(&self) -> Option<snafu::Location> {
        None
    }

    /// Returns the error as [Any](std::any::Any) so that it can be
    /// downcast to a specific implementation.
    fn as_any(&self) -> &dyn Any;

    fn output_msg(&self) -> String
    where
        Self: Sized,
    {
        match self.status_code() {
            StatusCode::Unknown | StatusCode::Internal => {
                // masks internal error from end user
                format!("Internal error: {}", self.status_code() as u32)
            }
            _ => {
                let error = self.last();
                if let Some(external_error) = error.source() {
                    // TODO: Should be stable API after issue-58520 is resolved in the future.
                    // refer to: https://github.com/GreptimeTeam/greptimedb/blob/v0.13.1/src/common/error/src/ext.rs#L43
                    //let external_root = external_error.sources().last().unwrap(); // currently rustc-1.85 is unstable api.
                    let external_root = external_error.source().unwrap();

                    if error.to_string().is_empty() {
                        format!("{external_root}")
                    } else {
                        format!("{error}: {external_root}")
                    }
                } else {
                    format!("{error}")
                }
            }
        }
    }
}

pub trait StackError: std::error::Error {
    fn debug_fmt(&self, layer: usize, buf: &mut Vec<String>);

    fn next(&self) -> Option<&dyn StackError>;

    fn last(&self) -> &dyn StackError
    where
        Self: Sized,
    {
        let Some(mut result) = self.next() else {
            return self;
        };
        while let Some(err) = result.next() {
            result = err;
        }
        result
    }
}

impl<T: ?Sized + StackError> StackError for Arc<T> {
    fn debug_fmt(&self, layer: usize, buf: &mut Vec<String>) {
        self.as_ref().debug_fmt(layer, buf);
    }

    fn next(&self) -> Option<&dyn StackError> {
        self.as_ref().next()
    }
}

impl<T: StackError> StackError for Box<T> {
    fn debug_fmt(&self, layer: usize, buf: &mut Vec<String>) {
        self.as_ref().debug_fmt(layer, buf);
    }

    fn next(&self) -> Option<&dyn StackError> {
        self.as_ref().next()
    }
}

/// An opaque boxed error based on errors that implement [`ErrorExt`] trait.
pub struct BoxedError {
    inner: Box<dyn crate::ext::ErrorExt + Send + Sync>,
}

impl BoxedError {
    pub fn new<E: crate::ext::ErrorExt + Send + Sync + 'static>(err: E) -> Self {
        Self { inner: Box::new(err) }
    }
}

impl std::fmt::Debug for BoxedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Use the pretty debug format of inner error for opaque error.
        let debug_format = crate::format::DebugFormat::new(&*self.inner);
        debug_format.fmt(f)
    }
}

impl std::fmt::Display for BoxedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.inner)
    }
}

impl std::error::Error for BoxedError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.inner.source()
    }
}

impl crate::ext::ErrorExt for BoxedError {
    fn status_code(&self) -> crate::status_code::StatusCode {
        self.inner.status_code()
    }

    fn location_opt(&self) -> Option<snafu::Location> {
        self.inner.location_opt()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self.inner.as_any()
    }
}

// Implement ErrorCompat for this opaque error so the backtrace is also available
// via `ErrorCompat::backtrace()`.
impl snafu::ErrorCompat for BoxedError {
    fn backtrace(&self) -> Option<&snafu::Backtrace> {
        None
    }
}

impl StackError for BoxedError {
    fn debug_fmt(&self, layer: usize, buf: &mut Vec<String>) {
        self.inner.debug_fmt(layer, buf);
    }

    fn next(&self) -> Option<&dyn StackError> {
        self.inner.next()
    }
}

/// Error type with plain error message
#[derive(Debug)]
pub struct PlainError {
    msg: String,
    status_code: StatusCode,
}

impl PlainError {
    #[must_use]
    pub fn new(msg: String, status_code: StatusCode) -> Self {
        Self { msg, status_code }
    }
}

impl std::fmt::Display for PlainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.msg)
    }
}

impl std::error::Error for PlainError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        None
    }
}

impl crate::ext::ErrorExt for PlainError {
    fn status_code(&self) -> crate::status_code::StatusCode {
        self.status_code
    }

    fn location_opt(&self) -> Option<snafu::Location> {
        None
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self as _
    }
}

impl StackError for PlainError {
    fn debug_fmt(&self, layer: usize, buf: &mut Vec<String>) {
        buf.push(format!("{}: {}", layer, self.msg));
    }

    fn next(&self) -> Option<&dyn StackError> {
        None
    }
}
