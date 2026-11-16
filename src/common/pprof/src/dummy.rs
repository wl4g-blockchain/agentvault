// Copyright 2023 SigBot Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use crate::error::Result;

use crate::error::UnsupportedSnafu;
use std::time::Duration;

/// Dummpy CPU profiler utility.
#[derive(Debug)]
pub struct CPUProfiling {}

impl CPUProfiling {
    /// Creates a new profiler.
    pub fn new(_duration: Duration, _frequency: i32) -> CPUProfiling {
        CPUProfiling {}
    }

    /// Profiles and returns a generated text.
    pub async fn dump_text(&self) -> Result<String> {
        UnsupportedSnafu {}.fail()
    }

    /// Profiles and returns a generated flamegraph.
    pub async fn dump_flamegraph(&self) -> Result<Vec<u8>> {
        UnsupportedSnafu {}.fail()
    }

    /// Profiles and returns a generated proto.
    pub async fn dump_proto(&self) -> Result<Vec<u8>> {
        UnsupportedSnafu {}.fail()
    }
}
