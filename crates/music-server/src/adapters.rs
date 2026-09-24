//! Adapters: LoRA files the engine merges into its model for one request.
//!
//! Each adapter is a folder under `adapters/` in the studio data: the weight
//! files the engine reads, and `adapter.json`, what the studio knows about them
//! - a name, a trigger word, the strength each slot starts at, where it came
//! from. The engine is started with that folder as its adapter directory and a
//! request names folders, so nothing here parses weights: which parts of the
//! model an adapter touches is the engine's answer, asked of it and remembered.
//!
//! The examples are a catalogue shipped with the studio, every file pinned to a
//! revision. Nothing downloads on its own; a catalogue entry is fetched when the
//! user asks for it, through the same resumable downloader as every other extra.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::downloads::{Asset, AssetKind, Downloader};

/// The downloader scope the adapters page reads its progress under.
pub const SCOPE: &str = "adapters";

const META: &str = "adapter.json";

/// A name or description, one string or one per interface language.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Text {
    Plain(String),
    Localized(BTreeMap<String, String>),
}

impl Default for Text {
    fn default() -> Self {
        Text::Plain(String::new())
    }
}

#[derive(Debug, Clone, Deserialize)]
struct CatalogFile {
    url: String,
    file: String,
    bytes: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct CatalogEntry {
    id: String,
    engine: String,
    kind: String,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    page: Option<String>,
    #[serde(default)]
    trigger: Option<String>,
    name: Text,
    description: Text,
    scales: BTreeMap<String, f64>,
    #[serde(default)]
    range: Option<[f64; 2]>,
    files: Vec<CatalogFile>,
}

#[derive(Deserialize)]
struct CatalogFileFormat {
    adapters: Vec<CatalogEntry>,
}

/// A catalogue entry with its downloads, built once. The downloader works on
/// `'static` assets, and the catalogue is fixed for the life of the process.
struct CatalogItem {
    entry: CatalogEntry,
    assets: Vec<&'static Asset>,
}

fn catalog() -> &'static [CatalogItem] {
    static CATALOG: OnceLock<Vec<CatalogItem>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let parsed: CatalogFileFormat = serde_json::from_str(include_str!("../../../config/adapter-catalog.json"))
            .expect("config/adapter-catalog.json is valid");
        parsed
            .adapters
            .into_iter()
            .map(|entry| {
                let assets = entry
                    .files
                    .iter()
                    .map(|file| {
                        let leak = |text: String| -> &'static str { Box::leak(text.into_boxed_str()) };
                        let asset: &'static Asset = Box::leak(Box::new(Asset {
                            id: leak(format!("{}/{}", entry.id, file.file)),
                            label: leak(file.file.clone()),
                            kind: AssetKind::Model,
                            url: leak(file.url.clone()),
                            relative_path: leak(format!("{}/{}", entry.id, file.file)),
                            bytes: file.bytes,
                            unzip_into: None,
                            marker: "",
                            pick: &[],
                            vram_gb: None,
                            note: "",
                        }));
                        asset
                    })
                    .collect();
                CatalogItem { entry, assets }
            })
            .collect()
    })
}

/// Where an installed adapter came from.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Origin {
    Catalog { catalog_id: String },
    Imported,
    Trained,
}

/// What the studio remembers about an installed adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterMeta {
    pub id: String,
    pub engine: String,
    pub name: Text,
    #[serde(default)]
    pub description: Text,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub trigger: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub page: Option<String>,
    /// The strength each slot starts at when the adapter is picked.
    #[serde(default)]
    pub scales: BTreeMap<String, f64>,
    /// The slider range, when the adapter is only meaningful inside one.
    #[serde(default)]
    pub range: Option<[f64; 2]>,
    /// The slots the engine found weights for, remembered from its last answer
    /// so the page can show them while the engine is not running.
    #[serde(default)]
    pub slots: Vec<String>,
    pub origin: Origin,
    #[serde(default)]
    pub created_at: String,
}

/// An installed adapter as the interface sees it.
#[derive(Debug, Clone, Serialize)]
pub struct Installed {
    #[serde(flatten)]
    pub meta: AdapterMeta,
    pub bytes: u64,
    /// The engine's complaint about the files, when it has one.
    pub error: Option<String>,
}

/// A catalogue entry as the interface sees it.
#[derive(Debug, Clone, Serialize)]
pub struct Offered {
    pub id: String,
    pub name: Text,
    pub description: Text,
    pub kind: String,
    pub author: Option<String>,
    pub page: Option<String>,
    pub trigger: Option<String>,
    pub slots: Vec<String>,
    pub bytes: u64,
    pub installed: bool,
}

