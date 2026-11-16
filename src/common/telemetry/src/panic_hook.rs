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

use std::panic;
#[cfg(feature = "deadlock_detection")]
use std::time::Duration;

use backtrace::Backtrace;
use prometheus::{register_int_counter, IntCounter};
use std::sync::LazyLock;

pub static PANIC_COUNTER: LazyLock<IntCounter> = LazyLock::new(|| {
    register_int_counter!("common_panic_counter", "panic_counter").expect("panic metric must register once")
});

pub fn set_panic_hook() {
    // Set a panic hook that records the panic as a `tracing` event at the
    // `ERROR` verbosity level.
    //
    // If we are currently in a span when the panic occurred, the logged event
    // will include the current span, allowing the context in which the panic
    // occurred to be recorded.
    let default_hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic| {
        let backtrace = Backtrace::new();
        let backtrace = format!("{backtrace:?}");
        if let Some(location) = panic.location() {
            tracing::error!(
                message = %panic,
                backtrace = %backtrace,
                panic.file = location.file(),
                panic.line = location.line(),
                panic.column = location.column(),
            );
        } else {
            tracing::error!(message = %panic, backtrace = %backtrace);
        }
        PANIC_COUNTER.inc();
        default_hook(panic);
    }));

    #[cfg(feature = "deadlock_detection")]
    let _ = std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(5));
        let deadlocks = parking_lot::deadlock::check_deadlock();
        if deadlocks.is_empty() {
            continue;
        }

        tracing::info!("{} deadlocks detected", deadlocks.len());
        for (i, threads) in deadlocks.iter().enumerate() {
            tracing::info!("Deadlock #{}", i);
            for t in threads {
                tracing::info!("Thread Id {:#?}", t.thread_id());
                tracing::info!("{:#?}", t.backtrace());
            }
        }
    });
}
