use std::{
    collections::VecDeque,
    fs::{self, File, OpenOptions},
    io::{BufRead as _, BufReader, Write as _},
    os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
    sync::{Mutex, RwLock},
};

use uuid::Uuid;

use crate::{
    model::sign::{validate_identifier, PaymentSignHistoryPage, PaymentSignRecord, SignHistoryQuery},
    secret::ensure_private_dir,
    Error, Result,
};

const MAX_RECORD_BYTES: usize = 16 * 1024;
const DEFAULT_QUERY_LIMIT: usize = 100;
const MAX_QUERY_LIMIT: usize = 500;

/// Append-only, process-local owner of durable signing audit metadata.
pub struct SignHistoryStore {
    path: PathBuf,
    max_entries: usize,
    records: RwLock<VecDeque<PaymentSignRecord>>,
    writer: Mutex<File>,
}

impl SignHistoryStore {
    /// Opens or creates an owner-only JSON-lines history file.
    ///
    /// # Errors
    ///
    /// Returns an error for unsafe paths, malformed records, or I/O failures.
    pub fn open(path: impl AsRef<Path>, max_entries: usize) -> Result<Self> {
        if max_entries == 0 {
            return Err(Error::Config(
                "sign history max_entries must be greater than zero".to_owned(),
            ));
        }
        let path = path.as_ref().to_path_buf();
        let parent = path
            .parent()
            .ok_or_else(|| Error::Config(format!("history path has no parent: {}", path.display())))?;
        ensure_private_dir(parent, "sign history directory")?;
        reject_unsafe_existing_path(&path)?;
        let writer = open_append(&path)?;
        writer
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|source| Error::WriteFile {
                path: path.clone(),
                source,
            })?;

        let mut records = load_records(&path)?;
        while records.len() > max_entries {
            records.pop_front();
        }
        if records.len() != count_lines(&path)? {
            rewrite(&path, &records)?;
        }
        let writer = open_append(&path)?;
        Ok(Self {
            path,
            max_entries,
            records: RwLock::new(records),
            writer: Mutex::new(writer),
        })
    }

    /// Persists one audit record before making it visible to readers.
    ///
    /// # Errors
    ///
    /// Returns an error if append or bounded-history compaction fails.
    pub fn record(&self, record: PaymentSignRecord) -> Result<()> {
        let mut records = self.records.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        {
            let mut writer = self.writer.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            serde_json::to_writer(&mut *writer, &record)?;
            writer.write_all(b"\n").map_err(|source| Error::WriteFile {
                path: self.path.clone(),
                source,
            })?;
            writer.sync_data().map_err(|source| Error::WriteFile {
                path: self.path.clone(),
                source,
            })?;
        }
        records.push_back(record);
        if records.len() > self.max_entries {
            let retained = self.max_entries.saturating_mul(3).div_ceil(4).max(1);
            let remove = records.len().saturating_sub(retained);
            records.drain(..remove);
            rewrite(&self.path, &records)?;
            let mut writer = self.writer.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            *writer = open_append(&self.path)?;
        }
        Ok(())
    }

    /// Returns newest-first records matching the bounded query.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid identifiers, ranges, or pagination.
    pub fn search(&self, query: &SignHistoryQuery) -> Result<PaymentSignHistoryPage> {
        validate_query(query)?;
        let limit = query.limit.unwrap_or(DEFAULT_QUERY_LIMIT);
        let records = self.records.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        let matching = records.iter().rev().filter(|record| matches_query(record, query));
        let total = matching.clone().count();
        let items = matching.skip(query.offset).take(limit).cloned().collect();
        Ok(PaymentSignHistoryPage {
            items,
            total,
            offset: query.offset,
            limit,
        })
    }

    #[must_use]
    pub fn get_by_request_id(&self, request_id: Uuid) -> Option<PaymentSignRecord> {
        self.records
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .rev()
            .find(|record| record.request_id == request_id)
            .cloned()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.records
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Checks that the current history path remains a regular owner-only file.
    ///
    /// # Errors
    ///
    /// Returns an error when the file is unavailable or was replaced unsafely.
    pub fn readiness(&self) -> Result<()> {
        reject_unsafe_existing_path(&self.path)
    }
}