/// What a partial update may change.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Patch {
    pub name: Option<String>,
    pub trigger: Option<String>,
    pub scales: Option<BTreeMap<String, f64>>,
}

/// The engine's view of one entry of the adapter directory, from `/props`.
#[derive(Debug, Clone, Default)]
pub struct EngineView {
    pub slots: Vec<String>,
    pub trigger: Option<String>,
    pub error: Option<String>,
}

/// Reads the `adapters` list of an engine `/props` answer, keyed by folder.
/// The slot of each flag is the engine's own name for that half of its model.
pub fn engine_views(props: &Value, slot_ids: &[&str]) -> BTreeMap<String, EngineView> {
    let mut views = BTreeMap::new();
    for item in props.get("adapters").and_then(Value::as_array).into_iter().flatten() {
        let Some(name) = item.get("name").and_then(Value::as_str) else { continue };
        let slots = slot_ids
            .iter()
            .filter(|slot| item.get(**slot).and_then(Value::as_bool).unwrap_or(false))
            .map(|slot| slot.to_string())
            .collect();
        views.insert(
            name.to_string(),
            EngineView {
                slots,
                trigger: item.get("trigger").and_then(Value::as_str).map(str::to_owned),
                error: item.get("error").and_then(Value::as_str).map(str::to_owned),
            },
        );
    }
    views
}

pub struct AdapterLibrary {
    root: PathBuf,
    engine: String,
    downloader: Downloader,
    /// The catalogue entry downloading now. A set of files reports as one
    /// download, so the file names alone cannot say which entry it is.
    installing: std::sync::Mutex<Option<String>>,
}

impl AdapterLibrary {
    /// The library of one engine's adapters, under the studio data root.
    pub fn new(data_root: &Path, engine: &str) -> Self {
        let root = data_root.join("adapters");
        Self {
            downloader: Downloader::new(root.clone()),
            root,
            engine: engine.to_string(),
            installing: std::sync::Mutex::new(None),
        }
    }

    pub fn installing(&self) -> Option<String> {
        self.installing.lock().ok().and_then(|current| current.clone())
    }

    /// The folder the engine is pointed at.
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn downloader(&self) -> &Downloader {
        &self.downloader
    }

    fn folder(&self, id: &str) -> Result<PathBuf> {
        if id.is_empty() || id.starts_with('.') || id.contains(['/', '\\', ':']) {
            bail!("not an adapter id: {id}");
        }
        Ok(self.root.join(id))
    }

    fn read_meta(&self, id: &str) -> Option<AdapterMeta> {
        let text = fs::read_to_string(self.folder(id).ok()?.join(META)).ok()?;
        serde_json::from_str(&text).ok()
    }

    fn write_meta(&self, meta: &AdapterMeta) -> Result<()> {
        let folder = self.folder(&meta.id)?;
        fs::create_dir_all(&folder)?;
        let temporary = folder.join(format!("{META}.part"));
        fs::write(&temporary, serde_json::to_vec_pretty(meta)?)?;
        fs::rename(&temporary, folder.join(META))?;
        Ok(())
    }

