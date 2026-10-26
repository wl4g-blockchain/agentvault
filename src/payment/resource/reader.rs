use super::*;

impl ResourceImporter {
    pub(crate) fn read_resources(paths: &[PathBuf]) -> Result<ImportBatch> {
        if paths.is_empty() {
            return Err(Error::InvalidRequest(
                "console import requires at least one path".to_owned(),
            ));
        }
        let mut files = Vec::new();
        for path in paths {
            ResourceImporter::collect_resource_files(path, 0, &mut files)?;
        }
        files.sort();
        files.dedup();
        if files.is_empty() {
            return Err(Error::InvalidRequest(
                "console import found no YAML or JSON resources".to_owned(),
            ));
        }
        let mut batch = ImportBatch::default();
        for path in files {
            ResourceImporter::read_resource_file(&path, &mut batch)?;
        }
        Ok(batch)
    }

    pub(crate) fn collect_resource_files(path: &Path, depth: usize, files: &mut Vec<PathBuf>) -> Result<()> {
        if depth > MAX_IMPORT_DEPTH || files.len() >= MAX_RESOURCE_FILES {
            return Err(Error::Config("console import path limits were exceeded".to_owned()));
        }
        let metadata = fs::symlink_metadata(path).map_err(|source| Error::ReadFile {
            path: path.to_path_buf(),
            source,
        })?;
        if metadata.file_type().is_symlink() {
            return Err(Error::Config(format!(
                "console import path must not be a symbolic link: {}",
                path.display()
            )));
        }
        if metadata.is_file() {
            if ResourceImporter::is_resource_file(path) {
                files.push(path.to_path_buf());
            } else {
                return Err(Error::Config(format!(
                    "console import file must end in .yaml, .yml, or .json: {}",
                    path.display()
                )));
            }
            return Ok(());
        }
        if !metadata.is_dir() {
            return Err(Error::Config(format!(
                "console import path is not a regular file or directory: {}",
                path.display()
            )));
        }
        let mut entries = fs::read_dir(path)
            .map_err(|source| Error::ReadFile {
                path: path.to_path_buf(),
                source,
            })?
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(|source| Error::ReadFile {
                path: path.to_path_buf(),
                source,
            })?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            let child = entry.path();
            let child_metadata = fs::symlink_metadata(&child).map_err(|source| Error::ReadFile {
                path: child.clone(),
                source,
            })?;
            if child_metadata.is_dir()
                || ResourceImporter::is_resource_file(&child)
                || child_metadata.file_type().is_symlink()
            {
                ResourceImporter::collect_resource_files(&child, depth + 1, files)?;
            }
        }
        Ok(())
    }

    pub(crate) fn is_resource_file(path: &Path) -> bool {
        path.extension()
            .and_then(std::ffi::OsStr::to_str)
            .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "yaml" | "yml" | "json"))
    }

    pub(crate) fn read_resource_file(path: &Path, batch: &mut ImportBatch) -> Result<()> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)
            .map_err(|source| Error::ReadFile {
                path: path.to_path_buf(),
                source,
            })?;
        let metadata = file.metadata().map_err(|source| Error::ReadFile {
            path: path.to_path_buf(),
            source,
        })?;
        if !metadata.is_file() || metadata.len() > MAX_RESOURCE_FILE_BYTES {
            return Err(Error::Config(
                "console resource must be a regular file no larger than 1 MiB".to_owned(),
            ));
        }
        let mut body = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(MAX_RESOURCE_FILE_BODY_BYTES));
        file.take(MAX_RESOURCE_FILE_BYTES + 1)
            .read_to_end(&mut body)
            .map_err(|source| Error::ReadFile {
                path: path.to_path_buf(),
                source,
            })?;
        if body.len() > MAX_RESOURCE_FILE_BODY_BYTES {
            return Err(Error::Config(
                "console resource changed while reading or exceeds 1 MiB".to_owned(),
            ));
        }
        let mut documents = 0_usize;
        for document in serde_yaml::Deserializer::from_slice(&body) {
            let envelope = ResourceEnvelope::deserialize(document)
                .map_err(|error| Error::Config(format!("invalid console resource {}: {error}", path.display())))?;
            ResourceImporter::append_resource(batch, envelope, path)?;
            documents += 1;
        }
        if documents == 0 {
            return Err(Error::Config(format!(
                "console resource file is empty: {}",
                path.display()
            )));
        }
        Ok(())
    }

    pub(crate) fn append_resource(batch: &mut ImportBatch, envelope: ResourceEnvelope, path: &Path) -> Result<()> {
        if envelope.console_version != CONSOLE_VERSION {
            return Err(Error::Config(format!(
                "unsupported consoleVersion in {}: {}",
                path.display(),
                envelope.console_version
            )));
        }
        if envelope.metadata.name.trim().is_empty() {
            return Err(Error::InvalidRequest(format!(
                "console resource in {} has an empty metadata.name",
                path.display()
            )));
        }
        let enabled = envelope.metadata.status.enabled();
        match envelope.kind {
            ResourceKind::Client => batch.clients.push(ResourceImporter::decode_named_resource(
                envelope.metadata.name,
                enabled,
                envelope.data,
                path,
            )?),
            ResourceKind::Wallet => batch.wallets.push(ResourceImporter::decode_named_resource(
                envelope.metadata.name,
                enabled,
                envelope.data,
                path,
            )?),
            ResourceKind::Scenario => batch.scenarios.push(ResourceImporter::decode_named_resource(
                envelope.metadata.name,
                enabled,
                envelope.data,
                path,
            )?),
            ResourceKind::Policy => batch.policies.push(ResourceImporter::decode_named_resource(
                envelope.metadata.name,
                enabled,
                envelope.data,
                path,
            )?),
            ResourceKind::Price => batch.offers.push(ResourceImporter::decode_named_resource(
                envelope.metadata.name,
                enabled,
                envelope.data,
                path,
            )?),
            ResourceKind::Approver => {
                let mut value = ResourceImporter::resource_mapping(envelope.data, path)?;
                ResourceImporter::insert_field(&mut value, "enabled", Value::Bool(enabled), path)?;
                batch
                    .approvers
                    .push(serde_yaml::from_value(Value::Mapping(value)).map_err(|error| {
                        Error::Config(format!("invalid Approver resource {}: {error}", path.display()))
                    })?);
            }
        }
        Ok(())
    }

    pub(crate) fn decode_named_resource<T: for<'de> Deserialize<'de>>(
        name: String,
        enabled: bool,
        data: Value,
        path: &Path,
    ) -> Result<T> {
        let mut value = ResourceImporter::resource_mapping(data, path)?;
        ResourceImporter::insert_field(&mut value, "id", Value::String(name), path)?;
        ResourceImporter::insert_field(&mut value, "enabled", Value::Bool(enabled), path)?;
        serde_yaml::from_value(Value::Mapping(value))
            .map_err(|error| Error::Config(format!("invalid console resource {}: {error}", path.display())))
    }

    pub(crate) fn resource_mapping(data: Value, path: &Path) -> Result<Mapping> {
        match data {
            Value::Mapping(value) => Ok(value),
            _ => Err(Error::Config(format!(
                "console resource data must be a mapping: {}",
                path.display()
            ))),
        }
    }

    pub(crate) fn insert_field(mapping: &mut Mapping, field: &str, value: Value, path: &Path) -> Result<()> {
        if mapping.insert(Value::String(field.to_owned()), value).is_some() {
            return Err(Error::Config(format!(
                "console resource {} duplicates metadata-owned field {field}",
                path.display()
            )));
        }
        Ok(())
    }

    /// Converts aggregate test fixtures into the canonical one-resource-per-
    /// document import format. Production import intentionally has no aggregate
    /// compatibility path.
    #[cfg(test)]
    pub(crate) fn encode_test_resource_documents(aggregate: &serde_json::Value) -> String {
        let kinds = [
            ("clients", "Client"),
            ("wallets", "Wallet"),
            ("scenarios", "Scenario"),
            ("policies", "Policy"),
            ("offers", "Price"),
            ("approvers", "Approver"),
        ];
        let mut documents = Vec::new();
        for (collection, kind) in kinds {
            let Some(resources) = aggregate.get(collection).and_then(serde_json::Value::as_array) else {
                continue;
            };
            for resource in resources {
                let mut data = resource
                    .as_object()
                    .unwrap_or_else(|| panic!("{collection} test fixture entry must be an object"))
                    .clone();
                let name = if kind == "Approver" {
                    format!(
                        "{}--{}",
                        data.get("scenario_id")
                            .and_then(serde_json::Value::as_str)
                            .expect("approver scenario_id"),
                        data.get("auth_principal")
                            .and_then(serde_json::Value::as_str)
                            .expect("approver auth_principal")
                    )
                } else {
                    data.remove("id")
                        .and_then(|value| value.as_str().map(str::to_owned))
                        .unwrap_or_else(|| panic!("{kind} test fixture requires id"))
                };
                let enabled = data.remove("enabled").and_then(|value| value.as_bool()).unwrap_or(true);
                let document = serde_json::json!({
                    "consoleVersion": CONSOLE_VERSION,
                    "kind": kind,
                    "metadata": {
                        "name": name,
                        "status": if enabled { "active" } else { "disabled" }
                    },
                    "data": data
                });
                documents.push(serde_yaml::to_string(&document).expect("encode canonical test resource"));
            }
        }
        documents.join("---\n")
    }
}

#[cfg(test)]
#[path = "../../../tests/payment/resource/reader_test.rs"]
mod tests;
