//! Training adapters on the user's own songs, in a sidecar process.
//!
//! The trainer (`music-train`) and its weights are optional: nothing is
//! downloaded until the user asks on the training page. Datasets are engine
//! neutral - a folder of 48 kHz WAV files and a `dataset.json` with each song's
//! style and lyrics - so another studio of the family opens the same folder.
//! Runs are the engine's: the recipe, the stages and the checkpoints come from
//! the engine crate, and a finished checkpoint becomes an ordinary adapter.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use anyhow::{bail, Context, Result};
use music_engine::yue_train::{self, TrainingStep};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::RwLock;

use crate::downloads::{Asset, AssetKind, Downloader};

/// The downloader scope the training page reads its progress under.
pub const SCOPE: &str = "training";

/// One song of a dataset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetItem {
    pub id: String,
    pub title: String,
    /// The style sentence the model is prompted with.
    #[serde(default)]
    pub style: String,
    #[serde(default)]
    pub lyrics: String,
    #[serde(default)]
    pub instrumental: bool,
    /// The WAV file inside the dataset's `audio` folder.
    pub file: String,
    pub seconds: f64,
    /// Where it came from: a library song id or an imported file name.
    #[serde(default)]
    pub source: String,
}

/// A set of songs to train on, independent of any engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dataset {
    #[serde(default = "dataset_format")]
    pub format: String,
    pub id: String,
    pub name: String,
    /// A rare word the adapter learns to answer to.
    #[serde(default)]
    pub trigger: String,
    pub created_at: String,
    #[serde(default)]
    pub items: Vec<DatasetItem>,
}

fn dataset_format() -> String {
    "music-dataset-v1".into()
}

pub use yue_train::Recipe;

/// Separates the vocals of a song for lyric timing; the studio's separator.
pub trait VocalSeparator: Send + Sync {
    /// Writes the vocals of `mix` to `out` as WAV.
    fn separate(&self, mix: &Path, out: &Path) -> Result<()>;
}

/// The studio-side stage that runs before the trainer's.
const VOCALS_STAGE: &str = "vocals";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Done,
    Failed,
    Cancelled,
    /// The studio closed while it ran.
    Interrupted,
}

/// A training run as it is kept on disk and shown on the page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub engine: String,
    pub dataset_id: String,
    pub dataset_name: String,
    pub name: String,
    pub trigger: String,
    pub recipe: Recipe,
    pub status: RunStatus,
    /// The stage working now, or the one that failed.
    pub stage: Option<String>,
    pub stages: Vec<String>,
    #[serde(default)]
    pub steps: Vec<TrainingStepRecord>,
    #[serde(default)]
    pub error: Option<String>,
    pub created_at: String,
    #[serde(default)]
    pub finished_at: Option<String>,
    /// Checkpoints already added to the adapter library, by step.
    #[serde(default)]
    pub installed: Vec<u32>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TrainingStepRecord {
    pub step: u32,
    pub loss: f64,
    #[serde(default)]
    pub ar_kl: Option<f64>,
    #[serde(default)]
    pub step_ms: Option<f64>,
}

impl From<TrainingStep> for TrainingStepRecord {
    fn from(step: TrainingStep) -> Self {
        Self { step: step.step, loss: step.loss, ar_kl: Some(step.ar_kl), step_ms: step.step_ms }
    }
}

pub struct Training {
    root: PathBuf,
    engine: String,
    downloader: Downloader,
    /// The run in progress, with its trainer process to stop.
    active: RwLock<Option<Active>>,
}

struct Active {
    run_id: String,
    cancel: Arc<tokio::sync::Notify>,
}

/// The trainer build this studio uses, shared by every engine of the family.
#[derive(Deserialize)]
struct TrainerSource {
    commit: String,
    shipped_as: String,
    release_tag: String,
    asset: String,
}