    /// Every adapter of this engine that finished installing, the engine's
    /// view folded in: slots it found are remembered, a complaint is shown.
    pub fn installed(&self, views: Option<&BTreeMap<String, EngineView>>) -> Vec<Installed> {
        let Ok(entries) = fs::read_dir(&self.root) else { return Vec::new() };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let Some(id) = entry.file_name().to_str().map(str::to_owned) else { continue };
            let Some(mut meta) = self.read_meta(&id) else { continue };
            if meta.engine != self.engine {
                continue;
            }
            let mut error = None;
            if let Some(view) = views.and_then(|views| views.get(&id)) {
                error = view.error.clone();
                if error.is_none() && view.slots != meta.slots {
                    meta.slots = view.slots.clone();
                    if meta.trigger.is_none() {
                        meta.trigger = view.trigger.clone();
                    }
                    if let Err(problem) = self.write_meta(&meta) {
                        eprintln!("[ERROR] adapters: could not remember the slots of {id}: {problem}");
                    }
                }
            }
            out.push(Installed { bytes: folder_bytes(&entry.path()), meta, error });
        }
        out.sort_by(|a, b| b.meta.created_at.cmp(&a.meta.created_at));
        out
    }

    pub fn offered(&self) -> Vec<Offered> {
        catalog()
            .iter()
            .filter(|item| item.entry.engine == self.engine)
            .map(|item| Offered {
                id: item.entry.id.clone(),
                name: item.entry.name.clone(),
                description: item.entry.description.clone(),
                kind: item.entry.kind.clone(),
                author: item.entry.author.clone(),
                page: item.entry.page.clone(),
                trigger: item.entry.trigger.clone(),
                slots: item.entry.scales.keys().cloned().collect(),
                bytes: item.entry.files.iter().map(|file| file.bytes).sum(),
                installed: self.read_meta(&item.entry.id).is_some(),
            })
            .collect()
    }

    /// Downloads a catalogue entry and records it. The record is written last,
    /// so a folder without one is an unfinished download the list leaves out.
    pub async fn install(&self, catalog_id: &str) -> Result<()> {
        let item = catalog()
            .iter()
            .find(|item| item.entry.id == catalog_id && item.entry.engine == self.engine)
            .with_context(|| format!("no catalogue adapter {catalog_id}"))?;
        if let Ok(mut current) = self.installing.lock() {
            *current = Some(catalog_id.to_string());
        }
        let downloaded = self.downloader.install_all(SCOPE, &item.assets).await;
        if let Ok(mut current) = self.installing.lock() {
            *current = None;
        }
        downloaded?;
        let entry = &item.entry;
        self.write_meta(&AdapterMeta {
            id: entry.id.clone(),
            engine: self.engine.clone(),
            name: entry.name.clone(),
            description: entry.description.clone(),
            kind: entry.kind.clone(),
            trigger: entry.trigger.clone(),
            author: entry.author.clone(),
            page: entry.page.clone(),
            scales: entry.scales.clone(),
            range: entry.range,
            slots: entry.scales.keys().cloned().collect(),
            origin: Origin::Catalog { catalog_id: entry.id.clone() },
            created_at: now(),
        })
    }

    /// Stores uploaded weight files as a new adapter. An `adapter_config.json`
    /// travels with them, since that is where a PEFT export keeps its alpha;
    /// a `lora.json` beside them may name the trigger word.
    pub fn import(&self, name: &str, files: Vec<(String, Vec<u8>)>, origin: Origin) -> Result<AdapterMeta> {
        let weights: Vec<&(String, Vec<u8>)> = files.iter().filter(|(file, _)| file.ends_with(".safetensors")).collect();
        if weights.is_empty() {
            bail!("an adapter needs at least one .safetensors file");
        }
        let id = format!("{}-{}", slug(name), &uuid::Uuid::now_v7().simple().to_string()[..8]);
        let folder = self.folder(&id)?;
        fs::create_dir_all(&folder)?;
        let mut trigger = None;
        for (file, bytes) in &files {
            let file_name = Path::new(file).file_name().and_then(|value| value.to_str()).context("upload without a name")?;
            match file_name {
                "lora.json" => {
                    trigger = serde_json::from_slice::<Value>(bytes)
                        .ok()
                        .and_then(|value| value.get("trigger").and_then(Value::as_str).map(str::to_owned));
                }
                "adapter_config.json" => fs::write(folder.join(file_name), bytes)?,
                name if name.ends_with(".safetensors") => fs::write(folder.join(name), bytes)?,
                _ => {}
            }
        }
        let meta = AdapterMeta {
            id,
            engine: self.engine.clone(),
            name: Text::Plain(name.trim().to_string()),
            description: Text::default(),
            kind: "other".into(),
            trigger,
            author: None,
            page: None,
            scales: BTreeMap::new(),
            range: None,
            slots: Vec::new(),
            origin,
            created_at: now(),
        };
        self.write_meta(&meta)?;
        Ok(meta)
    }

    pub fn update(&self, id: &str, patch: Patch) -> Result<AdapterMeta> {
        let mut meta = self.read_meta(id).with_context(|| format!("no adapter {id}"))?;
        if let Some(name) = patch.name.map(|name| name.trim().to_string()).filter(|name| !name.is_empty()) {
            meta.name = Text::Plain(name);
        }
        if let Some(trigger) = patch.trigger {
            let trigger = trigger.trim().to_string();
            meta.trigger = (!trigger.is_empty()).then_some(trigger);
        }
        if let Some(scales) = patch.scales {
            meta.scales = scales;
        }
        self.write_meta(&meta)?;
        Ok(meta)
    }

    /// Removes an adapter's folder. Songs made with it keep their audio; only
    /// an exact re-render of them needs it back.
    pub fn remove(&self, id: &str) -> Result<()> {
        let folder = self.folder(id)?;
        if self.read_meta(id).is_none() {
            bail!("no adapter {id}");
        }
        fs::remove_dir_all(&folder).with_context(|| format!("remove {}", folder.display()))
    }

    pub fn exists(&self, id: &str) -> bool {
        self.read_meta(id).is_some_and(|meta| meta.engine == self.engine)
    }
}

