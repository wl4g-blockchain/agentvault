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

use std::error::Error;
use std::fmt;

#[derive(Debug)]
pub enum MemCopyError {
    DestTooSmall { dest_len: usize, src_len: usize },
    NullPointer,
}

impl fmt::Display for MemCopyError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            MemCopyError::DestTooSmall { dest_len, src_len } => write!(
                f,
                "Destination buffer too small. Dest len: {dest_len}, Src len: {src_len}"
            ),
            MemCopyError::NullPointer => write!(f, "Null pointer encountered"),
        }
    }
}

impl Error for MemCopyError {}

pub struct MemCopy;

impl MemCopy {
    ///
    /// # Errors
    /// Returns `MemCopyError::DestTooSmall` when the destination cannot hold the source bytes.
    pub fn fast_copy(dest: &mut [u8], src: &[u8]) -> Result<(), MemCopyError> {
        if dest.len() < src.len() {
            return Err(MemCopyError::DestTooSmall {
                dest_len: dest.len(),
                src_len: src.len(),
            });
        }
        dest[..src.len()].copy_from_slice(src);
        Ok(())
    }

    ///
    /// # Errors
    /// Returns `MemCopyError::DestTooSmall` when the destination cannot hold the source bytes.
    pub fn safe_copy(dest: &mut [u8], src: &[u8]) -> Result<(), MemCopyError> {
        if dest.len() < src.len() {
            return Err(MemCopyError::DestTooSmall {
                dest_len: dest.len(),
                src_len: src.len(),
            });
        }
        dest[..src.len()].copy_from_slice(src);
        Ok(())
    }

    // Copy part of the content, copy as much as possible
    pub fn partial_copy(dest: &mut [u8], src: &[u8]) -> usize {
        let copy_len = dest.len().min(src.len());
        dest[..copy_len].copy_from_slice(&src[..copy_len]);
        copy_len
    }
}
