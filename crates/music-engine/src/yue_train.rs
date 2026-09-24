//! How YuE2 adapters are trained: the weights the trainer needs, the stages it
//! runs and what they print.
//!
//! The trainer is HOT-Step's `ace-train`, built from a pinned commit and
//! shipped as `music-train`. This module knows only YuE2's side of it - which
//! files, which subcommands with which arguments, how a checkpoint looks - so
//! the studio's training page stays the same for every engine and another
//! engine brings a recipe of its own.
//!
//! A dataset reaches the trainer as a folder of 48 kHz WAV files, each with a
//! `<stem>.txt` sidecar (`caption:`, `is_instrumental:`, `lyrics:` last) and a
//! `<stem>.yue2.txt` holding the one-line style the planner is prompted with.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::Serialize;

/// The Hugging Face repository and commit the training weights come from.
pub const WEIGHTS_REPOSITORY: &str = "scragnog/YuE2-GGUF";
pub const WEIGHTS_REVISION: &str = "eb7de0903bf4dfbd14a2384ae0c0d14150cfa2c7";

/// One file of the training pack, stored flat in the training models folder.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct TrainingFile {
    pub id: &'static str,
    pub label: &'static str,
    /// The path inside the repository.
    pub source: &'static str,
    /// The name it is stored under, the one the trainer looks for.
    pub file: &'static str,
    pub bytes: u64,
}

pub const TRAINING_FILES: &[TrainingFile] = &[
    TrainingFile {
        id: "yue2-convrot-base",
        label: "YuE2 base for training (INT8 ConvRot)",
        source: "checkpoints/yue2_3b_int8_convrot.safetensors",
        file: "yue2_3b_int8_convrot.safetensors",
        bytes: 3_960_938_800,
    },
    TrainingFile {
        id: "yue2-vae-encoder",
        label: "YuE2 VAE with its encoder",
        source: "yue2-vae-standard-f32.gguf",
        file: "yue2-vae-standard-f32.gguf",
        bytes: 530_348_768,
    },
    TrainingFile {
        id: "yue2-semantic-tokenizer",
        label: "Semantic tokenizer",
        source: "yue2-tok-f16.gguf",
        file: "yue2-tok-f16.gguf",
        bytes: 1_211_632_224,
    },
    TrainingFile {
        id: "yue2-sheetsage",
        label: "SheetSage2 score transcriber",
        source: "sheetsage2-f16.gguf",
        file: "sheetsage2-f16.gguf",
        bytes: 1_360_020_736,
    },
    TrainingFile {
        id: "yue2-lyric-aligner",
        label: "MMS forced aligner (lyric timing)",
        source: "mms-fa/mms-fa-f32.gguf",
        file: "mms-fa-f32.gguf",
        bytes: 1_261_892_736,
    },
];

pub fn training_file_url(file: &TrainingFile) -> String {
    format!("https://huggingface.co/{WEIGHTS_REPOSITORY}/resolve/{WEIGHTS_REVISION}/{}", file.source)
}

/// What a run is asked to do.
#[derive(Debug, Clone)]
pub struct TrainingInputs {
    /// The folder of WAV files and sidecars.
    pub audio: PathBuf,
    /// The training models folder, holding [`TRAINING_FILES`].
    pub models: PathBuf,
    /// A YuE2 GGUF the studio already has, read for its text tokenizer.
    pub tokenizer: PathBuf,
    /// The run's own folder; every stage writes below it.
    pub run: PathBuf,
    /// Holds `<song>/vocals.wav` for every song with lyrics, the audio the
    /// lyric timing is aligned against.
    pub vocals: PathBuf,
    pub trigger: String,
    /// A cap: the run stops earlier once the planner has moved `target_kl`
    /// away from the base model.
    pub steps: u32,
    pub save_every: u32,
    pub seed: u32,
    /// 0 trains all the steps.
    pub target_kl: f64,
}

/// HOT-Step's joint recipe as its training page sends it (Yue2AitkTrainCard
/// DEFAULT_FORM at the pinned commit); the trainer's own flag defaults are the
/// older baseline and are not what the author tuned by ear.
pub mod recipe {
    pub const STEPS: u32 = 750;
    pub const SAVE_EVERY: u32 = 50;
    pub const SEED: u32 = 42;
    pub const TARGET_KL: f64 = 1.4;
    pub const OPTIMIZER: &str = "prodigy";
    pub const PRODIGY_D0: &str = "1e-6";
    pub const RANK: &str = "64";
    pub const ALPHA: &str = "256";
    pub const LOKR_DIM: &str = "64";
    pub const LOKR_FACTOR: &str = "4";
    /// The planner learns at 0.3 of the renderer's rate: likeness lives in the
    /// renderer, and a planner at full rate memorises the songs.
    pub const PLANNER_LR_SCALE: &str = "0.3";
    pub const CURSOR_WEIGHT: &str = "0.08";
}