fn folder_bytes(folder: &Path) -> u64 {
    fs::read_dir(folder)
        .map(|entries| entries.flatten().filter_map(|entry| entry.metadata().ok()).map(|meta| meta.len()).sum())
        .unwrap_or(0)
}

fn slug(name: &str) -> String {
    let slug: String = name
        .trim()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    let slug = slug.split('-').filter(|part| !part.is_empty()).collect::<Vec<_>>().join("-");
    if slug.is_empty() { "adapter".into() } else { slug.chars().take(40).collect() }
}

fn now() -> String {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library(label: &str) -> AdapterLibrary {
        let root = std::env::temp_dir().join(format!("adapters-test-{label}-{}", uuid::Uuid::now_v7().simple()));
        AdapterLibrary::new(&root, "yue2-cpp")
    }

    #[test]
    fn the_catalogue_is_complete_and_pinned() {
        let entries = catalog();
        assert!(entries.len() > 10);
        for item in entries {
            let entry = &item.entry;
            assert!(!entry.files.is_empty(), "{} has no files", entry.id);
            assert!(!entry.scales.is_empty(), "{} starts at no strength", entry.id);
            for file in &entry.files {
                assert!(file.url.contains("/resolve/") && !file.url.contains("/resolve/main/"), "{} is not pinned", file.url);
                assert!(file.bytes > 0);
                assert!(file.file.ends_with(".safetensors"));
            }
            if let Text::Localized(names) = &entry.name {
                for lang in ["en", "ru", "zh", "ja", "ko"] {
                    assert!(names.contains_key(lang), "{} has no {lang} name", entry.id);
                }
            }
        }
        let mut ids: Vec<&str> = entries.iter().map(|item| item.entry.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), entries.len(), "catalogue ids repeat");
    }

    #[test]
    fn an_import_is_listed_patched_and_removed() {
        let library = library("import");
        let meta = library
            .import(
                "My Voice!",
                vec![
                    ("voice.safetensors".into(), b"weights".to_vec()),
                    ("lora.json".into(), br#"{"trigger":"sv_me"}"#.to_vec()),
                    ("notes.txt".into(), b"ignored".to_vec()),
                ],
                Origin::Imported,
            )
            .unwrap();
        assert!(meta.id.starts_with("my-voice-"));
        assert_eq!(meta.trigger.as_deref(), Some("sv_me"));
        assert!(library.root().join(&meta.id).join("voice.safetensors").is_file());
        assert!(!library.root().join(&meta.id).join("notes.txt").exists());

        let mut views = BTreeMap::new();
        views.insert(meta.id.clone(), EngineView { slots: vec!["ar".into()], trigger: None, error: None });
        let listed = library.installed(Some(&views));
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].meta.slots, vec!["ar".to_string()]);
        // remembered for when the engine is not there to ask
        assert_eq!(library.installed(None)[0].meta.slots, vec!["ar".to_string()]);

        let patched = library.update(&meta.id, Patch { name: Some("Voice".into()), trigger: Some(String::new()), scales: None }).unwrap();
        assert!(matches!(patched.name, Text::Plain(ref name) if name == "Voice"));
        assert_eq!(patched.trigger, None);

        library.remove(&meta.id).unwrap();
        assert!(library.installed(None).is_empty());
        let _ = fs::remove_dir_all(library.root().parent().unwrap());
    }

    #[test]
    fn an_import_without_weights_is_refused_and_ids_cannot_escape() {
        let library = library("refuse");
        assert!(library.import("x", vec![("a.txt".into(), vec![1])], Origin::Imported).is_err());
        assert!(library.folder("../escape").is_err());
        assert!(library.folder("..").is_err());
        assert!(library.remove("missing").is_err());
    }

    #[test]
    fn engine_views_read_the_props_list() {
        let props = serde_json::json!({ "adapters": [
            { "name": "a", "ok": true, "ar": true, "nar": false, "trigger": "t" },
            { "name": "b", "ok": false, "ar": false, "nar": false, "error": "no key" },
        ]});
        let views = engine_views(&props, &["ar", "nar"]);
        assert_eq!(views["a"].slots, vec!["ar".to_string()]);
        assert_eq!(views["a"].trigger.as_deref(), Some("t"));
        assert_eq!(views["b"].error.as_deref(), Some("no key"));
    }
}
