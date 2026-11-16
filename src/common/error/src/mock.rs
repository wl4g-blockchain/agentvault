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

//! Utils for mock.

use std::any::Any;
use std::fmt;

use snafu::Location;

use crate::ext::ErrorExt;

use crate::ext::StackError;
use crate::status_code::StatusCode;

/// A mock error mainly for test.
#[derive(Debug)]
pub struct MockError {
    pub code: StatusCode,
    source: Option<Box<MockError>>,
}

impl MockError {
    /// Create a new [`MockError`] without backtrace.
    #[must_use]
    pub fn new(code: StatusCode) -> MockError {
        MockError { code, source: None }
    }

    /// Create a new [`MockError`] with source.
    #[must_use]
    pub fn with_source(source: MockError) -> MockError {
        MockError {
            code: source.code,
            source: Some(Box::new(source)),
        }
    }
}

impl fmt::Display for MockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code)
    }
}

impl std::error::Error for MockError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_ref().map(|e| e as _)
    }
}

impl ErrorExt for MockError {
    fn status_code(&self) -> StatusCode {
        self.code
    }

    fn location_opt(&self) -> Option<Location> {
        None
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl StackError for MockError {
    fn debug_fmt(&self, _: usize, _: &mut Vec<String>) {}

    fn next(&self) -> Option<&dyn StackError> {
        None
    }
}
