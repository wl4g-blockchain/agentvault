use std::{
    fs::OpenOptions,
    io::Read,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use zeroize::Zeroizing;

use crate::{
    config::{SecretRefConfig, SECRETS_ENV_FILE_ENV},
    Error, Result,
};

const MAX_SECRETS_ENV_FILE_BYTES: usize = 1024 * 1024;

/// Resolves sensitive field-path values without placing them in `AppConfig`.
struct SecretReader {
    projection: Option<PathBuf>,
}

impl SecretReader {
    fn from_environment() -> Self {
        Self::with_projection(std::env::var_os(SECRETS_ENV_FILE_ENV))
    }

    fn with_projection(selector: Option<std::ffi::OsString>) -> Self {
        Self {
            projection: selector.filter(|value| !value.is_empty()).map(PathBuf::from),
        }
    }

    fn read_regular_file(path: impl AsRef<Path>, description: &str, max_bytes: usize) -> Result<Zeroizing<Vec<u8>>> {
        let path = path.as_ref();
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .map_err(|source| Error::ReadFile {
                path: path.to_path_buf(),
                source,
            })?;
        let metadata = file.metadata().map_err(|source| Error::ReadFile {
            path: path.to_path_buf(),
            source,
        })?;
        if !metadata.is_file() {
            return Err(Error::Config(format!("{description} must be a regular file")));
        }
        if metadata.len() > max_bytes as u64 {
            return Err(Error::Config(format!("{description} exceeds {max_bytes} bytes")));
        }

        let capacity = usize::try_from(metadata.len())
            .map_err(|_| Error::Config(format!("{description} size is not supported")))?;
        let mut value = Zeroizing::new(Vec::with_capacity(capacity));
        file.take(max_bytes as u64 + 1)
            .read_to_end(&mut value)
            .map_err(|source| Error::ReadFile {
                path: path.to_path_buf(),
                source,
            })?;
        if value.len() > max_bytes {
            return Err(Error::Config(format!("{description} exceeds {max_bytes} bytes")));
        }
        Ok(value)
    }

    fn read_private_file(path: impl AsRef<Path>, description: &str, max_bytes: usize) -> Result<Zeroizing<Vec<u8>>> {
        let path = path.as_ref();
        let metadata = std::fs::symlink_metadata(path).map_err(|source| Error::ReadFile {
            path: path.to_path_buf(),
            source,
        })?;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(Error::Config(format!(
                "{description} must not be accessible by group or other users"
            )));
        }
        Self::read_regular_file(path, description, max_bytes)
    }

    fn read_private_text(path: impl AsRef<Path>, description: &str, max_bytes: usize) -> Result<Zeroizing<String>> {
        let value = Self::read_private_file(path, description, max_bytes)?;
        let text = String::from_utf8(value.to_vec())
            .map_err(|_| Error::Config(format!("{description} must contain UTF-8 text")))?;
        Ok(Zeroizing::new(text))
    }

    fn read_projected_env_value(
        path: impl AsRef<Path>,
        variable: &str,
        description: &str,
        max_bytes: usize,
    ) -> Result<Option<Zeroizing<String>>> {
        let path = path.as_ref();
        let metadata = std::fs::symlink_metadata(path).map_err(|source| Error::ReadFile {
            path: path.to_path_buf(),
            source,
        })?;
        if metadata.permissions().mode() & 0o022 != 0 {
            return Err(Error::Config(
                "projected secret env file must not be writable by group or other users".to_owned(),
            ));
        }
        let contents = Self::read_regular_file(path, "projected secret env file", MAX_SECRETS_ENV_FILE_BYTES)?;
        let text = std::str::from_utf8(&contents)
            .map_err(|_| Error::Config("projected secret env file must contain UTF-8 text".to_owned()))?;
        let mut found = None;
        for (index, raw_line) in text.lines().enumerate() {
            let line = raw_line.trim_end_matches('\r');
            if line.trim().is_empty() || line.trim_start().starts_with('#') {
                continue;
            }
            let (key, value) = line.split_once('=').ok_or_else(|| {
                Error::Config(format!(
                    "projected secret env file line {} must use KEY=VALUE",
                    index + 1
                ))
            })?;
            let key = key.trim();
            if key.is_empty() || !key.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_') {
                return Err(Error::Config(format!(
                    "projected secret env file line {} has an invalid key",
                    index + 1
                )));
            }
            if key == variable {
                if found.is_some() {
                    return Err(Error::Config(format!(
                        "projected secret env file defines {variable} more than once"
                    )));
                }
                if value.len() > max_bytes {
                    return Err(Error::Config(format!("{description} exceeds {max_bytes} bytes")));
                }
                found = Some(Zeroizing::new(value.to_owned()));
            }
        }
        Ok(found)
    }

    fn read_env(
        &self,
        variable: &str,
        description: &str,
        max_bytes: usize,
        lookup: impl Fn(&str) -> Option<std::ffi::OsString>,
    ) -> Result<Zeroizing<String>> {
        if variable.trim().is_empty() {
            return Err(Error::Config(format!(
                "{description} environment variable name is empty"
            )));
        }
        if let Some(path) = &self.projection {
            if let Some(value) = Self::read_projected_env_value(path, variable, description, max_bytes)? {
                return Ok(value);
            }
        }
        let value = lookup(variable)
            .ok_or_else(|| Error::Config(format!("{description} environment variable is unavailable")))?;
        let value = Zeroizing::new(
            value
                .into_string()
                .map_err(|_| Error::Config(format!("{description} must contain UTF-8 text")))?,
        );
        if value.len() > max_bytes {
            return Err(Error::Config(format!("{description} exceeds {max_bytes} bytes")));
        }
        Ok(value)
    }

    fn read_ref(reference: &SecretRefConfig, description: &str, max_bytes: usize) -> Result<Zeroizing<String>> {
        match (reference.file.trim(), reference.env.trim()) {
            (path, "") if !path.is_empty() => Self::read_private_text(path, description, max_bytes),
            ("", variable) if !variable.is_empty() => {
                Self::from_environment().read_env(variable, description, max_bytes, |name| std::env::var_os(name))
            }
            _ => Err(Error::Config(format!(
                "{description} must select exactly one secret source"
            ))),
        }
    }
}

/// Reads a named secret value; the selected projection takes precedence over process env.
pub(crate) fn read_secret_env(variable: &str, description: &str, max_bytes: usize) -> Result<Zeroizing<String>> {
    SecretReader::from_environment().read_env(variable, description, max_bytes, |name| std::env::var_os(name))
}

/// Resolves an indirect reference without serializing secret values into configuration.
pub(crate) fn read_secret_ref(
    reference: &SecretRefConfig,
    description: &str,
    max_bytes: usize,
) -> Result<Zeroizing<String>> {
    SecretReader::read_ref(reference, description, max_bytes)
}

#[cfg(test)]
#[path = "../../tests/utils/secret_test.rs"]
mod tests;
