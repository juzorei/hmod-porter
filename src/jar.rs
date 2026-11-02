//! Reads a Java mod's .jar: entries, fabric.mod.json / mods.toml manifest,
//! and the assets/<ns>/... and data/<ns>/... trees that a data-only port
//! can carry over.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    /// PNG, JSON, etc. — a file worth carrying.
    Resource(Vec<u8>),
    /// Compiled Java byte code.
    Class,
    /// Anything else (META-INF signatures, native libs, ...).
    Other,
}

#[derive(Debug, Default)]
pub struct JarContents {
    /// "assets/<ns>/..." and "data/<ns>/..." entries.
    pub content: BTreeMap<String, Entry>,
    pub classes: usize,
    pub other: BTreeMap<String, Entry>,
}

#[derive(Debug, Clone)]
pub struct JarManifest {
    pub mod_id: String,
    pub version: Option<String>,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub authors: Vec<String>,
    pub license: Option<String>,
    /// Loader the jar declares (fabric / neoforge / forge / unknown).
    pub loader: String,
}

impl JarContents {
    pub fn total_content_bytes(&self) -> usize {
        self.content.values().map(|e| match e {
            Entry::Resource(b) => b.len(),
            _ => 0,
        }).sum()
    }
}

/// Loads a jar without running any Java: only unzip + read manifests.
pub fn read_jar(path: &Path) -> Result<(JarContents, JarManifest)> {
    let file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut zip = zip::ZipArchive::new(file).context("read zip archive")?;

    let mut out = JarContents::default();
    let manifest = &mut None;

    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let name = entry
            .name()
            .map(|n| n.to_string())
            .with_context(|| "reading entry name (unsupported encoding?)")?;
        let (kind, bytes) = read_entry(&mut entry, &name)?;

        match kind {
            EntryKind::Manifest => {
                if manifest.is_none() {
                    if let Entry::Resource(b) = &bytes {
                        *manifest = Some(parse_manifest(&name, b)?);
                    }
                }
            }
            EntryKind::Content => {
                out.content.insert(name.clone(), bytes);
            }
            EntryKind::Class => out.classes += 1,
            EntryKind::Other => {
                out.other.insert(name.clone(), bytes);
            }
        }
    }

    let manifest = manifest
        .take()
        .with_context(|| format!("no fabric.mod.json or META-INF/neoforge.mods.toml/mods.toml found in {}", path.display()))?;

    Ok((out, manifest))
}

enum EntryKind {
    Manifest,
    Content,
    Class,
    Other,
}

fn read_entry<R: Read>(entry: &mut zip::read::ZipFile<'_, R>, name: &str) -> Result<(EntryKind, Entry)> {
    if name.ends_with('/') {
        return Ok((EntryKind::Other, Entry::Other));
    }
    let mut bytes = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut bytes)?;

    // fabric.mod.json at root, or any loader's toml manifest
    // (META-INF/mods.toml, META-INF/neoforge.mods.toml).
    if name == "fabric.mod.json"
        || name == "META-INF/mods.toml"
        || name == "META-INF/neoforge.mods.toml"
        || name == "quilt.mod.json"
    {
        return Ok((EntryKind::Manifest, Entry::Resource(bytes)));
    }
    if name.ends_with(".class") {
        return Ok((EntryKind::Class, Entry::Class));
    }
    if name.starts_with("assets/") || name.starts_with("data/") {
        return Ok((EntryKind::Content, Entry::Resource(bytes)));
    }
    Ok((EntryKind::Other, Entry::Other))
}

fn parse_manifest(name: &str, bytes: &[u8]) -> Result<JarManifest> {
    if name == "fabric.mod.json" {
        let v: serde_json::Value = serde_json::from_slice(bytes).context("parse fabric.mod.json")?;
        Ok(JarManifest {
            mod_id: v["id"].as_str().unwrap_or_default().to_string(),
            version: v["version"].as_str().map(String::from),
            display_name: v["name"].as_str().map(String::from),
            description: v.get("description").and_then(|d| d.as_str()).map(String::from),
            authors: authors_from_fabric(&v),
            license: v["license"].as_str().map(String::from),
            loader: "fabric".into(),
        })
    } else {
        // neoforge/forge mods.toml — parse as a document (Value rejects
        // root-level dotted/commented loader headers).
        let t: toml::Table = std::str::from_utf8(bytes)
            .context("mods.toml utf-8")?
            .parse()
            .context("parse mods.toml")?;
        let props = t.get("mods").and_then(|m| m.as_array()).and_then(|a| a.first());
        Ok(JarManifest {
            mod_id: props
                .and_then(|p| p.get("modId"))
                .and_then(toml::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            version: version_from_toml(&t, props),
            display_name: props
                .and_then(|p| p.get("displayName"))
                .and_then(toml::Value::as_str)
                .map(String::from),
            description: props
                .and_then(|p| p.get("description"))
                .and_then(toml::Value::as_str)
                .map(String::from),
            authors: authors_from_toml(&t, props),
            license: t
                .get("license")
                .and_then(toml::Value::as_str)
                .map(String::from),
            loader: if name.starts_with("META-INF/neoforge") { "neoforge".into() } else { "forge".into() },
        })
    }
}

fn version_from_toml(t: &toml::Table, props: Option<&toml::Value>) -> Option<String> {
    props
        .and_then(|p| p.get("version"))
        .and_then(toml::Value::as_str)
        .map(String::from)
        .or_else(|| {
            // "${file.jarVersion}" style: fall back to the jar's manifest below in main.
            t.get("mods").and_then(|_| None)
        })
}

fn authors_from_fabric(v: &serde_json::Value) -> Vec<String> {
    match v.get("authors") {
        Some(serde_json::Value::Array(a)) => a
            .iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect(),
        Some(serde_json::Value::String(s)) => vec![s.clone()],
        Some(obj) => obj
            .as_object()
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default(),
        None => Vec::new(),
    }
}

fn authors_from_toml(t: &toml::Table, props: Option<&toml::Value>) -> Vec<String> {
    if let Some(a) = t.get("authors").and_then(toml::Value::as_str) {
        return vec![a.to_string()];
    }
    props
        .and_then(|p| p.get("authors"))
        .and_then(toml::Value::as_str)
        .map(|s| s.split(',').map(|x| x.trim().to_string()).collect())
        .unwrap_or_default()
}