fn trainer_source() -> &'static TrainerSource {
    static SOURCE: OnceLock<TrainerSource> = OnceLock::new();
    SOURCE.get_or_init(|| serde_json::from_str(include_str!("../../../engines/music-train-source.json")).expect("engines/music-train-source.json is valid"))
}

/// The folder the trainer archive unpacks into.
const TRAINER_FOLDER: &str = "music-train";

/// Everything training needs: the trainer, released beside the studio, and
/// the engine's weights for it.
fn pack() -> &'static [Asset] {
    static PACK: OnceLock<Vec<Asset>> = OnceLock::new();
    PACK.get_or_init(|| {
        let leak = |text: String| -> &'static str { Box::leak(text.into_boxed_str()) };
        let source = trainer_source();
        let mut assets = vec![Asset {
            id: "music-train",
            label: leak(format!("Trainer (HOT-Step {})", &source.commit[..8])),
            kind: AssetKind::Runtime,
            url: leak(format!("https://github.com/timoncool/YuE2-Studio/releases/download/{}/{}", source.release_tag, source.asset)),
            relative_path: leak(source.asset.clone()),
            bytes: 60_000_000,
            unzip_into: Some(TRAINER_FOLDER),
            marker: leak(source.shipped_as.clone()),
            pick: &[],
            vram_gb: None,
            note: "",
        }];
        assets.extend(yue_train::TRAINING_FILES.iter().map(|file| Asset {
            id: file.id,
            label: file.label,
            kind: AssetKind::Model,
            url: leak(yue_train::training_file_url(file)),
            relative_path: leak(format!("models/{}", file.file)),
            bytes: file.bytes,
            unzip_into: None,
            marker: "",
            pick: &[],
            vram_gb: None,
            note: "",
        }));
        assets
    })
}

fn now() -> String {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs().to_string()).unwrap_or_default()
}

fn new_id() -> String {
    uuid::Uuid::now_v7().simple().to_string()
}

fn safe_id(id: &str) -> Result<&str> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        bail!("not an id: {id}");
    }
    Ok(id)
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let temporary = path.with_extension("json.part");
    std::fs::write(&temporary, serde_json::to_vec_pretty(value)?)?;
    std::fs::rename(&temporary, path)?;
    Ok(())
}

fn read_json<T: for<'a> Deserialize<'a>>(path: &Path) -> Option<T> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

impl Training {
    pub fn new(data_root: &Path, engine: &str) -> Self {
        let root = data_root.join("training");
        Self { downloader: Downloader::new(root.clone()), root, engine: engine.to_string(), active: RwLock::new(None) }
    }

    pub fn downloader(&self) -> &Downloader {
        &self.downloader
    }

