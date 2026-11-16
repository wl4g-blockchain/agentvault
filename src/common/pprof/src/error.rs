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

use common_error::ext::ErrorExt;
use common_error::status_code::StatusCode;
use common_macro::stack_trace_debug;
use snafu::Location;
use snafu::Snafu;
use std::any::Any;

#[derive(Snafu)]
#[stack_trace_debug]
#[snafu(visibility(pub(crate)))]
pub enum Error {
    #[cfg(unix)]
    #[snafu(display("Pprof error"))]
    Pprof {
        #[snafu(source)]
        error: pprof::Error,
        #[snafu(implicit)]
        location: Location,
    },

    #[snafu(display("Pprof is unsupported on this platform"))]
    Unsupported {
        #[snafu(implicit)]
        location: Location,
    },
}

pub type Result<T> = std::result::Result<T, Error>;

impl ErrorExt for Error {
    fn status_code(&self) -> StatusCode {
        match self {
            #[cfg(unix)]
            Error::Pprof { .. } => StatusCode::Unexpected,
            Error::Unsupported { .. } => StatusCode::Unsupported,
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