/// One trainer invocation.
#[derive(Debug, Clone)]
pub struct TrainingStage {
    /// The stage's name for the interface to translate.
    pub id: &'static str,
    pub args: Vec<OsString>,
}

fn path(value: &Path) -> OsString {
    value.as_os_str().to_owned()
}

/// The trainer's stages of a run, in order: latents, semantic codes, lyric
/// timing, scores, the joint dataset, then training. The vocal stems the
/// timing stage reads are the studio's to separate before these start.
pub fn training_stages(inputs: &TrainingInputs) -> Vec<TrainingStage> {
    let models = &inputs.models;
    let cache = inputs.run.join("cache");
    let manifest = cache.join("yue2_preprocess.json");
    let prepared = inputs.run.join("prepared");
    let arg = |text: &str| OsString::from(text);
    let model = |name: &str, file: &str| OsString::from(format!("{name}={}", models.join(file).display()));
    let mut train = vec![
        arg("yue2-joint-train"),
        arg("--checkpoint"),
        path(&models.join(TRAINING_FILES[0].file)),
        arg("--dataset"),
        path(&prepared.join("dataset.json")),
        arg("--output"),
        path(&inputs.run.join("output")),
        arg("--steps"),
        arg(&inputs.steps.to_string()),
        arg("--save-every"),
        arg(&inputs.save_every.max(1).to_string()),
        arg("--seed"),
        arg(&inputs.seed.to_string()),
        arg("--device"),
        arg("CUDA0"),
        arg("--rank"),
        arg(recipe::RANK),
        arg("--alpha"),
        arg(recipe::ALPHA),
        arg("--adapter-type"),
        arg("lokr"),
        arg("--lokr-dim"),
        arg(recipe::LOKR_DIM),
        arg("--lokr-factor"),
        arg(recipe::LOKR_FACTOR),
        arg("--optimizer"),
        arg(recipe::OPTIMIZER),
        arg("--prodigy-d0"),
        arg(recipe::PRODIGY_D0),
        arg("--planner-lr-scale"),
        arg(recipe::PLANNER_LR_SCALE),
        arg("--cursor-weight"),
        arg(recipe::CURSOR_WEIGHT),
    ];
    if inputs.target_kl > 0.0 {
        train.extend([arg("--target-kl"), arg(&inputs.target_kl.to_string())]);
    }
    let mut prepare = vec![
        arg("yue2-prepare-aitk"),
        arg("--legacy-manifest"),
        path(&manifest),
        arg("--checkpoint"),
        path(&models.join(TRAINING_FILES[0].file)),
        arg("--tokenizer"),
        path(&inputs.tokenizer),
        arg("--output"),
        path(&prepared),
        arg("--model"),
        model("vae", TRAINING_FILES[1].file),
        arg("--model"),
        model("semantic", TRAINING_FILES[2].file),
        arg("--model"),
        model("sheetsage", TRAINING_FILES[3].file),
        arg("--lyric-timing"),
        arg("1"),
    ];
    if !inputs.trigger.trim().is_empty() {
        prepare.extend([arg("--trigger"), arg(inputs.trigger.trim())]);
    }
    vec![
        TrainingStage {
            id: "latents",
            args: vec![
                arg("yue2-preprocess"),
                arg("--audio"),
                path(&inputs.audio),
                arg("--out"),
                path(&cache),
                arg("--models"),
                path(models),
                arg("--vae"),
                arg("standard"),
                arg("--caption-mode"),
                arg("yue2"),
            ],
        },
        TrainingStage { id: "codes", args: vec![arg("yue2-tokenize"), arg("--manifest"), path(&manifest), arg("--models"), path(models)] },
        TrainingStage {
            id: "align",
            args: vec![
                arg("yue2-align"),
                arg("--manifest"),
                path(&manifest),
                arg("--mmsfa"),
                path(&models.join(TRAINING_FILES[4].file)),
                arg("--stems"),
                path(&inputs.vocals),
            ],
        },
        TrainingStage { id: "scores", args: vec![arg("yue2-sheet"), arg("--manifest"), path(&manifest), arg("--models"), path(models)] },
        TrainingStage { id: "prepare", args: prepare },
        TrainingStage { id: "train", args: train },
    ]
}

/// The two sidecars a song needs beside its audio.
pub fn sidecars(style: &str, lyrics: &str, instrumental: bool) -> [(&'static str, String); 2] {
    let style = style.split_whitespace().collect::<Vec<_>>().join(" ");
    [
        (".txt", format!("caption: {style}\nis_instrumental: {instrumental}\nlyrics:\n{}\n", lyrics.trim())),
        (".yue2.txt", format!("{style}\n")),
    ]
}

/// A step of the training stage, as the trainer reports it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct TrainingStep {
    pub step: u32,
    /// The combined loss HOT-Step charts: AR cross-entropy, 0.2 x AR KL and
    /// the NAR flow error.
    pub loss: f64,
    /// How far the composition half has moved from the base model; it keeps
    /// climbing once the adapter starts memorising the dataset.
    pub ar_kl: f64,
    pub step_ms: Option<f64>,
}

