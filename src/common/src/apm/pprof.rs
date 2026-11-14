//! Lightweight, allocation-bounded runtime diagnostics for the private
//! management listener. This mirrors `AuthGuard`'s process snapshot without
//! embedding a sampling profiler in the payment data plane.

use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeProfile {
    pub pid: u32,
    pub logical_cpus: usize,
    pub user_cpu_ticks: Option<u64>,
    pub system_cpu_ticks: Option<u64>,
    pub resident_memory_kib: Option<u64>,
    pub virtual_memory_kib: Option<u64>,
    pub threads: Option<u64>,
}

#[must_use]
pub fn runtime_snapshot() -> RuntimeProfile {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let (user_cpu_ticks, system_cpu_ticks) = cpu_ticks(&stat);
    RuntimeProfile {
        pid: std::process::id(),
        logical_cpus: std::thread::available_parallelism().map_or(1, usize::from),
        user_cpu_ticks,
        system_cpu_ticks,
        resident_memory_kib: status_value(&status, "VmRSS:"),
        virtual_memory_kib: status_value(&status, "VmSize:"),
        threads: status_value(&status, "Threads:"),
    }
}

fn cpu_ticks(stat: &str) -> (Option<u64>, Option<u64>) {
    let fields = stat.rsplit_once(") ").map_or(stat, |(_, fields)| fields);
    let fields = fields.split_whitespace().collect::<Vec<_>>();
    (
        fields.get(11).and_then(|value| value.parse().ok()),
        fields.get(12).and_then(|value| value.parse().ok()),
    )
}

fn status_value(status: &str, key: &str) -> Option<u64> {
    status
        .lines()
        .find_map(|line| line.strip_prefix(key)?.split_whitespace().next()?.parse().ok())
}

#[cfg(test)]
#[path = "../../tests/apm/pprof_test.rs"]
mod tests;
