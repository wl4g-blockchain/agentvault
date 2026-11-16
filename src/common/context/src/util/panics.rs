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

use tracing::error;

pub struct PanicHelper {}

impl PanicHelper {
    /// set enterprise-level panic hook, for:
    /// 1. unified error format and logging
    /// 2. record to structured logging system (tracing)
    /// 3. format backtrace, remove noise information
    /// 4. provide clear error location information
    ///
    /// # Panics
    /// Panics if called from a panicking thread or if the static stack-trace filter cannot be compiled.
    pub fn set_hook_default(human_log_mode: bool) {
        // save default hook, in case it's needed to be called.
        // let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // extract panic message.
            let message = info
                .payload()
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| info.payload().downcast_ref::<String>().map(std::string::String::as_str))
                .unwrap_or("unknown panic");

            if let Some(location) = info.location() {
                let file_path = location.file();

                // only print the path relative to src/, not the full path.
                let re = regex::Regex::new(r".*/src/(.*)").unwrap();
                let relative_path = if let Some(captures) = re.captures(file_path) {
                    if let Some(path_match) = captures.get(1) {
                        format!("src/{}", path_match.as_str())
                    } else {
                        file_path.to_string()
                    }
                } else {
                    // if no match, return the file name.
                    std::path::Path::new(file_path)
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string()
                };

                // record to structured logging system (tracing).
                error!(
                    panic.message = %message,
                    panic.file = %relative_path,
                    panic.line = location.line(),
                    panic.column = location.column(),
                    "Panic occurred"
                );

                // print panic backtrace.
                let backtrace = std::backtrace::Backtrace::capture();
                match backtrace.status() {
                    std::backtrace::BacktraceStatus::Captured => {
                        // format backtrace, remove noise from standard library and closure.
                        let backtrace_str = format!("{backtrace}");
                        // get backtrace mode from env.
                        let backtrace_mode = std::env::var("RUST_BACKTRACE").unwrap_or_default().to_uppercase();
                        if backtrace_mode == "1" || backtrace_mode == "FULL" {
                            // record full backtrace to log (for debugging).
                            error!(backtrace = %backtrace_str, human = human_log_mode, "Full panic backtrace");
                        } else {
                            let filtered_backtrace = Self::filter_backtrace(&backtrace_str);
                            error!(backtrace = %filtered_backtrace, human = human_log_mode, "Filtered panic backtrace");
                        }
                    }
                    std::backtrace::BacktraceStatus::Disabled => {
                        error!(
                            human = human_log_mode,
                            "Panic backtrace disabled - set RUST_BACKTRACE=1 to enable"
                        );
                    }
                    std::backtrace::BacktraceStatus::Unsupported => {
                        error!(human = human_log_mode, "Panic backtrace unsupported on this platform");
                    }
                    _ => {
                        error!(human = human_log_mode, "Panic backtrace unknown status");
                    }
                }
            } else {
                error!(panic.message = %message, human = human_log_mode, "Panic occurred without location info");
            }

            // call default hook (keep standard behavior, like print to stderr).
            //default_hook(info);
        }));
    }

    /// filter backtrace, remove noise from standard library and closure, only keep user code.
    fn filter_backtrace(backtrace: &str) -> String {
        let lines: Vec<&str> = backtrace.lines().collect();
        let mut filtered = Vec::new();
        let mut skip_next = false;

        for line in lines {
            // skip standard library paths (rustc, std, core, alloc, etc.).
            if line.contains("/rustc/")
                || line.contains("/std/src/")
                || line.contains("/core/src/")
                || line.contains("/alloc/src/")
                || line.contains("__rust_")
                || line.contains("rust_begin_unwind")
                || line.contains("rust_panic")
            {
                continue;
            }

            // skip closure marked line (but keep its call location).
            if line.contains("{{closure}}") {
                skip_next = true;
                continue;
            }

            // if the previous line is a closure, skip this line (usually internal implementation).
            if skip_next {
                skip_next = false;
                // but keep the line containing the user code path.
                if line.contains("src/") && !line.contains("/target/") {
                    filtered.push(line);
                }
                continue;
            }

            // keep the line containing the user code.
            if line.contains("src/") && !line.contains("/target/") {
                filtered.push(line);
            } else if !line.trim().is_empty() && !line.starts_with("   ") {
                // keep the non-empty and not indented content (like stack frame number).
                filtered.push(line);
            }
        }

        if filtered.is_empty() {
            backtrace.to_string()
        } else {
            filtered.join("\n")
        }
    }
}
