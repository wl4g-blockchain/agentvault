use uuid::Uuid;

pub(crate) struct RuntimeIdentity;

const MAX_RUNTIME_ID_BYTES: usize = 128;

impl RuntimeIdentity {
    /// Creates a process-instance identity suitable for SQL lease ownership and
    /// transport client identifiers. The random suffix prevents two replicas
    /// with the same operator configuration from sharing one lease identity.
    pub(crate) fn unique(prefix: &str) -> String {
        let suffix = Uuid::new_v4().simple().to_string();
        let max_prefix_bytes = MAX_RUNTIME_ID_BYTES.saturating_sub(suffix.len() + 1);
        let trimmed = prefix.trim();
        let mut end = trimmed.len().min(max_prefix_bytes);
        while !trimmed.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        let prefix = &trimmed[..end];
        if prefix.is_empty() {
            suffix
        } else {
            format!("{prefix}-{suffix}")
        }
    }
}

#[cfg(test)]
#[path = "../../tests/utils/runtime_identity_test.rs"]
mod tests;