fn validate_query(query: &SignHistoryQuery) -> Result<()> {
    if query.limit.is_some_and(|limit| limit == 0 || limit > MAX_QUERY_LIMIT) {
        return Err(Error::InvalidRequest(format!(
            "limit must be between 1 and {MAX_QUERY_LIMIT}"
        )));
    }
    if query.from.zip(query.to).is_some_and(|(from, to)| from > to) {
        return Err(Error::InvalidRequest("from must not be after to".to_owned()));
    }
    for (field, value) in [
        ("client_id", query.client_id.as_deref()),
        ("wallet_id", query.wallet_id.as_deref()),
        ("purpose", query.purpose.as_deref()),
    ] {
        if let Some(value) = value {
            validate_identifier(field, value)?;
        }
    }
    Ok(())
}

fn matches_query(record: &PaymentSignRecord, query: &SignHistoryQuery) -> bool {
    query.request_id.is_none_or(|value| record.request_id == value)
        && query.client_id.as_ref().is_none_or(|value| record.client_id == *value)
        && query.wallet_id.as_ref().is_none_or(|value| record.wallet_id == *value)
        && query.purpose.as_ref().is_none_or(|value| record.purpose == *value)
        && query.outcome.is_none_or(|value| record.outcome == value)
        && query.from.is_none_or(|value| record.completed_at >= value)
        && query.to.is_none_or(|value| record.completed_at <= value)
}

fn reject_unsafe_existing_path(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(Error::Config(format!(
            "sign history path must be a regular non-symlink file: {}",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(Error::ReadFile {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn open_append(path: &Path) -> Result<File> {
    OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|source| Error::WriteFile {
            path: path.to_path_buf(),
            source,
        })
}

fn load_records(path: &Path) -> Result<VecDeque<PaymentSignRecord>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|source| Error::ReadFile {
            path: path.to_path_buf(),
            source,
        })?;
    let mut reader = BufReader::new(file);
    let mut records = VecDeque::new();
    let mut line = Vec::new();
    loop {
        line.clear();
        let bytes = reader.read_until(b'\n', &mut line)?;
        if bytes == 0 {
            break;
        }
        if line.len() > MAX_RECORD_BYTES {
            return Err(Error::Config("sign history contains an oversized record".to_owned()));
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        if !line.is_empty() {
            records.push_back(serde_json::from_slice(&line)?);
        }
    }
    Ok(records)
}

fn count_lines(path: &Path) -> Result<usize> {
    let file = File::open(path).map_err(|source| Error::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(BufReader::new(file).lines().count())
}

fn rewrite(path: &Path, records: &VecDeque<PaymentSignRecord>) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::Config(format!("history path has no parent: {}", path.display())))?;
    let temporary = parent.join(format!(".sign-history-{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temporary)?;
        for record in records {
            serde_json::to_writer(&mut file, record).map_err(std::io::Error::other)?;
            file.write_all(b"\n")?;
        }
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        File::open(parent)?.sync_all()
    })();
    if let Err(source) = result {
        let _ = fs::remove_file(&temporary);
        return Err(Error::WriteFile {
            path: path.to_path_buf(),
            source,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::model::sign::{SignOutcome, SignTransport};

    use super::*;

    #[test]
    fn persists_and_filters_without_sensitive_payloads() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("history/history.jsonl");
        let history = SignHistoryStore::open(&path, 10).unwrap();
        let request_id = Uuid::new_v4();
        history
            .record(PaymentSignRecord {
                event_id: Uuid::new_v4(),
                request_id,
                client_id: "flowgent-1".to_owned(),
                wallet_id: "payer".to_owned(),
                purpose: "x402.payment".to_owned(),
                scheme: "eip712-secp256k1".to_owned(),
                transport: SignTransport::Local,
                outcome: SignOutcome::Succeeded,
                error_code: None,
                issued_at: 10,
                completed_at: 11,
            })
            .unwrap();
        let page = history
            .search(&SignHistoryQuery {
                wallet_id: Some("payer".to_owned()),
                ..SignHistoryQuery::default()
            })
            .unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(history.get_by_request_id(request_id).unwrap().request_id, request_id);
        assert!(!fs::read_to_string(path).unwrap().contains("signature"));
    }
}
