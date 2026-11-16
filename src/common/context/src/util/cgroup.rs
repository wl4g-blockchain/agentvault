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

// Thanks based on: https://github.com/wl4g-collect/openobserve/blob/v0.10.9/src/config/src/utils/cgroup.rs
use sysinfo::SystemExt;

/// Reads process resource limits from cgroup v1/v2 with host fallbacks.
pub struct Cgroup;

impl Cgroup {
    #[must_use]
    pub fn cpu_limit() -> usize {
        let mut cpu_num = Self::read_cpu_cgroup_v2();
        if cpu_num == 0 {
            cpu_num = Self::read_cpu_cgroup_v1();
        }
        if cpu_num > 0 {
            if cpu_num < 100_000 {
                1
            } else {
                cpu_num / 100_000
            }
        } else {
            let mut system = sysinfo::System::new();
            system.refresh_cpu();
            system.cpus().len()
        }
    }

    #[must_use]
    pub fn memory_limit() -> usize {
        let mut mem_size = Self::read_memory_cgroup_v2();
        if mem_size == 0 {
            mem_size = Self::read_memory_cgroup_v1();
        }
        let node_mem_size = {
            let mut system = sysinfo::System::new();
            system.refresh_memory();
            usize::try_from(system.total_memory()).unwrap_or(usize::MAX)
        };
        if mem_size == 0 || mem_size > node_mem_size {
            node_mem_size
        } else {
            mem_size
        }
    }

    fn read_cpu_cgroup_v2() -> usize {
        if let Ok(value) = std::fs::read_to_string("/sys/fs/cgroup/cpu.max") {
            if !value.is_empty() && !value.to_ascii_lowercase().starts_with("max") {
                return value
                    .split_whitespace()
                    .next()
                    .and_then(|quota| quota.parse::<usize>().ok())
                    .unwrap_or_default();
            }
        }
        0
    }

    fn read_memory_cgroup_v2() -> usize {
        if let Ok(value) = std::fs::read_to_string("/sys/fs/cgroup/memory.max") {
            if !value.is_empty() && !value.to_ascii_lowercase().starts_with("max") {
                return value.trim().parse::<usize>().unwrap_or_default();
            }
        }
        0
    }

    fn read_cpu_cgroup_v1() -> usize {
        std::fs::read_to_string("/sys/fs/cgroup/cpu/cpu.cfs_quota_us")
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or_default()
    }

    fn read_memory_cgroup_v1() -> usize {
        std::fs::read_to_string("/sys/fs/cgroup/memory/memory.limit_in_bytes")
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or_default()
    }
}
