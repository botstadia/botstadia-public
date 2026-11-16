//! A bot's bundle, read from wherever it is, and the deterministic ZIP.
//!
//! A bundle is a ZIP holding `component.wasm`, `botstadia.toml` and any
//! models under `artifacts/` ([`botstadia_interface::bundle`]). Locally a bot
//! can also be a bundle directory with the same files, or a bare component
//! with no manifest and no models, which is how the house bots used to be
//! seated.
//!
//! [`zip_bytes`] builds the ZIP deterministically: entries in name order,
//! one fixed timestamp and mode, one compression. The same files always give
//! the same bytes, so the same digest.

use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use botstadia_interface::bundle::{
    self as format, BotKind, COMPONENT, MANIFEST, MAX_BUNDLE_BYTES, Manifest,
};

use crate::budget::BudgetVersion;

/// A bot, ready to seat.
#[derive(Debug, Clone)]
pub struct Bundle {
    /// What to call it in a run: its file or directory name.
    pub name: String,
    /// `None` for a bare component.
    pub manifest: Option<Manifest>,
    pub component: Vec<u8>,
    /// In manifest order: `(name, bytes)`.
    pub models: Vec<(String, Vec<u8>)>,
    /// `sha256-<hex>` of the deterministic ZIP, or of a bare component.
    pub digest: String,
}

impl Bundle {
    /// Load a bot from a bundle ZIP, a bundle directory or a bare `.wasm`.
    pub fn load(path: &Path, budget: &BudgetVersion) -> Result<Self> {
        let name = bot_name(path);
        if path.is_dir() {
            let mut bundle = Self::from_dir(path)?;
            bundle.name = name;
            return Ok(bundle);
        }
        let bytes = std::fs::read(path)
            .with_context(|| format!("reading the bot at {}", path.display()))?;
        if bytes.starts_with(b"\0asm") {
            return Ok(Self {
                name,
                manifest: None,
                digest: crate::digest(&bytes),
                component: bytes,
                models: Vec::new(),
            });
        }
        if bytes.starts_with(b"PK") {
            let mut bundle = Self::from_zip(&bytes, budget)?;
            bundle.name = name;
            return Ok(bundle);
        }
        bail!(
            "{} is neither a WASM component nor a bundle ZIP",
            path.display()
        )
    }

    /// A bundle directory: `botstadia.toml`, `component.wasm`, `artifacts/`.
    pub fn from_dir(dir: &Path) -> Result<Self> {
        let manifest_path = dir.join(MANIFEST);
        let manifest = Manifest::from_toml(
            &std::fs::read_to_string(&manifest_path)
                .with_context(|| format!("reading {}", manifest_path.display()))?,
        )
        .map_err(anyhow::Error::msg)?;
        check_manifest(&manifest)?;
        let component = std::fs::read(dir.join(COMPONENT))
            .with_context(|| format!("reading {}", dir.join(COMPONENT).display()))?;
        let mut models = Vec::new();
        for model in &manifest.models {
            let bytes = std::fs::read(dir.join(&model.path))
                .with_context(|| format!("reading model `{}` at {}", model.name, model.path))?;
            models.push((model.name.clone(), bytes));
        }
        let mut bundle = Self {
            name: bot_name(dir),
            manifest: Some(manifest),
            component,
            models,
            digest: String::new(),
        };
        bundle.digest = crate::digest(&bundle.zip()?);
        Ok(bundle)
    }