/// Reads one line of the training stage's output; progress lines are JSON.
pub fn parse_training_step(line: &str) -> Option<TrainingStep> {
    let value: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    if value.get("stage")?.as_str()? != "joint" {
        return None;
    }
    let number = |key: &str| value.get(key).and_then(serde_json::Value::as_f64).filter(|value| value.is_finite());
    Some(TrainingStep {
        step: value.get("step")?.as_u64()? as u32,
        loss: number("ar_ce")? + 0.2 * number("ar_kl")? + number("nar_mse")?,
        ar_kl: number("ar_kl")?,
        step_ms: number("step_ms"),
    })
}

/// A finished checkpoint: its step and the adapter files generation uses.
#[derive(Debug, Clone, Serialize)]
pub struct TrainingCheckpoint {
    pub step: u32,
    pub files: Vec<PathBuf>,
}

/// The checkpoints a run has written so far, the latest first. A folder
/// counts once both native exports are in it.
pub fn checkpoints(run: &Path) -> Vec<TrainingCheckpoint> {
    let Ok(entries) = std::fs::read_dir(run.join("output")) else { return Vec::new() };
    let mut found: Vec<TrainingCheckpoint> = entries
        .flatten()
        .filter_map(|entry| {
            let step = entry.file_name().to_str()?.strip_prefix("checkpoint-step")?.parse().ok()?;
            let files: Vec<PathBuf> = ["native-ar.safetensors", "native-nar.safetensors"].iter().map(|name| entry.path().join(name)).collect();
            files.iter().all(|file| file.is_file()).then_some(TrainingCheckpoint { step, files })
        })
        .collect();
    found.sort_by(|a, b| b.step.cmp(&a.step));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_joint_line_becomes_a_step_and_other_lines_do_not() {
        let step = parse_training_step(r#"{"stage":"joint","step":12,"ar_ce":2.0,"ar_kl":0.5,"nar_mse":0.3,"step_ms":800}"#).unwrap();
        assert_eq!(step.step, 12);
        assert!((step.loss - 2.4).abs() < 1e-9);
        assert_eq!(step.ar_kl, 0.5);
        assert_eq!(step.step_ms, Some(800.0));
        assert!(parse_training_step(r#"{"stage":"AR transformer backward","step":3}"#).is_none());
        assert!(parse_training_step("[YuE2] Unloaded").is_none());
    }

    #[test]
    fn stages_run_in_order_and_train_last() {
        let inputs = TrainingInputs {
            audio: "a".into(),
            models: "m".into(),
            tokenizer: "t.gguf".into(),
            run: "r".into(),
            vocals: "v".into(),
            trigger: "sks".into(),
            steps: 750,
            save_every: 50,
            seed: 42,
            target_kl: 1.4,
        };
        let stages = training_stages(&inputs);
        assert_eq!(stages.iter().map(|stage| stage.id).collect::<Vec<_>>(), ["latents", "codes", "align", "scores", "prepare", "train"]);
        let strings = |index: usize| -> Vec<String> { stages[index].args.iter().map(|arg| arg.to_string_lossy().into_owned()).collect() };
        let train = strings(5);
        assert_eq!(train[0], "yue2-joint-train");
        for pair in [["--optimizer", "prodigy"], ["--adapter-type", "lokr"], ["--planner-lr-scale", "0.3"], ["--target-kl", "1.4"], ["--cursor-weight", "0.08"]] {
            assert!(train.windows(2).any(|window| window == pair), "{pair:?}");
        }
        assert!(!train.iter().any(|arg| arg == "--lr"), "Prodigy sets its own step size");
        let prepare = strings(4);
        assert!(prepare.windows(2).any(|pair| pair == ["--trigger", "sks"]));
        assert!(prepare.windows(2).any(|pair| pair == ["--lyric-timing", "1"]));
        assert!(strings(2).windows(2).any(|pair| pair == ["--stems", "v"]));
    }

    #[test]
    fn sidecars_put_lyrics_last() {
        let [(ext, text), (style_ext, style)] = sidecars("indie  pop,\nfemale", "[Verse]\nla", false);
        assert_eq!(ext, ".txt");
        assert!(text.starts_with("caption: indie pop, female\n"));
        assert!(text.ends_with("lyrics:\n[Verse]\nla\n"));
        assert_eq!((style_ext, style.as_str()), (".yue2.txt", "indie pop, female\n"));
    }
}