    /// The trainer: where `YUE_TRAIN_BIN` points in a developer build, else
    /// the one the pack unpacked.
    pub fn trainer(&self) -> PathBuf {
        std::env::var_os("YUE_TRAIN_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|| self.downloader.runtime_dir(TRAINER_FOLDER).join(&trainer_source().shipped_as))
    }

    pub fn models_dir(&self) -> PathBuf {
        self.root.join("models")
    }

    fn datasets_dir(&self) -> PathBuf {
        self.root.join("datasets")
    }

    fn runs_dir(&self) -> PathBuf {
        self.root.join("runs")
    }

    fn dataset_dir(&self, id: &str) -> Result<PathBuf> {
        Ok(self.datasets_dir().join(safe_id(id)?))
    }

    /// Where a dataset keeps its separated vocals: `<song>/vocals.wav`, the
    /// layout the trainer's aligner reads.
    fn vocals_dir(&self, id: &str) -> Result<PathBuf> {
        Ok(self.dataset_dir(id)?.join("vocals"))
    }

    /// The separated vocals of one dataset song, whether or not made yet.
    pub fn item_vocals(&self, id: &str, item_id: &str) -> Result<PathBuf> {
        let dataset = self.dataset(id)?;
        let item = dataset.items.iter().find(|item| item.id == item_id).with_context(|| format!("no song {item_id} in the dataset"))?;
        Ok(self.vocals_dir(id)?.join(item.file.trim_end_matches(".wav")).join("vocals.wav"))
    }

    /// Songs with lyrics whose vocals are not separated yet.
    fn missing_vocals(&self, dataset: &Dataset) -> Result<Vec<(PathBuf, PathBuf)>> {
        let audio = self.dataset_dir(&dataset.id)?.join("audio");
        let vocals = self.vocals_dir(&dataset.id)?;
        Ok(dataset
            .items
            .iter()
            .filter(|item| !item.instrumental && !item.lyrics.trim().is_empty())
            .map(|item| (audio.join(&item.file), vocals.join(item.file.trim_end_matches(".wav")).join("vocals.wav")))
            .filter(|(_, out)| !out.is_file())
            .collect())
    }

    fn run_dir(&self, id: &str) -> Result<PathBuf> {
        Ok(self.runs_dir().join(safe_id(id)?))
    }

    /// Whether a part of the pack is usable; the trainer counts as present
    /// wherever `trainer` finds it.
    fn installed(&self, asset: &Asset) -> bool {
        if asset.unzip_into == Some(TRAINER_FOLDER) {
            return self.trainer().is_file();
        }
        self.downloader.is_installed(asset)
    }

    /// The pack's files with whether each is on disk.
    pub fn pack_status(&self) -> Vec<serde_json::Value> {
        pack()
            .iter()
            .map(|asset| serde_json::json!({ "id": asset.id, "label": asset.label, "bytes": asset.bytes, "installed": self.installed(asset) }))
            .collect()
    }

    pub fn pack_ready(&self) -> bool {
        pack().iter().all(|asset| self.installed(asset))
    }

    pub async fn install_pack(&self) -> Result<()> {
        let missing: Vec<&'static Asset> = pack().iter().filter(|asset| !self.installed(asset)).collect();
        if missing.is_empty() {
            return Ok(());
        }
        self.downloader.install_all(SCOPE, &missing).await
    }

    // ── datasets ────────────────────────────────────────────────────────────

    pub fn datasets(&self) -> Vec<Dataset> {
        let Ok(entries) = std::fs::read_dir(self.datasets_dir()) else { return Vec::new() };
        let mut list: Vec<Dataset> = entries.flatten().filter_map(|entry| read_json(&entry.path().join("dataset.json"))).collect();
        list.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        list
    }

    pub fn dataset(&self, id: &str) -> Result<Dataset> {
        read_json(&self.dataset_dir(id)?.join("dataset.json")).with_context(|| format!("no dataset {id}"))
    }

    fn save_dataset(&self, dataset: &Dataset) -> Result<()> {
        let dir = self.dataset_dir(&dataset.id)?;
        std::fs::create_dir_all(dir.join("audio"))?;
        write_json(&dir.join("dataset.json"), dataset)
    }

    pub fn create_dataset(&self, name: &str, trigger: &str) -> Result<Dataset> {
        let name = name.trim();
        if name.is_empty() {
            bail!("a dataset needs a name");
        }
        let dataset = Dataset { format: dataset_format(), id: new_id(), name: name.into(), trigger: trigger.trim().into(), created_at: now(), items: Vec::new() };
        self.save_dataset(&dataset)?;
        Ok(dataset)
    }

    pub fn update_dataset(&self, id: &str, name: Option<String>, trigger: Option<String>) -> Result<Dataset> {
        let mut dataset = self.dataset(id)?;
        if let Some(name) = name.map(|name| name.trim().to_string()).filter(|name| !name.is_empty()) {
            dataset.name = name;
        }
        if let Some(trigger) = trigger {
            dataset.trigger = trigger.trim().into();
        }
        self.save_dataset(&dataset)?;
        Ok(dataset)
    }

    /// Adds a song, stored as 48 kHz 24-bit WAV whatever it came as: the
    /// model's own rate, and more depth than any source has.
    pub fn add_item(&self, id: &str, source_audio: &Path, title: &str, style: &str, lyrics: &str, source: &str) -> Result<Dataset> {
        let mut dataset = self.dataset(id)?;
        let audio = crate::audio_pcm::decode_stereo(source_audio)?;
        let audio = if audio.rate == 48_000 { audio } else { audio.resampled(48_000)? };
        let seconds = audio.frames() as f64 / 48_000.0;
        if seconds < 10.0 {
            bail!("{title} is shorter than ten seconds; the trainer cuts songs into ten-second pieces");
        }
        let item_id = new_id();
        let file = format!("{item_id}.wav");
        crate::audio_pcm::write_wav24(&self.dataset_dir(id)?.join("audio").join(&file), &audio)?;
        dataset.items.push(DatasetItem {
            id: item_id,
            title: title.trim().into(),
            style: style.trim().into(),
            lyrics: lyrics.trim().into(),
            instrumental: lyrics.trim().is_empty(),
            file,
            seconds,
            source: source.into(),
        });
        self.save_dataset(&dataset)?;
        Ok(dataset)
    }

    pub fn update_item(&self, id: &str, item_id: &str, patch: ItemPatch) -> Result<Dataset> {
        let mut dataset = self.dataset(id)?;
        let item = dataset.items.iter_mut().find(|item| item.id == item_id).with_context(|| format!("no song {item_id} in the dataset"))?;
        if let Some(title) = patch.title {
            item.title = title.trim().into();
        }
        if let Some(style) = patch.style {
            item.style = style.trim().into();
        }
        if let Some(lyrics) = patch.lyrics {
            item.lyrics = lyrics.trim().into();
        }
        if let Some(instrumental) = patch.instrumental {
            item.instrumental = instrumental;
        }
        self.save_dataset(&dataset)?;
        Ok(dataset)
    }

    /// The stored audio of one song of a dataset.
    pub fn item_audio(&self, id: &str, item_id: &str) -> Result<PathBuf> {
        let dataset = self.dataset(id)?;
        let item = dataset.items.iter().find(|item| item.id == item_id).with_context(|| format!("no song {item_id} in the dataset"))?;
        Ok(self.dataset_dir(id)?.join("audio").join(&item.file))
    }

    pub fn remove_item(&self, id: &str, item_id: &str) -> Result<Dataset> {
        let mut dataset = self.dataset(id)?;
        let position = dataset.items.iter().position(|item| item.id == item_id).with_context(|| format!("no song {item_id} in the dataset"))?;
        let item = dataset.items.remove(position);
        let _ = std::fs::remove_file(self.dataset_dir(id)?.join("audio").join(&item.file));
        let _ = std::fs::remove_dir_all(self.vocals_dir(id)?.join(item.file.trim_end_matches(".wav")));
        self.save_dataset(&dataset)?;
        Ok(dataset)
    }

    pub fn remove_dataset(&self, id: &str) -> Result<()> {
        let dir = self.dataset_dir(id)?;
        if !dir.join("dataset.json").is_file() {
            bail!("no dataset {id}");
        }
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    /// Writes the sidecars the recipe reads beside every song, from what the
    /// dataset says now, so an edit made after import is what trains.
    fn write_sidecars(&self, dataset: &Dataset) -> Result<PathBuf> {
        let audio = self.dataset_dir(&dataset.id)?.join("audio");
        for item in &dataset.items {
            let stem = item.file.trim_end_matches(".wav");
            let lyrics = if item.instrumental { "" } else { item.lyrics.as_str() };
            for (extension, text) in yue_train::sidecars(&item.style, lyrics, item.instrumental) {
                std::fs::write(audio.join(format!("{stem}{extension}")), text)?;
            }
        }
        Ok(audio)
    }

    // ── runs ────────────────────────────────────────────────────────────────

    pub fn runs(&self) -> Vec<Run> {
        let Ok(entries) = std::fs::read_dir(self.runs_dir()) else { return Vec::new() };
        let mut list: Vec<Run> = entries.flatten().filter_map(|entry| read_json(&entry.path().join("run.json"))).filter(|run: &Run| run.engine == self.engine).collect();
        list.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        list
    }

    pub fn run(&self, id: &str) -> Result<Run> {
        read_json(&self.run_dir(id)?.join("run.json")).with_context(|| format!("no training run {id}"))
    }

    fn save_run(&self, run: &Run) -> Result<()> {
        let dir = self.run_dir(&run.id)?;
        std::fs::create_dir_all(&dir)?;
        write_json(&dir.join("run.json"), run)
    }

    /// A run left `running` by a studio that closed is marked interrupted.
    pub fn recover(&self) {
        for mut run in self.runs().into_iter().filter(|run| run.status == RunStatus::Running) {
            run.status = RunStatus::Interrupted;
            run.finished_at = Some(now());
            let _ = self.save_run(&run);
        }
    }

    pub async fn active_run(&self) -> Option<String> {
        self.active.read().await.as_ref().map(|active| active.run_id.clone())
    }

    pub fn checkpoints(&self, run_id: &str) -> Vec<yue_train::TrainingCheckpoint> {
        self.run_dir(run_id).map(|dir| yue_train::checkpoints(&dir)).unwrap_or_default()
    }

    pub fn log_tail(&self, run_id: &str, lines: usize) -> Vec<String> {
        let Ok(text) = self.run_dir(run_id).and_then(|dir| Ok(std::fs::read_to_string(dir.join("run.log"))?)) else { return Vec::new() };
        let all: Vec<&str> = text.lines().filter(|line| !line.trim_start().starts_with('{')).collect();
        all[all.len().saturating_sub(lines)..].iter().map(|line| line.to_string()).collect()
    }

    /// Starts training a dataset; one run at a time, since each wants the card.
    /// `libraries` is where the CUDA runtime the trainer imports lives: the
    /// engine's, fetched on its first start, so it is not downloaded twice.
    pub async fn start(
        self: &Arc<Self>,
        libraries: Option<PathBuf>,
        tokenizer: PathBuf,
        separator: Option<Arc<dyn VocalSeparator>>,
        dataset_id: &str,
        name: &str,
        recipe: Recipe,
    ) -> Result<Run> {
        let trainer = self.trainer();
        if !self.pack_ready() {
            bail!("the training files are not downloaded yet");
        }
        if !trainer.is_file() {
            bail!("the trainer is not installed: {} is missing", trainer.display());
        }
        let dataset = self.dataset(dataset_id)?;
        if dataset.items.is_empty() {
            bail!("the dataset has no songs yet");
        }
        if let Err(problem) = recipe.check() {
            bail!("{problem}");
        }
        let missing = if recipe.lyric_timing { self.missing_vocals(&dataset)? } else { Vec::new() };
        if !missing.is_empty() && separator.is_none() {
            bail!("the vocal separator is not installed; lyric timing needs the vocals of every song with lyrics");
        }
        let mut active = self.active.write().await;
        if active.is_some() {
            bail!("a training run is already going");
        }
        let audio = self.write_sidecars(&dataset)?;
        let run_id = new_id();
        let run_dir = self.run_dir(&run_id)?;
        let inputs = yue_train::TrainingInputs {
            audio,
            models: self.models_dir(),
            tokenizer,
            run: run_dir.clone(),
            vocals: self.vocals_dir(&dataset.id)?,
            trigger: dataset.trigger.clone(),
            recipe: recipe.clone(),
        };
        let stages = yue_train::training_stages(&inputs);
        let run = Run {
            id: run_id.clone(),
            engine: self.engine.clone(),
            dataset_id: dataset.id.clone(),
            dataset_name: dataset.name.clone(),
            name: if name.trim().is_empty() { dataset.name.clone() } else { name.trim().into() },
            trigger: dataset.trigger.clone(),
            recipe,
            status: RunStatus::Running,
            stage: None,
            stages: inputs.recipe.lyric_timing.then_some(VOCALS_STAGE).into_iter().chain(stages.iter().map(|stage| stage.id)).map(str::to_string).collect(),
            steps: Vec::new(),
            error: None,
            created_at: now(),
            finished_at: None,
            installed: Vec::new(),
        };
        self.save_run(&run)?;
        let cancel = Arc::new(tokio::sync::Notify::new());
        *active = Some(Active { run_id: run_id.clone(), cancel: cancel.clone() });
        drop(active);

        let training = self.clone();
        tokio::spawn(async move {
            let outcome = match training.separate_vocals(&run_id, separator, missing, cancel.clone()).await {
                Ok(true) => training.work(&trainer, libraries.as_deref(), &run_dir, &run_id, stages, cancel).await,
                other => other,
            };
            if let Ok(mut run) = training.run(&run_id) {
                run.finished_at = Some(now());
                match outcome {
                    Ok(true) => {
                        run.status = RunStatus::Done;
                        run.stage = None;
                    }
                    Ok(false) => run.status = RunStatus::Cancelled,
                    Err(error) => {
                        run.status = RunStatus::Failed;
                        run.error = Some(format!("{error:#}"));
                    }
                }
                let _ = training.save_run(&run);
            }
            *training.active.write().await = None;
        });
        Ok(run)
    }

    /// Separates the vocals still missing, one song at a time; the result is
    /// kept with the dataset, so the next run and lyric recognition reuse it.
    async fn separate_vocals(&self, run_id: &str, separator: Option<Arc<dyn VocalSeparator>>, missing: Vec<(PathBuf, PathBuf)>, cancel: Arc<tokio::sync::Notify>) -> Result<bool> {
        if missing.is_empty() {
            return Ok(true);
        }
        let mut run = self.run(run_id)?;
        run.stage = Some(VOCALS_STAGE.into());
        self.save_run(&run)?;
        let Some(separator) = separator else { return Ok(true) };
        for (mix, out) in missing {
            let separator = separator.clone();
            let job = tokio::task::spawn_blocking(move || -> Result<()> {
                let folder = out.parent().context("vocals folder")?;
                std::fs::create_dir_all(folder)?;
                let partial = folder.join("vocals.part.wav");
                separator.separate(&mix, &partial)?;
                std::fs::rename(&partial, &out)?;
                Ok(())
            });
            tokio::select! {
                done = job => done.context("vocal separation")?.context("separating the vocals")?,
                _ = cancel.notified() => return Ok(false),
            }
        }
        Ok(true)
    }

    /// Runs the stages in order; `Ok(false)` when cancelled.
    async fn work(&self, trainer: &Path, libraries: Option<&Path>, run_dir: &Path, run_id: &str, stages: Vec<yue_train::TrainingStage>, cancel: Arc<tokio::sync::Notify>) -> Result<bool> {
        use tokio::io::AsyncWriteExt;
        let mut log = tokio::fs::OpenOptions::new().create(true).append(true).open(run_dir.join("run.log")).await?;
        for stage in stages {
            {
                let mut run = self.run(run_id)?;
                run.stage = Some(stage.id.to_string());
                self.save_run(&run)?;
            }
            log.write_all(format!("==== {}\n", stage.id).as_bytes()).await?;
            let mut command = tokio::process::Command::new(trainer);
            command
                .args(&stage.args)
                .current_dir(run_dir)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true);
            if let Some(libraries) = libraries {
                let mut path = std::ffi::OsString::from(libraries.as_os_str());
                if let Some(existing) = std::env::var_os("PATH") {
                    path.push(";");
                    path.push(existing);
                }
                command.env("PATH", path);
            }
            #[cfg(windows)]
            command.creation_flags(0x0800_0000);
            let mut child = command.spawn().with_context(|| format!("start {}", trainer.display()))?;
            let stdout = child.stdout.take().context("trainer output")?;
            let stderr = child.stderr.take().context("trainer errors")?;
            let (lines_out, mut lines_in) = tokio::sync::mpsc::unbounded_channel::<String>();
            for stream in [Box::new(stdout) as Box<dyn tokio::io::AsyncRead + Unpin + Send>, Box::new(stderr)] {
                let lines_out = lines_out.clone();
                tokio::spawn(async move {
                    let mut lines = BufReader::new(stream).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        if lines_out.send(line).is_err() {
                            break;
                        }
                    }
                });
            }
            drop(lines_out);
            let mut last_error = String::new();
            let status = loop {
                tokio::select! {
                    line = lines_in.recv() => match line {
                        Some(line) => {
                            log.write_all(line.as_bytes()).await?;
                            log.write_all(b"\n").await?;
                            if let Some(step) = yue_train::parse_training_step(&line) {
                                let mut run = self.run(run_id)?;
                                run.steps.push(step.into());
                                self.save_run(&run)?;
                            } else if line.to_ascii_lowercase().contains("error") {
                                last_error = line;
                            }
                        }
                        None => break child.wait().await?,
                    },
                    _ = cancel.notified() => {
                        let _ = child.kill().await;
                        return Ok(false);
                    }
                }
            };
            if !status.success() {
                bail!("{} stopped ({status}){}", stage.id, if last_error.is_empty() { String::new() } else { format!(": {last_error}") });
            }
        }
        Ok(true)
    }

