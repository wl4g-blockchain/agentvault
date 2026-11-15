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

// TODO: Should be stable API after issue-58520 is resolved in the future.
// refer to: https://github.com/wl4g/sigbot/blob/main/common/telemetry/src/tracing_sampler.rs#L59
// #![feature(let_chains)]
pub mod logging;
mod macros;
pub mod metric;
mod panic_hook;
#[cfg(feature = "profiling-pyroscope")]
pub mod pyroscope_agent;
pub mod readiness;
pub mod tracing_context;
mod tracing_sampler;

pub use logging::init_global_logging;

pub use common_error;
pub use logging::RELOAD_HANDLE;
pub use panic_hook::set_panic_hook;
pub use tracing;
pub use tracing_subscriber;
