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

/// The standard logging macro.
#[macro_export]
macro_rules! log {
    // log!(target: "my_target", Level::INFO, "a {} event", "log");
    (
        target: $target:expr,
        $lvl:expr,
        $($arg:tt)+
    ) => {
        {
        $crate::tracing::event!(target: $target, $lvl, $($arg)+)
        }
    };

    // log!(Level::INFO, "a log event")
    (
        $lvl:expr,
        $($arg:tt)+
    ) => {
        {
        $crate::tracing::event!($lvl, $($arg)+)
        }
    };
}

/// Logs a message at the error level.
#[macro_export]
macro_rules! error {
    // error!(target: "my_target", "a {} event", "log")
    (
        target: $target:expr,
        $($arg:tt)+
    ) => (
        {
        $crate::log!(target: $target, $crate::tracing::Level::ERROR, $($arg)+)
        }
    );

    // error!(e; target: "my_target", "a {} event", "log")
    (
        $e:expr;
        target: $target:expr,
        $($arg:tt)+
    ) => (
        {
        $crate::log!(
            target: $target,
            $crate::tracing::Level::ERROR,
            err = ?$e,
            $($arg)+
        )
        }
    );

    // error!(%e; target: "my_target", "a {} event", "log")
    (
        % $e:expr;
        target: $target:expr,
        $($arg:tt)+
    ) => (
        {
        $crate::log!(
            target: $target,
            $crate::tracing::Level::ERROR,
            err = %$e,
            $($arg)+
        )
        }
    );

    // error!(e; "a {} event", "log")
    (
        $e:expr;
        $($arg:tt)+
    ) => (
        {
        $crate::log!(
            $crate::tracing::Level::ERROR,
            err = ?$e,
            $($arg)+
        )
        }
    );

    // error!(%e; "a {} event", "log")
    (
        % $e:expr;
        $($arg:tt)+
    ) => (
        {
        $crate::log!(
            $crate::tracing::Level::ERROR,
            err = %$e,
            $($arg)+
        )
        }
    );

    // error!("a {} event", "log")
    ($($arg:tt)+) => (
        {
        $crate::log!($crate::tracing::Level::ERROR, $($arg)+)
        }
    );
}

/// Logs a message at the warn level.
#[macro_export]
macro_rules! warn {
    // warn!(target: "my_target", "a {} event", "log")
    (
        target: $target:expr,
        $($arg:tt)+
    ) => {
        $crate::log!(target: $target, $crate::tracing::Level::WARN, $($arg)+)
    };

    // warn!(e; "a {} event", "log")
    (
        $e:expr;
        $($arg:tt)+
    ) => (
        {
        $crate::log!(
            $crate::tracing::Level::WARN,
            err = ?$e,
            $($arg)+
        )
        }
    );

    // warn!(%e; "a {} event", "log")
    (
        % $e:expr;
        $($arg:tt)+
    ) => (
        {
        $crate::log!(
            $crate::tracing::Level::WARN,
            err = %$e,
            $($arg)+
        )
        }
    );

    // warn!("a {} event", "log")
    ($($arg:tt)+) => {
        $crate::log!($crate::tracing::Level::WARN, $($arg)+)
    };
}

/// Logs a message at the info level.
#[macro_export]
macro_rules! info {
    // info!(target: "my_target", "a {} event", "log")
    (
        target: $target:expr,
        $($arg:tt)+
    ) => {
        $crate::log!(target: $target, $crate::tracing::Level::INFO, $($arg)+)
    };

    // info!("a {} event", "log")
    ($($arg:tt)+) => {
        $crate::log!($crate::tracing::Level::INFO, $($arg)+)
    };
}

/// Logs a message at the debug level.
#[macro_export]
macro_rules! debug {
    // debug!(target: "my_target", "a {} event", "log")
    (
        target: $target:expr,
        $($arg:tt)+
    ) => {
        $crate::log!(target: $target, $crate::tracing::Level::DEBUG, $($arg)+)
    };

    // debug!("a {} event", "log")
    ($($arg:tt)+) => {
        $crate::log!($crate::tracing::Level::DEBUG, $($arg)+)
    };
}

/// Logs a message at the trace level.
#[macro_export]
macro_rules! trace {
    // trace!(target: "my_target", "a {} event", "log")
    (
        target: $target:expr,
        $($arg:tt)+
    ) => {
        $crate::log!(target: $target, $crate::tracing::Level::TRACE, $($arg)+)
    };

    // trace!("a {} event", "log")
    ($($arg:tt)+) => {
        $crate::log!($crate::tracing::Level::TRACE, $($arg)+)
    };
}

#[cfg(test)]
#[path = "../tests/unit/macros_test.rs"]
mod tests;