    pub async fn cancel(&self, run_id: &str) -> Result<()> {
        let active = self.active.read().await;
        match active.as_ref() {
            Some(active) if active.run_id == run_id => {
                active.cancel.notify_one();
                Ok(())
            }
            _ => bail!("run {run_id} is not going"),
        }
    }

    pub fn mark_installed(&self, run_id: &str, step: u32) -> Result<()> {
        let mut run = self.run(run_id)?;
        if !run.installed.contains(&step) {
            run.installed.push(step);
        }
        self.save_run(&run)
    }

    pub async fn remove_run(&self, run_id: &str) -> Result<()> {
        if self.active_run().await.as_deref() == Some(run_id) {
            bail!("stop the run before removing it");
        }
        let dir = self.run_dir(run_id)?;
        if !dir.join("run.json").is_file() {
            bail!("no training run {run_id}");
        }
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }
}

/// What an edit of a dataset song may change.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ItemPatch {
    pub title: Option<String>,
    pub style: Option<String>,
    pub lyrics: Option<String>,
    pub instrumental: Option<bool>,
}

/// Lyrics from a `.txt` or `.lrc` file: time stamps and word timings removed,
/// metadata tags such as `[ar:...]` dropped, section tags kept.
pub fn plain_lyrics(text: &str) -> String {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut rest = line.trim();
        while let Some(stripped) = rest.strip_prefix('[') {
            let Some(end) = stripped.find(']') else { break };
            let tag = &stripped[..end];
            let timed = tag.chars().next().is_some_and(|c| c.is_ascii_digit());
            let meta = tag.contains(':') && !timed;
            if !(timed || meta) {
                break;
            }
            rest = stripped[end + 1..].trim_start();
        }
        let mut clean = String::new();
        let mut inside = false;
        for c in rest.chars() {
            match c {
                '<' => inside = true,
                '>' if inside => inside = false,
                _ if !inside => clean.push(c),
                _ => {}
            }
        }
        out.push(clean.trim().to_string());
    }
    out.join("\n").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lrc_files_become_plain_lyrics() {
        let lrc = "[ar:Someone]\n[00:01.00]First <00:01.50>line\n\n[Chorus]\n[00:05.20]Second line";
        assert_eq!(plain_lyrics(lrc), "First line\n\n[Chorus]\nSecond line");
        assert_eq!(plain_lyrics("[Verse 1]\nplain"), "[Verse 1]\nplain");
    }
}