    /// A bundle ZIP, checked as verification checks one: only the files the
    /// manifest names, no paths outside the bundle, and nothing past the
    /// uncompressed limit.
    pub fn from_zip(bytes: &[u8], budget: &BudgetVersion) -> Result<Self> {
        if bytes.len() as u64 > MAX_BUNDLE_BYTES {
            bail!(
                "the bundle is {} bytes, over the {MAX_BUNDLE_BYTES}-byte limit",
                bytes.len()
            );
        }
        let mut archive =
            zip::ZipArchive::new(Cursor::new(bytes)).context("reading the bundle ZIP")?;
        let mut files = std::collections::BTreeMap::new();
        let mut total: u64 = 0;
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index)?;
            if entry.is_dir() {
                continue;
            }
            let name = entry.name()?.to_string();
            if name.starts_with('/') || name.split('/').any(|part| part == ".." || part.is_empty())
            {
                bail!("the bundle has an unsafe path `{name}`");
            }
            let mut content = Vec::new();
            let limit = budget.uncompressed_bundle_bytes.saturating_sub(total) + 1;
            entry.by_ref().take(limit).read_to_end(&mut content)?;
            total += content.len() as u64;
            if total > budget.uncompressed_bundle_bytes {
                bail!(
                    "the bundle unzips past {} bytes",
                    budget.uncompressed_bundle_bytes
                );
            }
            if files.insert(name.clone(), content).is_some() {
                bail!("the bundle holds `{name}` twice");
            }
        }
        let manifest = Manifest::from_toml(
            std::str::from_utf8(
                files
                    .get(MANIFEST)
                    .context("the bundle has no botstadia.toml")?,
            )
            .context("botstadia.toml isn't UTF-8")?,
        )
        .map_err(anyhow::Error::msg)?;
        check_manifest(&manifest)?;
        for name in files.keys() {
            if !format::entry_allowed(&manifest, name) {
                bail!("the bundle holds `{name}`, which its manifest doesn't name");
            }
        }
        let component = files
            .remove(COMPONENT)
            .context("the bundle has no component.wasm")?;
        let mut models = Vec::new();
        for model in &manifest.models {
            let bytes = files.remove(&model.path).with_context(|| {
                format!(
                    "the manifest names {}, which isn't in the bundle",
                    model.path
                )
            })?;
            models.push((model.name.clone(), bytes));
        }
        Ok(Self {
            name: String::new(),
            manifest: Some(manifest),
            component,
            models,
            digest: crate::digest(bytes),
        })
    }

    pub fn kind(&self) -> Option<BotKind> {
        self.manifest.as_ref().map(|manifest| manifest.kind)
    }

    /// The deterministic ZIP of this bundle. A bare component has none.
    pub fn zip(&self) -> Result<Vec<u8>> {
        let manifest = self
            .manifest
            .as_ref()
            .context("a bare component has no bundle: build one with a manifest")?;
        let mut files: Vec<(String, Vec<u8>)> = vec![
            (MANIFEST.to_string(), manifest.to_toml().into_bytes()),
            (COMPONENT.to_string(), self.component.clone()),
        ];
        for (entry, (_, bytes)) in manifest.models.iter().zip(&self.models) {
            files.push((entry.path.clone(), bytes.clone()));
        }
        zip_bytes(files)
    }

    /// Write the bundle ZIP to `path`, returning its digest.
    pub fn write_zip(&self, path: &Path) -> Result<String> {
        let bytes = self.zip()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, &bytes).with_context(|| format!("writing {}", path.display()))?;
        Ok(crate::digest(&bytes))
    }
}

fn check_manifest(manifest: &Manifest) -> Result<()> {
    let problems = manifest.problems();
    if !problems.is_empty() {
        bail!("botstadia.toml: {}", problems.join("; "));
    }
    Ok(())
}

/// A bot's name from its path: the file stem, or the directory's name.
pub fn bot_name(path: &Path) -> String {
    let stem = if path.is_dir() {
        path.file_name()
    } else {
        path.file_stem()
    };
    stem.map_or("bot".into(), |stem| stem.to_string_lossy().into_owned())
}

/// A deterministic ZIP of `files`: entries in name order, a fixed time and
/// mode, deflate at one level.
pub fn zip_bytes(mut files: Vec<(String, Vec<u8>)>) -> Result<Vec<u8>> {
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .compression_level(Some(6))
        .last_modified_time(zip::DateTime::default())
        .unix_permissions(0o644);
    for (name, bytes) in files {
        writer.start_file(name, options)?;
        writer.write_all(&bytes)?;
    }
    Ok(writer.finish()?.into_inner())
}

/// Every bundle-like path under `dir`: used to find staged bots.
pub fn find(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.join(MANIFEST).is_file()
                || path
                    .extension()
                    .is_some_and(|ext| ext == "wasm" || ext == "zip")
        })
        .collect();
    found.sort();
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use botstadia_interface::bundle::ModelEntry;

    fn sample() -> Bundle {
        let mut manifest = Manifest::new("dance-off", 1, BotKind::Policy);
        manifest.models.push(ModelEntry {
            name: "policy".into(),
            path: "artifacts/policy.onnx".into(),
        });
        Bundle {
            name: "sample".into(),
            manifest: Some(manifest),
            component: b"\0asm-not-really".to_vec(),
            models: vec![("policy".into(), vec![1, 2, 3])],
            digest: String::new(),
        }
    }

    #[test]
    fn the_zip_is_deterministic_and_round_trips() {
        let bundle = sample();
        let a = bundle.zip().unwrap();
        let b = bundle.zip().unwrap();
        assert_eq!(a, b);
        let back = Bundle::from_zip(&a, &crate::budget::V1).unwrap();
        assert_eq!(back.component, bundle.component);
        assert_eq!(back.models, bundle.models);
        assert_eq!(back.digest, crate::digest(&a));
    }

    #[test]
    fn unnamed_files_and_unsafe_paths_are_refused() {
        let bundle = sample();
        let manifest = bundle.manifest.as_ref().unwrap().to_toml();
        let extra = zip_bytes(vec![
            (MANIFEST.into(), manifest.clone().into_bytes()),
            (COMPONENT.into(), vec![0]),
            ("artifacts/policy.onnx".into(), vec![1]),
            ("notes.txt".into(), vec![2]),
        ])
        .unwrap();
        let error = Bundle::from_zip(&extra, &crate::budget::V1).unwrap_err();
        assert!(format!("{error}").contains("notes.txt"));
        let unsafe_path = zip_bytes(vec![
            (MANIFEST.into(), manifest.into_bytes()),
            ("../component.wasm".into(), vec![0]),
        ])
        .unwrap();
        assert!(Bundle::from_zip(&unsafe_path, &crate::budget::V1).is_err());
    }
}
