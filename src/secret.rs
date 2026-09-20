use std::{
    fs::{self, OpenOptions},
    io::Read,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
};

use zeroize::Zeroizing;

use crate::{Error, Result};

pub(crate) fn ensure_private_dir(path: impl AsRef<Path>, description: &str) -> Result<()> {
    let path = path.as_ref();
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(Error::Config(format!("{description} must be a real directory")));
            }
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(Error::Config(format!(
                    "{description} must not be accessible by group or other users"
                )));
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path).map_err(|source| Error::WriteFile {
                path: path.to_path_buf(),
                source,
            })?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| Error::WriteFile {
                path: path.to_path_buf(),
                source,
            })
        }
        Err(source) => Err(Error::ReadFile {
            path: path.to_path_buf(),
            source,
        }),
    }
}

pub(crate) fn read_private_file(
    path: impl AsRef<Path>,
    description: &str,
    max_bytes: usize,
) -> Result<Zeroizing<Vec<u8>>> {
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
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(Error::Config(format!(
            "{description} must not be accessible by group or other users"
        )));
    }
    if metadata.len() > max_bytes as u64 {
        return Err(Error::Config(format!("{description} exceeds {max_bytes} bytes")));
    }

    let capacity =
        usize::try_from(metadata.len()).map_err(|_| Error::Config(format!("{description} size is not supported")))?;
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

pub(crate) fn read_private_text(
    path: impl AsRef<Path>,
    description: &str,
    max_bytes: usize,
) -> Result<Zeroizing<String>> {
    let value = read_private_file(path, description, max_bytes)?;
    let text = String::from_utf8(value.to_vec())
        .map_err(|_| Error::Config(format!("{description} must contain UTF-8 text")))?;
    Ok(Zeroizing::new(text))
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::symlink};

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn rejects_symlink_and_overly_permissive_secret() {
        let temp = TempDir::new().unwrap();
        let secret = temp.path().join("secret");
        fs::write(&secret, "value").unwrap();
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_private_text(&secret, "test secret", 32).is_err());

        fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
        let link = temp.path().join("link");
        symlink(&secret, &link).unwrap();
        assert!(read_private_text(&link, "test secret", 32).is_err());
    }
}
