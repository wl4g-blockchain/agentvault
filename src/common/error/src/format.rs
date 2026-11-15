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

use crate::ext::ErrorExt;

/// Pretty debug format for error, also prints source and backtrace.
pub struct DebugFormat<'a, E: ?Sized>(&'a E);

impl<'a, E: ?Sized> DebugFormat<'a, E> {
    /// Create a new format struct from `err`.
    pub fn new(err: &'a E) -> Self {
        Self(err)
    }
}

impl<E: ErrorExt + ?Sized> fmt::Debug for DebugFormat<'_, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.", self.0)?;
        if let Some(source) = self.0.source() {
            // Source error use debug format for more verbose info.
            write!(f, " Caused by: {source:?}")?;
        }
        if let Some(location) = self.0.location_opt() {
            // Add a newline to separate causes and backtrace.
            write!(f, " at: {location}")?;
        }

        Ok(())
    }
}

#[cfg(test)]
#[path = "../tests/unit/format_test.rs"]
mod tests;
