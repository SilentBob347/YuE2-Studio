mod adapters;
mod processing;
mod training;
mod auto_title;
mod tagging;
mod cover_prompt;
mod providers;
mod assistant;
mod assistant_runtime;
mod audio_pcm;
mod downloads;
mod engine_runtime;
mod lyrics_sync;
mod credentials;
mod model_manager;
mod hardware;
mod request_log;
mod resources;
mod chunked;
mod separation;
mod sizes;
mod skill;
mod library;
mod engine_result;
mod progress;

use std::{collections::HashMap, env, fs, net::SocketAddr, path::PathBuf, sync::Arc};
use anyhow::Context;
use futures_util::StreamExt;

use axum::{
    extract::{DefaultBodyLimit, Multipart, Path, Query, State},
    body::Body,
    http::{header, HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use music_core::{Capability, EngineDescriptor, ExecutionMode, StudioConfiguration};
use model_manager::{InstallRequest, ModelManager};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::RwLock;
use tower_http::{cors::CorsLayer, trace::TraceLayer};

const PRIMARY_MUSIC_ENGINE_ID: &str = model_manager::ENGINE_ID;

#[derive(Clone)]
struct AppState {
    configuration: Arc<RwLock<StudioConfiguration>>,
    jobs: Arc<RwLock<HashMap<String, MusicJob>>>,
    music_server: EngineClient,
    model_manager: ModelManager,
    selected_profile_id: Arc<RwLock<Option<String>>>,
    selected_component_ids: Arc<RwLock<Option<Vec<String>>>>,
    settings_path: PathBuf,
    openrouter_catalog: Arc<RwLock<OpenRouterCatalogState>>,
    library: library::Library,
    /// Owned local engine process, when this service started one.
    engine: Arc<tokio::sync::Mutex<Option<music_engine::yue_server::YueServerSupervisor>>>,
    engine_options: Arc<RwLock<EngineOptions>>,
    /// The CUDA libraries the engine binary imports. They are downloaded, not
    /// installed, so the engine cannot start until they are on disk.
    engine_runtime: Arc<engine_runtime::EngineRuntime>,
    assistant: Arc<RwLock<AssistantConfig>>,
    assistant_runtime: Arc<assistant_runtime::AssistantRuntime>,
    lyrics_sync: Arc<lyrics_sync::LyricsSync>,
    lyrics_sync_config: Arc<RwLock<lyrics_sync::LyricsSyncConfig>>,
    /// Saved cover looks, filled in from whichever track a cover is for.
    cover_templates: Arc<RwLock<Vec<cover_prompt::CoverTemplate>>>,
    /// The look a new cover starts from, chosen in Settings.
    cover_template_default: Arc<RwLock<Option<String>>>,
    separator: Arc<separation::Separator>,
    separation_config: Arc<RwLock<separation::SeparationConfig>>,
    /// Draw a cover as soon as a track is finished.
    cover_auto: Arc<RwLock<bool>>,
    /// What is being done to finished tracks right now - covers, karaoke - so
    /// the interface can say it instead of leaving the user guessing.
    activity: Arc<RwLock<Vec<Activity>>>,
    /// The separation run in progress, if any. One at a time: the model wants
    /// the whole machine for a minute, and two runs would only make both slow.
    separation_run: Arc<RwLock<Option<SeparationRun>>>,
    /// LoRA adapters for the local engine, and the example catalogue.
    adapters: Arc<adapters::AdapterLibrary>,
    /// The processing run in progress or the last one, with its preview.
    processing_run: Arc<RwLock<Option<processing::ProcessRun>>>,
    /// Adapter training: its optional weights, datasets and runs.
    training: Arc<training::Training>,
}

#[derive(Clone)]
struct EngineClient {
    base_url: String,
    http: reqwest::Client,
    /// The last health answer and when it was taken. Every status poll asks,
    /// and on Windows a refused loopback connect takes about two seconds, so
    /// with the engine down the polls queued up behind each other.
    health_cache: Arc<std::sync::Mutex<Option<(std::time::Instant, bool)>>>,
}

/// One autoregressive stage's sampling preset. Every knob is optional: an
/// absent one is the checkpoint value the engine applies.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
struct SamplingPreset {
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_k: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    repetition_penalty: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    penalty_window: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    min_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
}

impl SamplingPreset {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The protocol bounds yue-server enforces, checked here so the user is
    /// told which knob is wrong instead of reading a bare 400.
    fn validate(&self, label: &str) -> Result<(), String> {
        if self.temperature.is_some_and(|value| !(0.0..=5.0).contains(&value)) {
            return Err(format!("{label}: temperature must be between 0 and 5"));
        }
        if self.top_p.is_some_and(|value| !(value > 0.0 && value <= 1.0)) {
            return Err(format!("{label}: top_p must be above 0 and at most 1"));
        }
        if self.top_k.is_some_and(|value| value < 1) {
            return Err(format!("{label}: top_k must be at least 1"));
        }
        if self.repetition_penalty.is_some_and(|value| !(value > 0.0 && value.is_finite())) {
            return Err(format!("{label}: repetition_penalty must be positive"));
        }
        if self.penalty_window.is_some_and(|value| !(1..=100).contains(&value)) {
            return Err(format!("{label}: penalty_window must be between 1 and 100"));
        }
        if self.max_tokens.is_some_and(|value| value < 1) {
            return Err(format!("{label}: max_tokens must be at least 1"));
        }
        if let (Some(min), Some(max)) = (self.min_tokens, self.max_tokens) {
            if min > max {
                return Err(format!("{label}: min_tokens cannot exceed max_tokens"));
            }
        }
        Ok(())
    }
}

/// A YuE2 generation request, in the engine's own vocabulary. Fields left
/// out are the engine's protocol defaults.
#[derive(Debug, Clone, Default, Deserialize)]
struct CreateMusicJobRequest {
    /// Comma-separated style tags, verbatim under `[Tags]`.
    #[serde(default)]
    style: String,
    /// Lyrics with their structural tags, verbatim under `[Lyrics]`.
    #[serde(default)]
    lyrics: String,
    /// ABC score to realise; empty lets the model write one.
    abc: Option<String>,
    /// Chain-of-thought mode: `full`, `melody` or `off`.
    cot: Option<String>,
    /// Target length in seconds; the model may end the song earlier.
    duration_seconds: Option<f64>,
    lm_seed: Option<i64>,
    seed: Option<i64>,
    steps: Option<u32>,
    lm_batch_size: Option<u32>,
    synth_batch_size: Option<u32>,
    cfg_scale: Option<f64>,
    /// Comma-separated semantic codes; present means the AR stage is skipped.
    semantic_tokens: Option<String>,
    abc_sampling: Option<SamplingPreset>,
    semantic_sampling: Option<SamplingPreset>,
    peak_clip: Option<i32>,
    output_format: Option<String>,
    mp3_bitrate: Option<u32>,
    /// Library title only, never sent to the engine.
    title: Option<String>,
    /// What the cover should show, when the assistant described it.
    cover_prompt: Option<String>,
    /// Installed adapters to merge for this song, in order.
    #[serde(default)]
    adapters: Vec<AdapterUse>,
}

/// One adapter of a request: its folder and a strength per engine slot. A slot
/// left out is not changed.
#[derive(Debug, Clone, Default, Deserialize)]
struct AdapterUse {
    id: String,
    #[serde(default)]
    scales: std::collections::BTreeMap<String, f64>,
}

/// The name this request goes into the library under: the user's, or one
/// taken from the song when they left the field empty.
fn titled(request: &CreateMusicJobRequest) -> String {
    request
        .title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| auto_title::auto_title(&request.style, &request.lyrics, request.lyrics.trim().is_empty()))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
enum MusicJobStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
enum MusicJobDispatch {
    NotConfigured,
    Local,
    Cancelled,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
enum MusicJobPhase {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize)]
struct MusicJob {
    id: String,
    engine_id: String,
    /// What the assistant said this track's cover should show, if anything.
    #[serde(skip_serializing_if = "Option::is_none")]
    cover_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    status: MusicJobStatus,
    dispatch: MusicJobDispatch,
    phase: MusicJobPhase,
    style: String,
    lyrics: String,
    duration_seconds: f64,
    generation_settings: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    song: Option<CompletedSong>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    songs: Vec<CompletedSong>,
    message: String,
}

#[derive(Debug, Clone, Serialize)]
struct CompletedSong {
    id: String,
    song: library::Song,
    audio_url: String,
}

#[derive(Debug, Serialize)]
struct LocalMusicModelCatalog {
    engine_id: String,
    catalog: Value,
}

#[derive(Debug, Serialize)]
struct CapabilitiesResponse {
    engines: Vec<EngineDescriptor>,
}

#[derive(Debug, Serialize)]
struct ApiError {
    error: String,
}

#[derive(Debug, Deserialize)]
struct EngineSubmitResponse {
    id: String,
}

#[derive(Debug, Deserialize)]
struct EngineJobResponse {
    status: String,
}

struct EngineResultResponse {
    content_type: String,
    body: Vec<u8>,
}

#[derive(Debug, Deserialize)]
struct SetupSelectRequest {
    #[serde(default)]
    profile_id: Option<String>,
    #[serde(default)]
    component_ids: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct SetupDownloadRequest {
    // The panel calls this field `component_ids`. Reading only `ids` meant a
    // download request arrived empty, and an empty request quietly fell back to
    // the default set - which is how pressing "download" on the 11.9 GB set
    // started fetching the 26.6 GB one.
    #[serde(default, alias = "component_ids")]
    ids: Vec<String>,
    profile_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ReplayMusicJobRequest {
    song_id: Option<String>,
    replay_request: Option<Value>,
    steps: Option<u32>,
    seed: Option<i64>,
    synth_batch_size: Option<u32>,
    output_format: Option<String>,
    peak_clip: Option<i32>,
    mp3_bitrate: Option<u32>,
    /// A title for the re-render; the source track's own name otherwise.
    title: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistedStudioSettings {
    #[serde(default)]
    engine_options: EngineOptions,
    #[serde(default)]
    assistant: AssistantConfig,
    lyrics_sync: lyrics_sync::LyricsSyncConfig,
    configuration: StudioConfiguration,
    selected_profile_id: Option<String>,
    #[serde(default)]
    selected_component_ids: Option<Vec<String>>,
    #[serde(default)]
    cover_templates: Option<Vec<cover_prompt::CoverTemplate>>,
    #[serde(default)]
    cover_template_default: Option<String>,
    #[serde(default)]
    separation: Option<separation::SeparationConfig>,
    /// Whether a finished track gets its cover drawn without being asked.
    #[serde(default)]
    cover_auto: Option<bool>,
}

#[derive(Default)]
struct OpenRouterCatalogState {
    catalog: Option<providers::openrouter::CapabilityCatalog>,
    refreshed_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterTranscriptionRequest {
    model_id: String,
    audio_base64: String,
    audio_format: String,
    language: Option<String>,
}

/// Launch flags for the local engine process. They belong to the running
/// engine, so changing one restarts it.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
struct EngineOptions {
    backend: music_engine::yue_server::ComputeBackend,
    keep_loaded: bool,
    max_batch: Option<u32>,
    max_seq: Option<u32>,
    vae_core: Option<u32>,
    vae_halo: Option<u32>,
    disable_flash_attention: bool,
    clamp_fp16: bool,
}

impl EngineOptions {
    /// Songs one request may draw, and the `--max-batch` the engine starts
    /// with. Each song reserves a KV set, so nothing is reserved unasked.
    /// Whether the engine will compute on CUDA and so needs cuBLAS: chosen
    /// outright, or left to ggml on a machine with an NVIDIA card.
    fn uses_cuda(&self) -> bool {
        use music_engine::yue_server::ComputeBackend;
        match self.backend {
            ComputeBackend::Cuda => true,
            ComputeBackend::Auto => hardware::hardware().nvidia,
            ComputeBackend::Vulkan | ComputeBackend::Cpu => false,
        }
    }

    /// Whether the engine will compute on Vulkan: chosen outright, or left to
    /// ggml on a machine whose card is not NVIDIA.
    fn uses_vulkan(&self) -> bool {
        use music_engine::yue_server::ComputeBackend;
        match self.backend {
            ComputeBackend::Vulkan => true,
            ComputeBackend::Auto => {
                let hardware = hardware::hardware();
                !hardware.nvidia && hardware.gpu_name.is_some()
            }
            ComputeBackend::Cuda | ComputeBackend::Cpu => false,
        }
    }

    fn effective_max_batch(&self) -> u32 {
        self.max_batch.unwrap_or(1).max(1)
    }

    fn to_engine(self) -> music_engine::yue_server::YueServerOptions {
        music_engine::yue_server::YueServerOptions {
            backend: self.backend,
            keep_loaded: self.keep_loaded,
            max_batch: Some(self.effective_max_batch()),
            max_seq: self.max_seq,
            vae_core: self.vae_core,
            vae_halo: self.vae_halo,
            disable_flash_attention: self.disable_flash_attention,
            // On an AMD Radeon through Vulkan the hidden states leave the FP16
            // range and the song comes out as pure silence, which the engine's
            // MP3 path then crashes on; the engine's own clamp fixes it.
            clamp_fp16: self.clamp_fp16 || self.uses_vulkan(),
        }
    }
}

/// Where the optional writing assistant runs.
///
/// `None` is the default and a first-class state: the manual form is the
/// primary way to use this model, and on a modest card nobody wants a language
/// model competing for VRAM. Nothing is downloaded or started unless the user
/// picks a provider.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
struct AssistantConfig {
    provider: AssistantProvider,
    /// Base URL of an OpenAI-compatible server (llama.cpp, LM Studio, Ollama).
    local_base_url: Option<String>,
    local_model: Option<String>,
    openrouter_model: Option<String>,
    /// Id of a model downloaded through the assistant runtime, run as a
    /// sidecar by Studio itself.
    managed_model: Option<String>,
    /// A GGUF already on this machine, run by the same sidecar. Machines that
    /// already keep a Gemma around for another tool do not need a second copy.
    managed_path: Option<String>,
    /// How hard a reasoning model should think, in OpenRouter's own terms:
    /// minimal, low, medium, high, xhigh, max - or none.
    reasoning_effort: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AssistantProvider {
    #[default]
    None,
    Local,
    OpenRouter,
    /// A model Studio downloaded and runs itself with llama.cpp.
    Managed,
}

impl AssistantConfig {
    fn available(&self) -> bool {
        match self.provider {
            AssistantProvider::None => false,
            AssistantProvider::Local => {
                self.local_base_url.as_deref().is_some_and(|url| !url.trim().is_empty())
                    && self.local_model.as_deref().is_some_and(|model| !model.trim().is_empty())
            }
            AssistantProvider::OpenRouter => {
                self.openrouter_model.as_deref().is_some_and(|model| !model.trim().is_empty())
                    && credentials::openrouter_source().is_some()
            }
            // Availability is confirmed against the disk in `assistant_status`;
            // a model id alone only says one was chosen.
            AssistantProvider::Managed => {
                self.managed_model.as_deref().is_some_and(|model| !model.trim().is_empty())
                    || self.managed_path.as_deref().is_some_and(|path| !path.trim().is_empty())
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct ProxyImageRequest {
    url: String,
}

#[derive(Debug, Deserialize)]
struct OpenRouterSettingsRequest {
    /// `None` or an empty string clears the locally stored credential.
    api_key: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterCompletionRequest {
    model_id: String,
    prompt: String,
}

#[derive(Debug, Deserialize)]
struct OpenRouterCoverRequest {
    model_id: String,
    prompt: String,
}

#[derive(Debug, Serialize)]
struct OpenRouterResponse {
    body: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation_id: Option<String>,
}

/// Runs the studio service until the process is asked to stop.
///
/// This is a library entry point on purpose: the desktop application hosts it
/// in-process, so a release is a single executable rather than a launcher that
/// has to start a second binary and keep it alive.
pub async fn serve() -> anyhow::Result<()> {
    let settings_path = studio_settings_path();
    let persisted = load_studio_settings(&settings_path);
    let model_manager = ModelManager::from_environment()?;
    let persisted_components = persisted
        .as_ref()
        .and_then(|settings| settings.selected_component_ids.clone())
        .filter(|ids| model_manager.installed_component_files(ids).is_ok());
    let (selected_profile_id, selected_component_ids) = match persisted_components {
        Some(ids) => match model_manager::profile_matching(&ids) {
            Some(profile) => (Some(profile.to_owned()), None),
            None => (None, Some(ids)),
        },
        None => (
            persisted
                .as_ref()
                .and_then(|settings| settings.selected_profile_id.clone())
                .or_else(|| Some(hardware::recommended_local_profile().into())),
            None,
        ),
    };
    let state = AppState {
        configuration: Arc::new(RwLock::new(sanitize_persisted_configuration(
            persisted.as_ref().map(|settings| settings.configuration.clone()).unwrap_or_else(initial_configuration),
        ))),
        jobs: Arc::new(RwLock::new(HashMap::new())),
        music_server: EngineClient::from_environment(),
        model_manager,
        cover_templates: Arc::new(RwLock::new(
            persisted
                .as_ref()
                .and_then(|settings| settings.cover_templates.clone())
                .filter(|templates| !templates.is_empty())
                .unwrap_or_else(cover_prompt::default_templates),
        )),
        cover_template_default: Arc::new(RwLock::new(
            persisted.as_ref().and_then(|settings| settings.cover_template_default.clone()),
        )),
        activity: Arc::new(RwLock::new(Vec::new())),
        cover_auto: Arc::new(RwLock::new(
            persisted.as_ref().and_then(|settings| settings.cover_auto).unwrap_or(false),
        )),
        separation_config: Arc::new(RwLock::new(
            persisted.as_ref().and_then(|settings| settings.separation.clone()).unwrap_or_default(),
        )),
        separator: Arc::new(separation::Separator::new(
            &studio_data_root().unwrap_or_else(|| std::path::PathBuf::from(".")),
        )),
        separation_run: Arc::new(RwLock::new(None)),
        adapters: Arc::new(adapters::AdapterLibrary::new(
            &studio_data_root().unwrap_or_else(|| std::path::PathBuf::from(".")),
            PRIMARY_MUSIC_ENGINE_ID,
        )),
        processing_run: Arc::new(RwLock::new(None)),
        training: Arc::new(training::Training::new(
            &studio_data_root().unwrap_or_else(|| std::path::PathBuf::from(".")),
            PRIMARY_MUSIC_ENGINE_ID,
        )),
        selected_profile_id: Arc::new(RwLock::new(selected_profile_id)),
        selected_component_ids: Arc::new(RwLock::new(selected_component_ids)),
        settings_path,
        openrouter_catalog: Arc::new(RwLock::new(OpenRouterCatalogState::default())),
        library: library::Library::open_default()?,
        engine: Arc::new(tokio::sync::Mutex::new(None)),
        engine_options: Arc::new(RwLock::new(persisted.as_ref().map(|settings| settings.engine_options).unwrap_or_default())),
        engine_runtime: Arc::new(engine_runtime::EngineRuntime::new(&engine_bundle_root())),
        assistant: Arc::new(RwLock::new(persisted.as_ref().map(|settings| settings.assistant.clone()).unwrap_or_default())),
        assistant_runtime: Arc::new(assistant_runtime::AssistantRuntime::new(
            &studio_data_root().unwrap_or_else(|| std::path::PathBuf::from(".")),
        )),
        lyrics_sync: Arc::new(lyrics_sync::LyricsSync::new(
            &studio_data_root().unwrap_or_else(|| std::path::PathBuf::from(".")),
        )),
        lyrics_sync_config: Arc::new(RwLock::new(
            persisted.as_ref().map(|settings| settings.lyrics_sync.clone()).unwrap_or_default(),
        )),
    };
    processing::clear_workspace(state.library.media_dir());
    state.training.recover();

    let app = Router::new()
        .route("/health", get(health))
        .route("/v1/capabilities", get(capabilities))
        .route("/v1/configuration", get(configuration).put(update_configuration))
        .route("/engine/options", get(engine_options).put(update_engine_options))
        .route("/engine/restart", post(restart_local_engine))
        .route("/v1/engine/logs", get(engine_logs))
        .route("/v1/system/resources", get(system_resources))
        .route("/v1/proxy/image", get(proxy_image))
        .route("/v1/openrouter/settings", get(openrouter_settings).put(update_openrouter_settings))
        .route("/v1/openrouter/logs", get(openrouter_logs))
        .route("/v1/assistant/status", get(assistant_status).put(update_assistant_settings))
        .route("/v1/assistant/local-models", get(assistant_local_models))
        .route("/v1/assistant/write", post(assistant_write))
        .route("/v1/assistant/write/stream", post(assistant_write_stream))
        .route("/v1/assistant/runtime", get(assistant_runtime_status))
        .route("/v1/assistant/runtime/install", post(assistant_runtime_install))
        .route("/v1/assistant/runtime/cancel", post(cancel_assistant_download))
        .route("/v1/assistant/runtime/remove", post(assistant_runtime_remove))
        .route("/v1/assistant/runtime/start", post(assistant_runtime_start))
        .route("/v1/assistant/runtime/stop", post(assistant_runtime_stop))
        .route("/v1/karaoke/status", get(karaoke_status).put(update_karaoke_settings))
        .route("/v1/karaoke/install", post(karaoke_install))
        .route("/v1/karaoke/cancel", post(cancel_karaoke_download))
        .route("/v1/karaoke/remove", post(karaoke_remove))
        .route("/v1/library/songs/{id}/karaoke", post(create_song_karaoke).delete(delete_song_karaoke))
        .route("/v1/openrouter/catalog", get(openrouter_catalog))
        .route("/v1/openrouter/catalog/refresh", post(refresh_openrouter_catalog))
        .route("/v1/openrouter/transcriptions", post(create_openrouter_transcription))
        .route("/v1/openrouter/covers", post(create_openrouter_cover))
        .route("/editor", get(|| async { axum::response::Redirect::permanent("/editor/index.html") }))
        .route("/editor/{*path}", get(editor_asset))
        .route("/v1/separation/runtime", get(separation_assets))
        .route("/v1/separation/runtime/install", post(install_separation_asset))
        .route("/v1/separation/runtime/cancel", post(cancel_separation_download))
        .route("/v1/separation/status", get(separation_status))
        .route("/v1/separation/settings", get(read_separation_settings).put(write_separation_settings))
        .route("/v1/separation/install", post(install_separation_model))
        .route("/v1/separation/remove", post(remove_separation_model))
        .route("/v1/adapters", get(list_adapters))
        .route("/v1/adapters/import", post(import_adapter))
        .route("/v1/adapters/cancel", post(cancel_adapter_download))
        .route("/v1/adapters/install", post(install_catalog_adapters))
        .route("/v1/adapters/hub", get(search_hub_adapters))
        .route("/v1/adapters/hub/files", get(list_hub_files))
        .route("/v1/adapters/hub/install", post(install_hub_adapters))
        .route("/v1/adapters/{id}", axum::routing::patch(update_adapter).delete(delete_adapter))
        .route("/v1/library/songs/{id}/process", post(start_processing))
        .route("/v1/library/songs/{id}/version", axum::routing::put(select_song_version))
        .route("/v1/library/songs/{id}/versions/{version}", axum::routing::delete(remove_song_version))
        .route("/v1/processing", get(read_processing))
        .route("/v1/processing/preview", get(processing_preview))
        .route("/v1/processing/keep", post(keep_processing))
        .route("/v1/processing/discard", post(discard_processing))
        .route("/v1/processing/reference", post(upload_processing_reference))
        .route("/v1/training", get(read_training))
        .route("/v1/training/pack/install", post(install_training_pack))
        .route("/v1/training/pack/cancel", post(cancel_training_pack))
        .route("/v1/training/datasets", post(create_training_dataset))
        .route("/v1/training/datasets/{id}", axum::routing::patch(update_training_dataset).delete(delete_training_dataset))
        .route("/v1/training/datasets/{id}/songs", post(add_training_songs))
        .route("/v1/training/datasets/{id}/files", post(upload_training_files))
        .route("/v1/training/datasets/{id}/items/{item}", axum::routing::patch(update_training_item).delete(delete_training_item))
        .route("/v1/training/datasets/{id}/items/{item}/autofill", post(autofill_training_item))
        .route("/v1/training/runs", post(start_training))
        .route("/v1/training/runs/{id}/cancel", post(cancel_training))
        .route("/v1/training/runs/{id}", axum::routing::delete(delete_training_run))
        .route("/v1/training/runs/{id}/checkpoints/{step}/install", post(install_training_checkpoint))
        .route("/v1/library/songs/{id}/stems", get(read_stems).post(start_separation))
        .route("/v1/library/songs/{id}/stems/{stem}", get(read_stem_audio))
        .route("/v1/library/songs/{id}/cover/auto", post(draw_cover_now))
        .route("/v1/activity", get(read_activity))
        .route("/v1/cover-templates", get(read_cover_templates).put(write_cover_templates))
        .route("/v1/cover-templates/render", post(render_cover_template))
        .route("/v1/openrouter/completions", post(create_openrouter_completion))
        .route("/v1/library/songs", get(library_songs).post(create_library_song))
        .route("/v1/library/import", post(import_library_audio))
        .route("/v1/library/songs/{id}", get(library_song).put(update_library_song).delete(delete_library_song))
        .route("/v1/library/media/{song_id}", get(library_media))
        .route("/v1/library/songs/{id}/cover", get(library_cover).put(store_library_cover))
        .route("/v1/library/playlists", get(library_playlists).post(create_library_playlist))
        .route("/v1/library/playlists/{id}", get(library_playlist).put(update_library_playlist).delete(delete_library_playlist))
        .route("/setup/status", get(setup_status))
        .route("/setup/catalog", get(setup_catalog))
        .route("/setup/download", post(setup_download))
        .route("/setup/remove", post(setup_remove))
        .route("/setup/adopt", post(setup_adopt))
        .route("/v1/open-data-directory", post(open_data_directory))
        .route("/setup/select", post(setup_select))
        .route("/setup/cancel", post(setup_cancel))
        .route("/v1/local-models/music", get(local_music_model_catalog))
        .route("/v1/music/jobs", post(create_music_job).get(list_active_music_jobs))
        .route("/v1/music/replay", post(replay_music_job))
        .route("/v1/transcriptions", post(create_transcription))
        .route("/v1/transcriptions/{job_id}", get(score_job_status).post(cancel_score_job))
        .route("/v1/scores", post(compose_score))
        .route("/v1/scores/{job_id}", get(score_job_status).post(cancel_score_job))
        .route(
            "/v1/music/jobs/{job_id}",
            get(music_job_status).post(cancel_music_job),
        )
        .with_state(state.clone())
        // Covers and imported audio are megabytes, not kilobytes. The default
        // two-megabyte cap rejected a generated cover by dropping the
        // connection, which reaches the interface as "Failed to fetch".
        .layer(DefaultBodyLimit::max(256 * 1024 * 1024))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http());

    // The provider catalog is public and small; reading it once at startup
    // means the settings panel is right the first time it is opened, instead
    // of after the user presses a refresh button.
    // Start the engine as soon as a complete set is installed. It takes about
    // three seconds; making the user press a button for it - or worse, wait
    // without knowing what for - is the studio being lazy on their time.
    // The engine is supervised, not started once and forgotten. It used to be
    // launched a single time at startup, and only if a complete set of weights
    // was already on disk - so a first installation downloaded its models,
    // nothing started them, and the window waited on "loading the models into
    // memory" until the studio was restarted by hand. The same gap swallowed a
    // crashed engine. This watches instead: whenever a complete set is on disk
    // and nothing is answering on the engine port, it brings the engine up.
    {
        let state = state.clone();
        tokio::spawn(async move {
            let mut complained = false;
            // Whether the engine was up on the last look. Only a fall from up to
            // down is a crash worth a line; an engine that has never started is
            // a startup that is failing, and repeating "stopped answering" every
            // two seconds is what filled a whole log with one sentence.
            let mut was_running = false;
            loop {
                let ready = state.model_manager.status(effective_install_target(&state).await).await.ready;
                let running = state.music_server.health().await;
                if ready && !running {
                    if was_running {
                        // It was answering and now it is not: the one line that
                        // explains a log which suddenly starts again from
                        // "Listening on". Written once, not once a cycle.
                        music_engine::yue_server::note_in_log("the engine stopped answering; restarting it");
                    }
                    match restart_engine(&state).await {
                        Ok(()) => complained = false,
                        Err(error) => {
                            // Say it once per failure, not once every few
                            // seconds: a card with too little memory would
                            // otherwise fill the log with the same line.
                            if !complained {
                                eprintln!("the local engine did not start: {error}");
                                complained = true;
                            }
                        }
                    }
                }
                was_running = running;
                tokio::time::sleep(std::time::Duration::from_secs(if running { 5 } else { 2 })).await;
            }
        });
    }

    let address = SocketAddr::from(([127, 0, 0, 1], listen_port()));
    let listener = tokio::net::TcpListener::bind(address).await?;
    println!("music-server listening on http://{address}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

async fn library_songs(State(state): State<AppState>) -> Result<Json<Vec<library::Song>>, (StatusCode, Json<ApiError>)> { state.library.list_songs().map(Json).map_err(|e|api_error(StatusCode::INTERNAL_SERVER_ERROR,e.to_string())) }
async fn library_song(State(state): State<AppState>,Path(id):Path<String>)->Result<Json<library::Song>,(StatusCode,Json<ApiError>)>{state.library.get_song(&id).map_err(|e|api_error(StatusCode::INTERNAL_SERVER_ERROR,e.to_string()))?.map(Json).ok_or_else(||api_error(StatusCode::NOT_FOUND,"Song not found".into()))}
async fn library_media(State(state): State<AppState>, Path(song_id): Path<String>, headers: HeaderMap) -> Result<axum::response::Response, (StatusCode, Json<ApiError>)> {
    let song = state.library.get_song(&song_id).map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?.ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Song not found".into()))?;
    let path = state.library.media_path_for_song(&song).ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Song audio is not available in the studio media library".into()))?;
    // Tracks made before the studio tagged anything have no ID3 at all, and a
    // download of one lands in a player as an untitled file. Tag it on the way
    // out, once: the check is three bytes.
    if path.extension().and_then(|value| value.to_str()).map(str::to_ascii_lowercase).as_deref() == Some("mp3")
        && tokio::fs::read(&path).await.map(|bytes| bytes.get(..3) != Some(b"ID3")).unwrap_or(false)
    {
        tag_stored_song(&state, &song_id).await;
    }
    serve_audio_file(&path, &headers).await
}

/// An audio file with single byte-range support, which `<audio>` seeking needs.
async fn serve_audio_file(path: &std::path::Path, headers: &HeaderMap) -> Result<axum::response::Response, (StatusCode, Json<ApiError>)> {
    let bytes = tokio::fs::read(path).await.map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("read song audio: {error}")))?;
    let content_type = match path.extension().and_then(|extension| extension.to_str()).map(|extension| extension.to_ascii_lowercase()).as_deref() {
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        _ => return Err(api_error(StatusCode::UNSUPPORTED_MEDIA_TYPE, "Stored song has an unsupported audio extension".into())),
    };
    let total = bytes.len();
    let range = headers.get(header::RANGE).and_then(|value| value.to_str().ok()).and_then(|value| parse_single_byte_range(value, total));
    let response = if let Some((start, end)) = range {
        axum::response::Response::builder()
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{total}"))
            .header(header::CONTENT_LENGTH, end - start + 1)
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CONTENT_TYPE, content_type)
            .body(Body::from(bytes[start..=end].to_vec()))
    } else if headers.contains_key(header::RANGE) {
        axum::response::Response::builder()
            .status(StatusCode::RANGE_NOT_SATISFIABLE)
            .header(header::CONTENT_RANGE, format!("bytes */{total}"))
            .body(Body::empty())
    } else {
        axum::response::Response::builder()
            .header(header::CONTENT_TYPE, content_type)
            .header(header::CONTENT_LENGTH, total)
            .header(header::ACCEPT_RANGES, "bytes")
            .body(Body::from(bytes))
    };
    Ok(response.expect("valid audio response"))
}

/// Parses the one byte-range form used by HTMLAudioElement. Multiple ranges are
/// intentionally declined; a single 206 keeps native seeking interoperable.
fn parse_single_byte_range(value: &str, total: usize) -> Option<(usize, usize)> {
    let value = value.strip_prefix("bytes=")?;
    if value.contains(',') || total == 0 { return None; }
    let (start, end) = value.split_once('-')?;
    if start.is_empty() {
        let suffix = end.parse::<usize>().ok()?;
        if suffix == 0 { return None; }
        return Some((total.saturating_sub(suffix), total - 1));
    }
    let start = start.parse::<usize>().ok()?;
    if start >= total { return None; }
    let end = if end.is_empty() { total - 1 } else { end.parse::<usize>().ok()?.min(total - 1) };
    (end >= start).then_some((start, end))
}
#[derive(Debug, Deserialize)]
struct StoreCoverRequest {
    /// Raw base64 image bytes, without a data-URL prefix.
    image_base64: String,
    media_type: String,
}

/// Cover art is Studio-side metadata: a track keeps working without one, so a
/// missing cover is a 404 the UI answers with its generated placeholder art
/// rather than an error state.
#[derive(Debug, Deserialize)]
struct CoverTemplatesRequest {
    templates: Vec<cover_prompt::CoverTemplate>,
    /// Draw a cover as soon as a track finishes.
    #[serde(default)]
    auto: Option<bool>,
    /// Which of them a new cover starts from. `None` leaves it as it was.
    #[serde(default)]
    default_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RenderCoverPromptRequest {
    template: String,
    /// The track the prompt is for. Without it the placeholders have nothing
    /// to stand in for, which is only useful for previewing the wording.
    song_id: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    style: Option<String>,
    #[serde(default)]
    lyrics: Option<String>,
}


/// The waveform editor, carried inside the binary.
///
/// It is a static web application; embedding it keeps the promise that the
/// studio is one executable, and serving it over the studio's own port means
/// the browser can open it with the track already loaded.
static EDITOR: include_dir::Dir<'_> = include_dir::include_dir!("$CARGO_MANIFEST_DIR/../../app/public/editor");

async fn editor_asset(Path(path): Path<String>) -> Result<axum::response::Response, (StatusCode, Json<ApiError>)> {
    let file = EDITOR
        .get_file(path.trim_start_matches('/'))
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, format!("the editor has no file {path}")))?;
    let media_type = match std::path::Path::new(&path).extension().and_then(|value| value.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("mp3") => "audio/mpeg",
        Some("mp4") => "video/mp4",
        Some("wasm") => "application/wasm",
        _ => "application/octet-stream",
    };
    Ok(axum::response::Response::builder()
        .header(header::CONTENT_TYPE, media_type)
        .header(header::CONTENT_LENGTH, file.contents().len())
        .body(Body::from(file.contents().to_vec()))
        .expect("valid editor response"))
}

/// One separation in progress, as the interface sees it.
#[derive(Debug, Clone, Serialize)]
struct SeparationRun {
    song_id: String,
    /// Between 0 and 1.
    progress: f64,
    done: bool,
    error: Option<String>,
    stems: Vec<String>,
    /// Whether the graphics card did the work, once the run is over.
    used_gpu: Option<bool>,
}

/// Where a song's stems live: beside the track, named after it.
fn stem_path(state: &AppState, song_id: &str, stem: &str) -> PathBuf {
    state.library.media_dir().join(format!("{song_id}-{stem}.wav"))
}

fn stems_on_disk(state: &AppState, song_id: &str) -> Vec<String> {
    separation::STEMS
        .iter()
        .filter(|stem| stem_path(state, song_id, stem).is_file())
        .map(|stem| (*stem).to_string())
        .collect()
}

/// The separator as an optional module: its files, and whichever download is
/// running. The same envelope the assistant and karaoke use, because the models
/// page lists all three the same way.
async fn separation_assets(State(state): State<AppState>) -> Json<Value> {
    let runtime_installed = state.lyrics_sync.onnxruntime_library().is_some();
    let assets = serde_json::json!([
        {
            "id": separation::MODEL.id,
            "label": separation::MODEL.label,
            "bytes": separation::MODEL.bytes,
            "note": separation::MODEL.note,
            "installed": state.separator.is_installed(),
        },
        {
            "id": "onnxruntime-cuda",
            "label": "ONNX Runtime 1.30.0 · CUDA",
            "bytes": 379_723_801u64,
            "note": "The CUDA build of the runtime.",
            "installed": state.lyrics_sync.has_cuda_runtime(),
        },
        {
            "id": "cuda-cublas",
            "label": "NVIDIA cuBLAS 12.9",
            "bytes": 549_731_131u64,
            "note": "The linear algebra the CUDA provider is built on.",
            "installed": state.lyrics_sync.downloader().runtime_dir("onnx-cuda").join("cublasLt64_12.dll").is_file(),
        },
        {
            "id": "cuda-cudart",
            "label": "NVIDIA CUDA runtime 12.9",
            "bytes": 3_521_238u64,
            "note": "The CUDA runtime itself.",
            "installed": state.lyrics_sync.downloader().runtime_dir("onnx-cuda").join("cudart64_12.dll").is_file(),
        },
        {
            "id": "cuda-cudnn",
            "label": "NVIDIA cuDNN 9.25",
            "bytes": 1_904_452_100u64,
            "note": "The convolution kernels the separator spends its time in.",
            "installed": state.lyrics_sync.downloader().runtime_dir("onnx-cuda").join("cudnn64_9.dll").is_file(),
        },
        {
            "id": "onnxruntime",
            "label": "ONNX Runtime 1.30.0",
            "bytes": 82_645_522,
            "note": "Runs the separator and the karaoke recogniser; shared between them.",
            "installed": runtime_installed,
        }
    ]);
    let config = state.separation_config.read().await.clone();
    let mut set: Vec<&'static lyrics_sync::Asset> = Vec::new();
    if let Some(asset) = lyrics_sync::asset("onnxruntime") { set.push(asset); }
    if !matches!(config.runtime, lyrics_sync::OnnxFlavour::Cpu) {
        set.extend(CARD_ASSETS.iter().filter_map(|id| lyrics_sync::asset(id)));
    }
    let runtime_progress = set_progress(state.lyrics_sync.downloader(), &set);
    let model_installed = state.separator.is_installed();
    let bytes = runtime_progress["bytes"].as_u64().unwrap_or(0) + separation::MODEL.bytes;
    let installed_bytes = runtime_progress["installed_bytes"].as_u64().unwrap_or(0)
        + if model_installed { separation::MODEL.bytes } else { 0 };
    Json(serde_json::json!({
        "assets": assets,
        "settings": { "runtime": config.runtime },
        "set": {
            "bytes": bytes,
            "installed_bytes": installed_bytes,
            "ready": installed_bytes == bytes,
            "files": set.len() + 1,
        },
        // Only this panel's own download. The recogniser shares this
        // downloader, and its gigabytes are not the separator's business.
        "active_download": state.separator.downloader().active_for("separation").await.or(state.lyrics_sync.downloader().active_for("separation").await),
        // Not an error and not a stall: the file server is asking us to wait.
        "waiting_for_server": crate::chunked::waiting_for_server(),
    }))
}

#[derive(Debug, Deserialize)]
struct InstallSeparationAssetRequest {
    asset_id: String,
}

/// Everything the card path needs, in the order it is used. `karaoke_set`
/// builds a recogniser out of these plus its own model files, so this is the
/// one place the CUDA provider's parts are named.
const CARD_ASSETS: [&str; 5] = ["onnxruntime-cuda", "cuda-cudart", "cuda-cublas", "cuda-cufft", "cuda-cudnn"];

/// Stops a download. What arrived stays on disk: pressing this again later
/// carries on from the last finished piece rather than starting the file over.
///
/// Two panels can be downloading at once, and until this existed the only way
/// to stop one of them was to close the studio.
/// Points `ort` at the ONNX Runtime the studio installed, once per process.
///
/// It has to happen before anything touches `ort`, or the library binds to
/// whatever `onnxruntime.dll` the system happens to have - and on Windows a
/// DLL's own dependencies are resolved through the process search path, not
/// through the folder it came from, which is why the directory joins PATH too.
/// Every caller needs this, not only the separator: reading a track through the
/// same runtime without it left the request waiting on a library that was never
/// located.
fn point_ort_at(runtime: &std::path::Path) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    let runtime = runtime.to_path_buf();
    ONCE.call_once(|| unsafe {
        std::env::set_var("ORT_DYLIB_PATH", &runtime);
        if let Some(directory) = runtime.parent() {
            let existing = std::env::var("PATH").unwrap_or_default();
            std::env::set_var("PATH", format!("{};{existing}", directory.display()));
        }
    });
}

async fn cancel_separation_download(State(state): State<AppState>) -> Json<Value> {
    state.separator.downloader().cancel();
    state.lyrics_sync.downloader().cancel();
    Json(serde_json::json!({ "cancelled": true }))
}

async fn cancel_karaoke_download(State(state): State<AppState>) -> Json<Value> {
    state.lyrics_sync.downloader().cancel();
    Json(serde_json::json!({ "cancelled": true }))
}

/// Frees the disk an assistant takes: the model, the runtime and any half of
/// either. Every other capability could be removed from its panel; this one
/// could only be added.
async fn assistant_runtime_remove(
    State(state): State<AppState>,
    Json(request): Json<AssistantAssetRequest>,
) -> Result<Json<assistant_runtime::RuntimeStatus>, (StatusCode, Json<ApiError>)> {
    // "managed" from the panel means the whole thing: whichever llama.cpp build
    // is on disk, and the model that was chosen with it.
    let ids: Vec<String> = if request.asset_id == "managed" || request.asset_id == "cuda" || request.asset_id == "cpu" {
        let chosen = state.assistant.read().await.managed_model.clone();
        ["llama-cuda", "llama-cuda-runtime", "llama-cpu"].iter().map(|id| id.to_string()).chain(chosen).collect()
    } else {
        vec![request.asset_id.clone()]
    };
    for id in &ids {
        state
            .assistant_runtime
            .remove(id)
            .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?;
    }
    {
        let mut assistant = state.assistant.write().await;
        assistant.managed_model = None;
    }
    let _ = persist_studio_settings(&state).await;
    Ok(Json(state.assistant_runtime.status().await))
}

async fn cancel_assistant_download(State(state): State<AppState>) -> Json<Value> {
    state.assistant_runtime.cancel();
    Json(serde_json::json!({ "cancelled": true }))
}

async fn install_separation_asset(
    State(state): State<AppState>,
    Json(request): Json<InstallSeparationAssetRequest>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    // "card" means whatever is still missing for the graphics card, one after
    // another: asking someone to press four buttons in the right order is not a
    // setup, it is a quiz.
    // The separator as one thing: its model, the runtime that loads it, and -
    // for the card - the CUDA provider. Six rows of file names asked the user
    // to work out which of them belong together.
    if matches!(request.asset_id.as_str(), "auto" | "cuda" | "cpu") {
        let card = !matches!(request.asset_id.as_str(), "cpu");
        let separator = state.separator.clone();
        let sync = state.lyrics_sync.clone();
        let mut runtime: Vec<&'static lyrics_sync::Asset> = Vec::new();
        if let Some(asset) = lyrics_sync::asset("onnxruntime") { runtime.push(asset); }
        if card {
            runtime.extend(CARD_ASSETS.iter().filter_map(|id| lyrics_sync::asset(id)));
        }
        tokio::spawn(async move {
            if let Err(error) = separator.downloader().install_all("separation", &[&separation::MODEL]).await {
                eprintln!("the separator model could not be installed: {error}");
                return;
            }
            if let Err(error) = sync.downloader().install_all("separation", &runtime).await {
                eprintln!("the separator runtime could not be installed: {error}");
            }
        });
        return Ok(Json(serde_json::json!({ "started": true })));
    }
    if request.asset_id == "card" {
        let sync = state.lyrics_sync.clone();
        let card: Vec<&'static lyrics_sync::Asset> = CARD_ASSETS.iter().filter_map(|id| lyrics_sync::asset(id)).collect();
        tokio::spawn(async move {
            if let Err(error) = sync.downloader().install_all("separation", &card).await {
                eprintln!("the card path could not be installed: {error}");
            }
        });
        return Ok(Json(serde_json::json!({ "started": true })));
    }
    // Anything in the catalogue may be installed by name; listing the ids here
    // by hand is how cuFFT ended up silently rejected.
    // `install` starts a background task and returns immediately; its Result
    // says whether the download was accepted at all. Discarding it inside a
    // spawn - which is what this did - meant a refusal ("another download is
    // already running") was thrown away while the endpoint answered
    // "started: true". The button then did nothing, twice, silently, with a
    // successful reply, and no amount of error handling in the interface could
    // have shown it.
    if request.asset_id == separation::MODEL.id {
        state
            .separator
            .downloader()
            .install(&separation::MODEL)
            .await
            .map_err(|error| api_error(StatusCode::CONFLICT, error.to_string()))?;
    } else if let Some(asset) = lyrics_sync::asset(&request.asset_id) {
        state
            .lyrics_sync
            .downloader()
            .install(asset)
            .await
            .map_err(|error| api_error(StatusCode::CONFLICT, error.to_string()))?;
    } else {
        return Err(api_error(StatusCode::BAD_REQUEST, format!("unknown asset {}", request.asset_id)));
    }
    Ok(Json(serde_json::json!({ "started": true })))
}

async fn read_separation_settings(State(state): State<AppState>) -> Json<separation::SeparationConfig> {
    Json(state.separation_config.read().await.clone())
}

async fn write_separation_settings(
    State(state): State<AppState>,
    Json(config): Json<separation::SeparationConfig>,
) -> Result<Json<separation::SeparationConfig>, (StatusCode, Json<ApiError>)> {
    // A run that writes nothing is a run nobody wanted; an empty choice means
    // everything, which is also what the studio starts with.
    let stems: Vec<String> = if config.stems.is_empty() {
        separation::STEMS.iter().map(|stem| (*stem).to_string()).collect()
    } else {
        config.stems.iter().filter(|stem| separation::STEMS.contains(&stem.as_str())).cloned().collect()
    };
    let stored = separation::SeparationConfig { runtime: config.runtime, stems, overlap: config.sane_overlap() };
    *state.separation_config.write().await = stored.clone();
    persist_studio_settings(&state)
        .await
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(Json(stored))
}

async fn separation_status(State(state): State<AppState>) -> Json<Value> {
    let runtime = state.lyrics_sync.onnxruntime_library();
    Json(serde_json::json!({
        "model": {
            "id": separation::MODEL.id,
            "label": separation::MODEL.label,
            "bytes": separation::MODEL.bytes,
            "note": separation::MODEL.note,
            "installed": state.separator.is_installed(),
        },
        "runtime_installed": runtime.is_some(),
        "cuda_runtime_installed": state.lyrics_sync.has_cuda_libraries(),
        "card_missing_bytes": CARD_ASSETS
            .iter()
            .filter_map(|id| lyrics_sync::asset(id))
            .filter(|asset| !state.lyrics_sync.downloader().is_installed(asset))
            .map(|asset| asset.bytes)
            .sum::<u64>(),
        "ready": state.separator.ready(runtime.as_deref()),
        "stems": separation::STEMS,
        // Either downloader may be the busy one: the model has its own, the
        // card libraries come through karaoke's. Reporting only the first is
        // what made a running download look like a dead button.
        "download": match state.separator.downloader().active().await {
            Some(active) if !active.done => Some(active),
            other => match state.lyrics_sync.downloader().active().await {
                Some(active) if !active.done => Some(active),
                fallback => fallback.or(other),
            },
        },
        "settings": state.separation_config.read().await.clone(),
        "run": state.separation_run.read().await.clone(),
    }))
}

/// Fetches the separation model. Nothing here downloads on its own; this is the
/// button, and it also brings the runtime if karaoke has not already.
async fn install_separation_model(State(state): State<AppState>) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let separator = state.separator.clone();
    let sync = state.lyrics_sync.clone();
    tokio::spawn(async move {
        if sync.onnxruntime_library().is_none() {
            if let Some(runtime) = lyrics_sync::asset("onnxruntime") {
                let _ = sync.downloader().install(runtime).await;
            }
        }
        let _ = separator.downloader().install(&separation::MODEL).await;
    });
    Ok(Json(serde_json::json!({ "started": true })))
}

/// The adapter page: the parts of the model an adapter can change, the
/// installed adapters with what the engine found in each, the catalogue, and
/// the download in progress. The engine is asked only when it is already up;
/// a stopped engine leaves the slots the studio remembered.
async fn list_adapters(State(state): State<AppState>) -> Json<Value> {
    let slot_ids: Vec<&str> = music_engine::yue_server::ADAPTER_SLOTS.iter().map(|slot| slot.id).collect();
    let views = match tokio::time::timeout(std::time::Duration::from_secs(3), state.music_server.props()).await {
        Ok(Ok(props)) => Some(adapters::engine_views(&props, &slot_ids)),
        _ => None,
    };
    Json(serde_json::json!({
        "slots": music_engine::yue_server::ADAPTER_SLOTS,
        "installed": state.adapters.installed(views.as_ref()),
        "catalog": state.adapters.offered(),
        "engine_checked": views.is_some(),
        "download": state.adapters.downloader().active_for(adapters::SCOPE).await,
        "installing": state.adapters.installing(),
    }))
}

#[derive(Debug, Deserialize)]
struct InstallAdaptersRequest {
    /// Catalogue entries to fetch as one download.
    ids: Vec<String>,
}

async fn install_catalog_adapters(
    State(state): State<AppState>,
    Json(input): Json<InstallAdaptersRequest>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    state.adapters.begin_install(&input.ids).map_err(|error| api_error(StatusCode::CONFLICT, error.to_string()))?;
    Ok(Json(serde_json::json!({ "started": true })))
}

#[derive(Debug, Deserialize)]
struct HubQuery {
    #[serde(default)]
    q: String,
    #[serde(default)]
    repo: String,
}

async fn search_hub_adapters(State(state): State<AppState>, Query(query): Query<HubQuery>) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let found = state.adapters.hub_search(&sizes::client(), &query.q).await.map_err(|error| api_error(StatusCode::BAD_GATEWAY, format!("{error:#}")))?;
    Ok(Json(serde_json::json!({ "repos": found })))
}

/// The weight files of a repository; `repo` may be an id or any link into it,
/// and a link to one file comes back with that file named.
async fn list_hub_files(State(state): State<AppState>, Query(query): Query<HubQuery>) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let (repo, file) = adapters::hub_reference(&query.repo)
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "that is not a Hugging Face repository or file link".into()))?;
    let listing = state.adapters.hub_files(&sizes::client(), &repo).await.map_err(|error| api_error(StatusCode::BAD_GATEWAY, format!("{error:#}")))?;
    Ok(Json(serde_json::json!({ "listing": listing, "file": file })))
}

#[derive(Debug, Deserialize)]
struct InstallHubRequest {
    repo: String,
    paths: Vec<String>,
}

async fn install_hub_adapters(State(state): State<AppState>, Json(input): Json<InstallHubRequest>) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    state
        .adapters
        .begin_hub_install(&sizes::client(), &input.repo, &input.paths)
        .await
        .map_err(|error| api_error(StatusCode::CONFLICT, format!("{error:#}")))?;
    Ok(Json(serde_json::json!({ "started": true })))
}

async fn cancel_adapter_download(State(state): State<AppState>) -> Json<Value> {
    state.adapters.downloader().cancel();
    Json(serde_json::json!({ "cancelled": true }))
}

/// Stores uploaded adapter files: one or more `.safetensors`, and the
/// `adapter_config.json` or `lora.json` that came with them.
async fn import_adapter(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<adapters::AdapterMeta>), (StatusCode, Json<ApiError>)> {
    let mut name = String::new();
    let mut files = Vec::new();
    while let Some(field) = multipart.next_field().await.map_err(|e| api_error(StatusCode::BAD_REQUEST, format!("read adapter form: {e}")))? {
        match (field.name().unwrap_or_default().to_owned(), field.file_name().map(str::to_owned)) {
            (key, Some(file)) if key == "files" => {
                let bytes = field.bytes().await.map_err(|e| api_error(StatusCode::BAD_REQUEST, format!("read {file}: {e}")))?;
                files.push((file, bytes.to_vec()));
            }
            (key, _) if key == "name" => {
                name = field.text().await.map_err(|e| api_error(StatusCode::BAD_REQUEST, format!("read adapter name: {e}")))?;
            }
            _ => {}
        }
    }
    if name.trim().is_empty() {
        name = files
            .iter()
            .find(|(file, _)| file.ends_with(".safetensors"))
            .and_then(|(file, _)| std::path::Path::new(file).file_stem().and_then(|stem| stem.to_str()).map(str::to_owned))
            .unwrap_or_else(|| "Adapter".into());
    }
    let meta = state
        .adapters
        .import(&name, files, adapters::Origin::Imported)
        .map_err(|e| api_error(StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok((StatusCode::CREATED, Json(meta)))
}

async fn update_adapter(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(patch): Json<adapters::Patch>,
) -> Result<Json<adapters::AdapterMeta>, (StatusCode, Json<ApiError>)> {
    state.adapters.update(&id, patch).map(Json).map_err(|e| api_error(StatusCode::NOT_FOUND, e.to_string()))
}

async fn delete_adapter(State(state): State<AppState>, Path(id): Path<String>) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    state.adapters.remove(&id).map_err(|e| api_error(StatusCode::NOT_FOUND, e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

/// Starts processing a track into a preview. One run at a time: the stages are
/// quick, and a second request would only race the first for the preview.
async fn start_processing(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<processing::ProcessRequest>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    if request.stages().is_empty() {
        return Err(api_error(StatusCode::BAD_REQUEST, "choose at least one kind of processing".into()));
    }
    let song = state
        .library
        .get_song(&id)
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Song not found".into()))?;
    let source = state
        .library
        .media_path_for_song(&song)
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "this track has no stored audio".into()))?;
    let media = state.library.media_dir().to_path_buf();
    let reference = match &request.master {
        None => None,
        Some(processing::MasterSource::Song { song_id }) => {
            let reference_song = state
                .library
                .get_song(song_id)
                .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
                .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "the reference track is not in the library".into()))?;
            Some(
                state
                    .library
                    .media_path_for_song(&reference_song)
                    .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "the reference track has no stored audio".into()))?,
            )
        }
        Some(processing::MasterSource::Upload { upload_id }) => Some(
            processing::workspace_file(&media, upload_id)
                .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "the uploaded reference is gone; upload it again".into()))?,
        ),
    };

    let run_id = uuid::Uuid::now_v7().simple().to_string();
    {
        // checked and claimed under one lock, so two requests cannot both start
        let mut current = state.processing_run.write().await;
        if current.as_ref().is_some_and(|run| !run.done) {
            return Err(api_error(StatusCode::CONFLICT, "a track is already being processed".into()));
        }
        // a new run replaces the last preview nobody kept; its reference stays
        // when this run masters to the same upload
        if let Some(previous) = current.take() {
            let keep = reference.clone();
            for file in previous.leftovers(&media).into_iter().filter(|file| Some(file) != keep.as_ref()) {
                let _ = std::fs::remove_file(file);
            }
        }
        *current = Some(processing::ProcessRun {
            id: run_id.clone(),
            song_id: id.clone(),
            stages: request.stages(),
            stage: None,
            done: false,
            error: None,
            preview: None,
            preview_ready: false,
            request: request.clone(),
        });
    }

    let background = state.clone();
    tokio::task::spawn_blocking(move || {
        let handle = tokio::runtime::Handle::current();
        let current = |run: &Option<processing::ProcessRun>| run.as_ref().is_some_and(|run| run.id == run_id);
        let outcome = (|| -> anyhow::Result<std::path::PathBuf> {
            let audio = processing::run(&source, reference.as_deref(), &request, |stage| {
                let state = background.clone();
                let run_id = run_id.clone();
                handle.spawn(async move {
                    if let Some(run) = state.processing_run.write().await.as_mut().filter(|run| run.id == run_id) {
                        run.stage = Some(stage);
                    }
                });
            })?;
            let folder = processing::workspace(&media);
            std::fs::create_dir_all(&folder)?;
            let path = folder.join(format!("{id}-{}.wav", &run_id[run_id.len() - 8..]));
            audio_pcm::write_wav24(&path, &audio)?;
            Ok(path)
        })();
        handle.block_on(async {
            let mut guard = background.processing_run.write().await;
            if !current(&guard) {
                // discarded while it worked: nothing will ever ask for the preview
                if let Ok(path) = &outcome {
                    let _ = std::fs::remove_file(path);
                }
                return;
            }
            let run = guard.as_mut().expect("checked above");
            run.done = true;
            match outcome {
                Ok(path) => {
                    run.preview = path.file_name().and_then(|name| name.to_str()).map(str::to_owned);
                    run.preview_ready = run.preview.is_some();
                }
                Err(error) => run.error = Some(format!("{error:#}")),
            }
        });
    });
    Ok(Json(serde_json::json!({ "started": true })))
}

async fn read_processing(State(state): State<AppState>) -> Json<Value> {
    Json(serde_json::json!({ "run": state.processing_run.read().await.clone() }))
}

async fn processing_preview(State(state): State<AppState>, headers: HeaderMap) -> Result<axum::response::Response, (StatusCode, Json<ApiError>)> {
    let name = state.processing_run.read().await.as_ref().and_then(|run| run.preview.clone());
    let path = name
        .and_then(|name| processing::workspace_file(state.library.media_dir(), &name))
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "there is no processed preview".into()))?;
    serve_audio_file(&path, &headers).await
}

#[derive(Debug, Deserialize)]
struct KeepProcessingRequest {
    /// What the version is called in the track's version list.
    label: String,
}

/// Keeps the preview as a version of its track, playing from now on.
async fn keep_processing(
    State(state): State<AppState>,
    Json(input): Json<KeepProcessingRequest>,
) -> Result<Json<library::Song>, (StatusCode, Json<ApiError>)> {
    let run = state.processing_run.read().await.clone().filter(|run| run.preview_ready);
    let run = run.ok_or_else(|| api_error(StatusCode::NOT_FOUND, "there is no processed preview to keep".into()))?;
    let media = state.library.media_dir().to_path_buf();
    let preview = run
        .preview
        .as_deref()
        .and_then(|name| processing::workspace_file(&media, name))
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "the preview file is gone".into()))?;
    let filename = format!("{}-v{}-{}.wav", run.song_id, &uuid::Uuid::now_v7().simple().to_string()[..8], run.stages.join("-"));
    let stored = media.join(&filename);
    std::fs::rename(&preview, &stored).map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("store the version: {error}")))?;
    let reference_title = match &run.request.master {
        Some(processing::MasterSource::Song { song_id }) => state.library.get_song(song_id).ok().flatten().map(|song| song.title),
        _ => None,
    };
    let settings = processing::settings_record(&run.request, reference_title.as_deref());
    let recorded = match state.library.add_song_version(&run.song_id, &filename, input.label.trim(), settings) {
        Ok(Some(song)) => Ok(song),
        Ok(None) => Err(api_error(StatusCode::NOT_FOUND, "Song not found".into())),
        Err(error) => Err(api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())),
    };
    let song = match recorded {
        Ok(song) => song,
        Err(problem) => {
            // the preview goes back where it was, so the run can still be kept or discarded
            if let Err(error) = std::fs::rename(&stored, &preview) {
                eprintln!("[ERROR] processing: return {} to the workspace: {error}", stored.display());
            }
            return Err(problem);
        }
    };
    let mut current = state.processing_run.write().await;
    if current.as_ref().is_some_and(|now| now.id == run.id) {
        for file in current.take().map(|run| run.leftovers(&media)).unwrap_or_default() {
            let _ = std::fs::remove_file(file);
        }
    }
    Ok(Json(song))
}

/// Forgets the run, finished or not. A worker still going sees it is no longer
/// current and removes its own output.
async fn discard_processing(State(state): State<AppState>) -> Json<Value> {
    if let Some(run) = state.processing_run.write().await.take() {
        for file in run.leftovers(state.library.media_dir()) {
            let _ = std::fs::remove_file(file);
        }
    }
    Json(serde_json::json!({ "discarded": true }))
}

/// Stores a reference recording for mastering. It waits in the processing
/// folder, not the library: a reference is a tool, not a song.
async fn upload_processing_reference(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    while let Some(field) = multipart.next_field().await.map_err(|e| api_error(StatusCode::BAD_REQUEST, format!("read the upload: {e}")))? {
        if field.name() != Some("audio") {
            continue;
        }
        let original = field.file_name().unwrap_or("reference").to_owned();
        let extension = std::path::Path::new(&original)
            .extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase)
            .filter(|value| matches!(value.as_str(), "mp3" | "wav" | "flac" | "ogg" | "m4a"))
            .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "the reference must be MP3, WAV, FLAC, OGG or M4A".into()))?;
        let bytes = field.bytes().await.map_err(|e| api_error(StatusCode::BAD_REQUEST, format!("read the upload: {e}")))?;
        let folder = processing::workspace(state.library.media_dir());
        std::fs::create_dir_all(&folder).map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let upload_id = format!("reference-{}.{extension}", uuid::Uuid::now_v7().simple());
        std::fs::write(folder.join(&upload_id), &bytes).map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        return Ok(Json(serde_json::json!({ "upload_id": upload_id, "name": original })));
    }
    Err(api_error(StatusCode::BAD_REQUEST, "no audio part in the upload".into()))
}

#[derive(Debug, Deserialize)]
struct SelectVersionRequest {
    /// `original`, or the id of one of the track's versions.
    version: String,
}

async fn select_song_version(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<SelectVersionRequest>,
) -> Result<Json<library::Song>, (StatusCode, Json<ApiError>)> {
    let song = state
        .library
        .select_song_version(&id, &input.version)
        .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Song not found".into()))?;
    tag_stored_song(&state, &id).await;
    Ok(Json(song))
}

async fn remove_song_version(
    State(state): State<AppState>,
    Path((id, version)): Path<(String, String)>,
) -> Result<Json<library::Song>, (StatusCode, Json<ApiError>)> {
    let (song, file) = state
        .library
        .remove_song_version(&id, &version)
        .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Song not found".into()))?;
    if let Some(file) = file {
        if let Err(error) = std::fs::remove_file(&file) {
            eprintln!("[ERROR] remove version {version} of {id}: {error}");
        }
    }
    Ok(Json(song))
}

fn training_error(error: anyhow::Error) -> (StatusCode, Json<ApiError>) {
    api_error(StatusCode::BAD_REQUEST, format!("{error:#}"))
}

/// The training page: what is installed, the datasets, the runs, and what the
/// run in progress is doing.
async fn read_training(State(state): State<AppState>) -> Json<Value> {
    let training = &state.training;
    let active = training.active_run().await;
    let runs: Vec<Value> = training
        .runs()
        .into_iter()
        .map(|run| {
            let checkpoints = training.checkpoints(&run.id);
            let mut value = serde_json::to_value(&run).unwrap_or(Value::Null);
            value["checkpoints"] = serde_json::json!(checkpoints.iter().map(|checkpoint| checkpoint.step).collect::<Vec<_>>());
            if active.as_deref() == Some(run.id.as_str()) || run.status == training::RunStatus::Failed {
                value["log"] = serde_json::json!(training.log_tail(&run.id, 12));
            }
            value
        })
        .collect();
    Json(serde_json::json!({
        "pack": training.pack_status(),
        "pack_ready": training.pack_ready(),
        "download": training.downloader().active_for(training::SCOPE).await,
        "datasets": training.datasets(),
        "runs": runs,
        "active": active,
    }))
}

async fn install_training_pack(State(state): State<AppState>) -> Json<Value> {
    let training = state.training.clone();
    tokio::spawn(async move {
        if let Err(error) = training.install_pack().await {
            eprintln!("[ERROR] training pack: {error:#}");
        }
    });
    Json(serde_json::json!({ "started": true }))
}

async fn cancel_training_pack(State(state): State<AppState>) -> Json<Value> {
    state.training.downloader().cancel();
    Json(serde_json::json!({ "cancelled": true }))
}

#[derive(Debug, Deserialize)]
struct DatasetInput {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    trigger: Option<String>,
}

async fn create_training_dataset(State(state): State<AppState>, Json(input): Json<DatasetInput>) -> Result<Json<training::Dataset>, (StatusCode, Json<ApiError>)> {
    state.training.create_dataset(input.name.as_deref().unwrap_or_default(), input.trigger.as_deref().unwrap_or_default()).map(Json).map_err(training_error)
}

async fn update_training_dataset(State(state): State<AppState>, Path(id): Path<String>, Json(input): Json<DatasetInput>) -> Result<Json<training::Dataset>, (StatusCode, Json<ApiError>)> {
    state.training.update_dataset(&id, input.name, input.trigger).map(Json).map_err(training_error)
}

async fn delete_training_dataset(State(state): State<AppState>, Path(id): Path<String>) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    state.training.remove_dataset(&id).map_err(training_error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
struct DatasetSongs {
    song_ids: Vec<String>,
}

/// Adds library songs with their style and lyrics; decoding and resampling is
/// real work, so it runs off the request threads.
async fn add_training_songs(State(state): State<AppState>, Path(id): Path<String>, Json(input): Json<DatasetSongs>) -> Result<Json<training::Dataset>, (StatusCode, Json<ApiError>)> {
    let mut sources = Vec::new();
    for song_id in &input.song_ids {
        let song = state
            .library
            .get_song(song_id)
            .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
            .ok_or_else(|| api_error(StatusCode::NOT_FOUND, format!("song {song_id} is not in the library")))?;
        let audio = state.library.media_path_for_song(&song).ok_or_else(|| api_error(StatusCode::BAD_REQUEST, format!("{} has no stored audio", song.title)))?;
        sources.push((audio, song.title, song.caption, song.lyrics, song.id));
    }
    let training = state.training.clone();
    tokio::task::spawn_blocking(move || {
        let mut dataset = training.dataset(&id)?;
        for (audio, title, style, lyrics, song_id) in sources {
            dataset = training.add_item(&id, &audio, &title, &style, &lyrics, &format!("song:{song_id}"))?;
        }
        Ok::<_, anyhow::Error>(dataset)
    })
    .await
    .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
    .map(Json)
    .map_err(training_error)
}

/// Adds audio files from the user's disk; a same-named `.txt` or `.lrc` part
/// is taken as that song's lyrics.
async fn upload_training_files(State(state): State<AppState>, Path(id): Path<String>, mut multipart: Multipart) -> Result<Json<training::Dataset>, (StatusCode, Json<ApiError>)> {
    let folder = std::env::temp_dir().join(format!("training-upload-{}", uuid::Uuid::now_v7().simple()));
    std::fs::create_dir_all(&folder).map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    let mut audio = Vec::new();
    let mut texts = std::collections::HashMap::new();
    while let Some(field) = multipart.next_field().await.map_err(|e| api_error(StatusCode::BAD_REQUEST, format!("read the upload: {e}")))? {
        let Some(name) = field.file_name().map(|name| std::path::Path::new(name).file_name().and_then(|n| n.to_str()).unwrap_or("song").to_owned()) else { continue };
        let bytes = field.bytes().await.map_err(|e| api_error(StatusCode::BAD_REQUEST, format!("read {name}: {e}")))?;
        let lower = name.to_ascii_lowercase();
        let stem = std::path::Path::new(&name).file_stem().and_then(|stem| stem.to_str()).unwrap_or(&name).to_owned();
        if lower.ends_with(".txt") || lower.ends_with(".lrc") {
            texts.insert(stem, String::from_utf8_lossy(&bytes).into_owned());
        } else if [".wav", ".mp3", ".flac", ".ogg", ".m4a"].iter().any(|extension| lower.ends_with(extension)) {
            let path = folder.join(&name);
            std::fs::write(&path, &bytes).map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
            audio.push((path, stem, name));
        }
    }
    if audio.is_empty() {
        let _ = std::fs::remove_dir_all(&folder);
        return Err(api_error(StatusCode::BAD_REQUEST, "no audio in the upload: WAV, MP3, FLAC, OGG or M4A".into()));
    }
    let training = state.training.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        let mut dataset = training.dataset(&id)?;
        for (path, stem, name) in audio {
            let lyrics = texts.get(&stem).map(|text| training::plain_lyrics(text)).unwrap_or_default();
            dataset = training.add_item(&id, &path, &stem, "", &lyrics, &format!("file:{name}"))?;
        }
        Ok::<_, anyhow::Error>(dataset)
    })
    .await;
    let _ = std::fs::remove_dir_all(&folder);
    outcome.map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?.map(Json).map_err(training_error)
}

async fn update_training_item(
    State(state): State<AppState>,
    Path((id, item)): Path<(String, String)>,
    Json(patch): Json<training::ItemPatch>,
) -> Result<Json<training::Dataset>, (StatusCode, Json<ApiError>)> {
    state.training.update_item(&id, &item, patch).map(Json).map_err(training_error)
}

async fn delete_training_item(State(state): State<AppState>, Path((id, item)): Path<(String, String)>) -> Result<Json<training::Dataset>, (StatusCode, Json<ApiError>)> {
    state.training.remove_item(&id, &item).map(Json).map_err(training_error)
}

#[derive(Debug, Deserialize)]
struct StartTraining {
    dataset_id: String,
    #[serde(default)]
    name: String,
    recipe: training::Recipe,
}

/// Starts a run. It wants the whole card: refused while a song renders, and
/// the writing assistant is let go first.
async fn start_training(State(state): State<AppState>, Json(input): Json<StartTraining>) -> Result<Json<training::Run>, (StatusCode, Json<ApiError>)> {
    let rendering = state.jobs.read().await.values().any(|job| matches!(job.status, MusicJobStatus::Queued | MusicJobStatus::Running));
    if rendering {
        return Err(api_error(StatusCode::CONFLICT, "a song is being made; train once it is done".into()));
    }
    let tokenizer = selected_engine_models(&state).await.map_err(|error| api_error(StatusCode::CONFLICT, error))?.backbone;
    free_the_card_for_the_engine(&state).await;
    state
        .training
        .start(Some(engine_bundle_root()), tokenizer, &input.dataset_id, &input.name, input.recipe)
        .await
        .map(Json)
        .map_err(|error| api_error(StatusCode::CONFLICT, format!("{error:#}")))
}

async fn cancel_training(State(state): State<AppState>, Path(id): Path<String>) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    state.training.cancel(&id).await.map_err(training_error)?;
    Ok(Json(serde_json::json!({ "cancelled": true })))
}

async fn delete_training_run(State(state): State<AppState>, Path(id): Path<String>) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    state.training.remove_run(&id).await.map_err(training_error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
struct InstallCheckpoint {
    #[serde(default)]
    name: Option<String>,
}

/// Adds a checkpoint to the adapter library, where the create page finds it.
async fn install_training_checkpoint(
    State(state): State<AppState>,
    Path((id, step)): Path<(String, u32)>,
    Json(input): Json<InstallCheckpoint>,
) -> Result<Json<adapters::AdapterMeta>, (StatusCode, Json<ApiError>)> {
    let run = state.training.run(&id).map_err(training_error)?;
    let checkpoint = state
        .training
        .checkpoints(&id)
        .into_iter()
        .find(|checkpoint| checkpoint.step == step)
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, format!("the run has no checkpoint at step {step}")))?;
    let name = input.name.filter(|name| !name.trim().is_empty()).unwrap_or_else(|| format!("{} · {step}", run.name));
    let trigger = Some(run.trigger.clone());
    let meta = state
        .adapters
        .import_trained(&name, trigger, &checkpoint.files, adapters::Origin::Trained { run: id.clone(), step })
        .map_err(training_error)?;
    state.training.mark_installed(&id, step).map_err(training_error)?;
    Ok(Json(meta))
}

#[derive(Debug, Default, Deserialize)]
struct AutofillRequest {
    /// The language sung, when the user knows it; the recogniser guesses otherwise.
    #[serde(default)]
    language: Option<String>,
}

/// Writes a dataset song's lyrics from its recording: the vocals separated
/// when the separator is installed, recognised with the karaoke recogniser,
/// cut into lines at the pauses, and laid out in tagged sections by the
/// writing assistant. The user checks the result; nothing trains until then.
async fn autofill_training_item(
    State(state): State<AppState>,
    Path((id, item)): Path<(String, String)>,
    Json(input): Json<AutofillRequest>,
) -> Result<Json<training::Dataset>, (StatusCode, Json<ApiError>)> {
    if state.training.active_run().await.is_some() {
        return Err(api_error(StatusCode::CONFLICT, "a training run has the card; recognise lyrics once it finishes".into()));
    }
    let config = state.lyrics_sync_config.read().await.clone();
    if !config.available() || matches!(config.provider, lyrics_sync::AsrProvider::None) {
        return Err(api_error(StatusCode::CONFLICT, "no speech recogniser is set up: choose one under Settings - Karaoke".into()));
    }
    if !ensure_local_recogniser(&state, &config, &item).await {
        return Err(api_error(StatusCode::CONFLICT, "the speech recogniser is still downloading; try again when it is ready".into()));
    }
    let audio = state.training.item_audio(&id, &item).map_err(training_error)?;

    // The vocals alone recognise far better than the mix; without the
    // separator the whole song is used.
    let runtime = state.lyrics_sync.onnxruntime_library_of(if state.lyrics_sync.has_cuda_libraries() {
        lyrics_sync::OnnxFlavour::Cuda
    } else {
        lyrics_sync::OnnxFlavour::Cpu
    });
    let heard = match runtime.filter(|_| state.separator.is_installed()) {
        Some(runtime) => {
            let model = state.separator.model_path();
            let overlap = state.separation_config.read().await.sane_overlap();
            let on_gpu = state.lyrics_sync.has_cuda_libraries();
            let source = audio.clone();
            tokio::task::spawn_blocking(move || -> anyhow::Result<PathBuf> {
                point_ort_at(&runtime);
                let mix = audio_pcm::decode_stereo_44k(&source)?;
                let separated = separation::separate(&model, &mix, separation::STEMS.len(), overlap, on_gpu, |_| {})?;
                let vocals = separated.stems.into_iter().find(|stem| stem.name == "vocals").context("the separator returned no vocals")?;
                let path = std::env::temp_dir().join(format!("training-vocals-{}.wav", uuid::Uuid::now_v7().simple()));
                separation::write_wav_stereo(&path, &vocals.samples)?;
                Ok(path)
            })
            .await
            .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
            .map_err(|error| api_error(StatusCode::BAD_GATEWAY, format!("separating the vocals failed: {error:#}")))?
        }
        None => audio.clone(),
    };

    let words = match config.provider {
        lyrics_sync::AsrProvider::Parakeet => {
            let sync = state.lyrics_sync.clone();
            let path = heard.clone();
            tokio::task::spawn_blocking(move || sync.parakeet_words(&path)).await.map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
        }
        lyrics_sync::AsrProvider::Whisper => {
            let sync = state.lyrics_sync.clone();
            let config = config.clone();
            let path = heard.clone();
            let language = input.language.clone();
            tokio::task::spawn_blocking(move || sync.whisper_words(&config, &path, language.as_deref(), ""))
                .await
                .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
        }
        lyrics_sync::AsrProvider::OpenRouter => karaoke_words_from_openrouter(&state, &config, &heard.to_string_lossy(), input.language.as_deref()).await,
        lyrics_sync::AsrProvider::None => unreachable!("checked above"),
    };
    if heard != audio {
        let _ = std::fs::remove_file(&heard);
    }
    let words = words.map_err(|error| api_error(StatusCode::BAD_GATEWAY, format!("recognition failed: {error:#}")))?;
    // Punctuation the recogniser hangs at the start of a line belongs to the
    // line before, and an unknown-token marker is not a word.
    let mut lines: Vec<(f64, String)> = Vec::new();
    for (time, text) in lyrics_sync::group_words(&words) {
        let text = text.replace("<unk>", "");
        let body = text.trim_start_matches(|c: char| c.is_whitespace() || matches!(c, ',' | '.' | '!' | '?' | ';' | ':'));
        let lead = text.trim_start()[..text.trim_start().len() - body.len()].trim();
        if let (false, Some(last)) = (lead.is_empty(), lines.last_mut()) {
            last.1.push_str(lead);
        }
        if !body.trim().is_empty() {
            lines.push((time, body.trim().to_string()));
        }
    }
    if lines.is_empty() {
        return Err(api_error(StatusCode::BAD_GATEWAY, "no words were recognised in this song; mark it instrumental or write the lyrics".into()));
    }
    let transcript: String = lines
        .iter()
        .map(|(time, text)| format!("[{}:{:02}] {}", (*time as u64) / 60, (*time as u64) % 60, text.trim()))
        .collect::<Vec<_>>()
        .join("\n");

    let request = assistant::AssistRequest {
        target: assistant::AssistTarget::Transcript,
        description: transcript,
        instruction: String::new(),
        lyrics: String::new(),
        style: String::new(),
        abc: String::new(),
        duration_seconds: 0.0,
    };
    // A small model can answer a Cyrillic transcript in Latin letters; that is
    // not the song, so it is asked once more and then refused, never stored.
    let heard_cyrillic = assistant::cyrillic_share(&request.description) > 0.5;
    let mut lyrics = String::new();
    for _ in 0..2 {
        let Json(draft) = assistant_write(State(state.clone()), Json(request.clone())).await?;
        lyrics = draft.get("lyrics").and_then(Value::as_str).unwrap_or_default().to_string();
        if !heard_cyrillic || assistant::cyrillic_share(&lyrics) > 0.5 {
            break;
        }
        lyrics.clear();
    }
    if lyrics.is_empty() {
        return Err(api_error(StatusCode::BAD_GATEWAY, "the assistant rewrote the lyrics in Latin letters twice; choose a larger assistant model or write the lyrics".into()));
    }
    state
        .training
        .update_item(&id, &item, training::ItemPatch { lyrics: Some(lyrics), instrumental: Some(false), ..Default::default() })
        .map(Json)
        .map_err(training_error)
}

async fn read_stems(State(state): State<AppState>, Path(id): Path<String>) -> Json<Value> {
    Json(serde_json::json!({
        "song_id": id,
        "stems": stems_on_disk(&state, &id),
        "run": state.separation_run.read().await.clone(),
    }))
}

async fn read_stem_audio(
    State(state): State<AppState>,
    Path((id, stem)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<axum::response::Response, (StatusCode, Json<ApiError>)> {
    if !separation::STEMS.contains(&stem.as_str()) {
        return Err(api_error(StatusCode::BAD_REQUEST, format!("unknown stem {stem}")));
    }
    let path = stem_path(&state, &id, &stem);
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| api_error(StatusCode::NOT_FOUND, "this track has no such stem yet".into()))?;
    // Without range support a player cannot seek: it can only start at zero and
    // wait. The library's own audio has answered ranges from the beginning;
    // stems were served whole, which is why dragging their position did
    // nothing.
    let total = bytes.len();
    let range = headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| parse_single_byte_range(value, total));
    let response = if let Some((start, end)) = range {
        axum::response::Response::builder()
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_TYPE, "audio/wav")
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{total}"))
            .header(header::CONTENT_LENGTH, end - start + 1)
            .body(Body::from(bytes[start..=end].to_vec()))
    } else {
        axum::response::Response::builder()
            .header(header::CONTENT_TYPE, "audio/wav")
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CONTENT_LENGTH, total)
            .body(Body::from(bytes))
    };
    Ok(response.expect("valid stem response"))
}

/// Separates one track into stems, in the background, reporting progress.
async fn start_separation(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    if state.separation_run.read().await.as_ref().is_some_and(|run| !run.done) {
        return Err(api_error(StatusCode::CONFLICT, "a track is already being separated".into()));
    }
    let wanted_runtime = state.separation_config.read().await.runtime;
    // Which library is loaded is decided once per process - `ort` binds it on
    // first use - so always take the CUDA build when it is complete: it carries
    // the processor provider too, and the setting below decides which of them
    // actually runs. Choosing by the setting meant a studio that had run once
    // on the processor could never reach the card without a restart.
    let runtime = state
        .lyrics_sync
        .onnxruntime_library_of(if state.lyrics_sync.has_cuda_libraries() {
            lyrics_sync::OnnxFlavour::Cuda
        } else {
            lyrics_sync::OnnxFlavour::Cpu
        })
        .or_else(|| state.lyrics_sync.onnxruntime_library())
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "the ONNX Runtime is not installed yet".into()))?;
    // The card is only really available when every CUDA library the provider
    // links against is beside it.
    let on_gpu = !matches!(wanted_runtime, lyrics_sync::OnnxFlavour::Cpu)
        && state.lyrics_sync.has_cuda_libraries();
    if !state.separator.is_installed() {
        return Err(api_error(StatusCode::BAD_REQUEST, "the separation model is not installed yet".into()));
    }
    let song = state
        .library
        .get_song(&id)
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Song not found".into()))?;
    let audio_path = state
        .library
        .media_path_for_song(&song)
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "this track has no stored audio".into()))?;

    *state.separation_run.write().await =
        Some(SeparationRun { song_id: id.clone(), progress: 0.0, done: false, error: None, stems: vec![], used_gpu: None });

    let model = state.separator.model_path();
    let config = state.separation_config.read().await.clone();
    let overlap = config.sane_overlap();
    let wanted = config.stems.clone();
    let background = state.clone();
    let song_id = id.clone();
    tokio::task::spawn_blocking(move || {
        point_ort_at(&runtime);

        let outcome = (|| -> anyhow::Result<(Vec<String>, bool)> {
            let audio = audio_pcm::decode_stereo_44k(&audio_path)?;
            let handle = tokio::runtime::Handle::current();
            let separated = separation::separate(&model, &audio, separation::STEMS.len(), overlap, on_gpu, |fraction| {
                let state = background.clone();
                handle.spawn(async move {
                    if let Some(run) = state.separation_run.write().await.as_mut() {
                        run.progress = fraction;
                    }
                });
            })?;
            let mut written = Vec::new();
            let ran_on_gpu = separated.used_gpu;
            for stem in separated.stems {
                if !wanted.iter().any(|name| name == stem.name) {
                    continue;
                }
                let path = stem_path(&background, &song_id, stem.name);
                separation::write_wav_stereo(&path, &stem.samples)?;
                written.push(stem.name.to_string());
            }
            Ok((written, ran_on_gpu))
        })();

        let handle = tokio::runtime::Handle::current();
        handle.spawn(async move {
            if let Some(run) = background.separation_run.write().await.as_mut() {
                run.done = true;
                match outcome {
                    Ok((stems, ran_on_gpu)) => {
                        run.progress = 1.0;
                        run.stems = stems;
                        run.used_gpu = Some(ran_on_gpu);
                    }
                    Err(error) => run.error = Some(error.to_string()),
                }
            }
        });
    });

    Ok(Json(serde_json::json!({ "started": true, "song_id": id })))
}


/// Draws a cover for a finished track, if the studio was told to.
///
/// The same pieces the cover window uses: the default template, filled in from
/// this track, and the image model chosen on the provider page. Nothing happens
/// without a key, without a model, or when the user turned this off - and a
/// failure is written to the log rather than shown as a broken track.
/// Draws the cover for one track now, and says what went wrong if it did not.
async fn draw_cover_now(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    match draw_cover(&state, &id).await {
        Ok(()) => Ok(Json(serde_json::json!({ "drawn": true }))),
        Err(error) => Err(api_error(StatusCode::BAD_GATEWAY, error.to_string())),
    }
}


/// Times the lyrics of a finished track, if karaoke is switched on.
///
/// The switch said "on" and nothing happened: the timings were only ever made
/// by the button in the track menu. A track arrives with its words already
/// known, so this is the moment to time them.

/// One background piece of work on a finished track.
#[derive(Debug, Clone, Serialize)]
struct Activity {
    song_id: String,
    title: String,
    /// "cover" or "karaoke".
    kind: &'static str,
    /// "running", "done" or "failed".
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

/// Notes what is happening, keeping only the recent past.
async fn note_activity(state: &AppState, song_id: &str, title: &str, kind: &'static str, phase: &'static str, detail: Option<String>) {
    let mut activity = state.activity.write().await;
    if let Some(existing) = activity.iter_mut().find(|entry| entry.song_id == song_id && entry.kind == kind) {
        existing.state = phase;
        existing.detail = detail;
        existing.title = title.to_string();
    } else {
        activity.push(Activity {
            song_id: song_id.to_string(),
            title: title.to_string(),
            kind,
            state: phase,
            detail,
        });
    }
    let overflow = activity.len().saturating_sub(20);
    if overflow > 0 {
        activity.drain(..overflow);
    }
}

async fn read_activity(State(state): State<AppState>) -> Json<Value> {
    Json(serde_json::json!({ "activity": state.activity.read().await.clone() }))
}

/// Downloads whatever the chosen local recogniser is missing, then waits for it.
///
/// Choosing Parakeet or Whisper in the settings is the instruction to use it;
/// making the user then find a download button for it is a second instruction
/// nobody asked for. The first track that needs timings fetches the model and
/// carries on.
async fn ensure_local_recogniser(state: &AppState, config: &lyrics_sync::LyricsSyncConfig, song_id: &str) -> bool {
    let ready = |state: &AppState| match config.provider {
        lyrics_sync::AsrProvider::Parakeet => state.lyrics_sync.parakeet_ready(),
        lyrics_sync::AsrProvider::Whisper => {
            state.lyrics_sync.whisper_binary().is_some() && state.lyrics_sync.whisper_model_ready(config)
        }
        _ => true,
    };
    if ready(state) {
        return true;
    }

    let missing: Vec<&'static lyrics_sync::Asset> = match config.provider {
        lyrics_sync::AsrProvider::Parakeet => lyrics_sync::PARAKEET_ASSET_IDS
            .iter()
            .filter_map(|id| lyrics_sync::asset(id))
            .filter(|asset| !state.lyrics_sync.downloader().is_installed(asset))
            .collect(),
        // Whatever the chosen recogniser is made of, by the same reckoning the
        // panel uses. Naming the files here by hand is how this went on asking
        // for `whisper-cuda` after that asset had ceased to exist, and then
        // concluded that nothing was missing and nothing was ready.
        lyrics_sync::AsrProvider::Whisper => karaoke_set("whisper", config.runtime, config.whisper_model.as_deref())
            .into_iter()
            .filter(|asset| !state.lyrics_sync.downloader().is_installed(asset))
            .collect(),
        _ => Vec::new(),
    };
    if missing.is_empty() {
        return ready(state);
    }

    let title = state.library.get_song(song_id).ok().flatten().map(|song| song.title).unwrap_or_default();
    note_activity(state, song_id, &title, "karaoke", "running", Some("karaoke.downloading".into())).await;
    for asset in missing {
        if let Err(error) = state.lyrics_sync.downloader().install(asset).await {
            eprintln!("could not fetch the karaoke model {}: {error}", asset.id);
            note_activity(state, song_id, &title, "karaoke", "failed", Some(error.to_string())).await;
            return false;
        }
        // The downloader runs one file at a time in the background; the timings
        // wait for it rather than starting against half a model. The wait is
        // reported with real numbers: a spinner that says "downloading" for ten
        // minutes without moving is indistinguishable from one that is stuck.
        loop {
            let Some(progress) = state.lyrics_sync.downloader().active().await else { break };
            if progress.done {
                break;
            }
            let percent = if progress.total_bytes > 0 {
                (progress.downloaded_bytes * 100 / progress.total_bytes).min(100)
            } else {
                0
            };
            note_activity(state, song_id, &title, "karaoke", "running", Some(format!("karaoke.downloading {percent}%"))).await;
            tokio::time::sleep(std::time::Duration::from_millis(700)).await;
        }
    }
    ready(state)
}

async fn time_lyrics_for(state: AppState, song_id: String) {
    let config = state.lyrics_sync_config.read().await.clone();
    if !config.enabled || config.provider == lyrics_sync::AsrProvider::None {
        return;
    }
    // The cloud recogniser is the one case that cannot be fixed from here: a key
    // is the user's to add, and announcing a failure they cannot act on is
    // noise. A local recogniser is different - if its model is not on disk yet,
    // choosing it is the instruction to fetch it, so the first use downloads it
    // and then does the work.
    if config.provider == lyrics_sync::AsrProvider::OpenRouter && credentials::openrouter_api_key().is_none() {
        return;
    }
    if !ensure_local_recogniser(&state, &config, &song_id).await {
        return;
    }
    let Ok(Some(song)) = state.library.get_song(&song_id) else { return };
    // An instrumental has section markers and no words. Timing it means asking
    // the recogniser to find lyrics that were never sung.
    if !auto_title::has_sung_lines(&song.lyrics) {
        return;
    }
    let Some(audio) = state.library.media_path_for_song(&song) else { return };
    let audio = audio.display().to_string();

    note_activity(&state, &song_id, &song.title, "karaoke", "running", None).await;
    let words = match config.provider {
        lyrics_sync::AsrProvider::None => return,
        lyrics_sync::AsrProvider::Parakeet => {
            let sync = state.lyrics_sync.clone();
            let path = std::path::PathBuf::from(&audio);
            match tokio::task::spawn_blocking(move || sync.parakeet_words(&path)).await {
                Ok(result) => result,
                Err(error) => {
                    eprintln!("no karaoke for {song_id}: {error}");
                    return;
                }
            }
        }
        lyrics_sync::AsrProvider::Whisper => {
            let sync = state.lyrics_sync.clone();
            let config = config.clone();
            let path = std::path::PathBuf::from(&audio);
            let lyrics = song.lyrics.clone();
            match tokio::task::spawn_blocking(move || sync.whisper_words(&config, &path, None, &lyrics)).await {
                Ok(result) => result,
                Err(error) => {
                    eprintln!("no karaoke for {song_id}: {error}");
                    return;
                }
            }
        }
        lyrics_sync::AsrProvider::OpenRouter => {
            karaoke_words_from_openrouter(&state, &config, &audio, None).await
        }
    };
    let words = match words {
        Ok(words) => words,
        Err(error) => {
            eprintln!("no karaoke for {song_id}: {error}");
            note_activity(&state, &song_id, &song.title, "karaoke", "failed", Some(error.to_string())).await;
            return;
        }
    };
    let lines = lyrics_sync::align_lyrics_words(&words, &song.lyrics);
    if lines.is_empty() {
        let reason = "karaoke.no-match";
        eprintln!("no karaoke for {song_id}: {reason}");
        note_activity(&state, &song_id, &song.title, "karaoke", "failed", Some(reason.to_string())).await;
        return;
    }
    match state.library.set_song_lrc(&song_id, &lyrics_sync::enhanced_lrc(&lines)) {
        Ok(_) => note_activity(&state, &song_id, &song.title, "karaoke", "done", None).await,
        Err(error) => {
            eprintln!("could not store karaoke for {song_id}: {error}");
            note_activity(&state, &song_id, &song.title, "karaoke", "failed", Some(error.to_string())).await;
        }
    }
}

async fn draw_cover_for(state: AppState, song_id: String) {
    if !*state.cover_auto.read().await {
        return;
    }
    // Drawing a cover needs a cloud key. Without one there is nothing to try,
    // and announcing a failure the user cannot act on is noise: the track keeps
    // the placeholder artwork the library already shows.
    if credentials::openrouter_api_key().is_none() {
        return;
    }
    let title = state.library.get_song(&song_id).ok().flatten().map(|song| song.title).unwrap_or_default();
    note_activity(&state, &song_id, &title, "cover", "running", None).await;
    match draw_cover(&state, &song_id).await {
        Ok(()) => note_activity(&state, &song_id, &title, "cover", "done", None).await,
        Err(error) => {
            eprintln!("no cover for {song_id}: {error}");
            note_activity(&state, &song_id, &title, "cover", "failed", Some(error.to_string())).await;
        }
    }
}

/// The work itself, with its reasons kept rather than printed.
async fn draw_cover(state: &AppState, song_id: &str) -> anyhow::Result<()> {
    use anyhow::Context as _;
    let song = state
        .library
        .get_song(song_id)?
        .context("the track is not in the library")?;
    if song.metadata.get("cover_filename").is_some() {
        return Ok(());
    }
    let model = {
        let configuration = state.configuration.read().await;
        configuration
            .selections
            .iter()
            .find(|selection| selection.capability == Capability::CoverArt)
            .filter(|selection| selection.mode == ExecutionMode::OpenRouter)
            .and_then(|selection| selection.cloud_model.clone())
            .filter(|model| !model.trim().is_empty())
    };
    let catalog = catalog_for(state).await.map_err(|error| anyhow::anyhow!(error))?;
    let model = match model {
        Some(model) => model,
        None => providers::openrouter::suggested_model(&catalog, Capability::CoverArt)
            .context("no image model is chosen for covers")?,
    };
    let templates = state.cover_templates.read().await.clone();
    let default_id = state.cover_template_default.read().await.clone();
    let template = templates
        .iter()
        .find(|entry| Some(&entry.id) == default_id.as_ref())
        .or_else(|| templates.first())
        .map(|entry| entry.template.clone())
        .context("there are no cover templates")?;
    let facts = cover_prompt::TrackFacts {
        title: song.title.clone(),
        style: song.caption.clone(),
        lyrics: song.lyrics.clone(),
        duration_seconds: song.metadata.get("duration_seconds").and_then(Value::as_f64).unwrap_or(0.0),
    };
    let prompt = match song.metadata.get("cover_prompt").and_then(Value::as_str).map(str::trim).filter(|value| !value.is_empty()) {
        Some(written) => written.to_string(),
        None => cover_prompt::render(&template, &facts),
    };
    let request = providers::openrouter::request_for(&catalog, Capability::CoverArt, &model, &prompt)?;
    let answered = execute_openrouter_json(request).await.map_err(|error| anyhow::anyhow!(error))?;
    let first = answered
        .body
        .get("data")
        .and_then(|data| data.get(0))
        .context("the model returned no image")?;
    let image = first
        .get("b64_json")
        .and_then(Value::as_str)
        .context("the model returned no image")?;
    // The answer states its own format, and it is not always PNG.
    let media_type = first.get("media_type").and_then(Value::as_str).unwrap_or("image/png").to_string();
    use base64::{engine::general_purpose::STANDARD, Engine};
    let bytes = STANDARD.decode(image.trim()).context("the image was not valid base64")?;
    state.library.store_song_cover(song_id, &bytes, &media_type)?;
    tag_stored_song(state, song_id).await;
    Ok(())
}

async fn read_cover_templates(State(state): State<AppState>) -> Json<Value> {
    Json(serde_json::json!({
        "auto": *state.cover_auto.read().await,
        "templates": state.cover_templates.read().await.clone(),
        "default_id": state.cover_template_default.read().await.clone(),
        "placeholders": ["title", "style", "lyrics", "excerpt", "duration"],
    }))
}

async fn write_cover_templates(
    State(state): State<AppState>,
    Json(request): Json<CoverTemplatesRequest>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let templates = if request.templates.is_empty() { cover_prompt::default_templates() } else { request.templates };
    *state.cover_templates.write().await = templates.clone();
    // A default that names a template nobody kept is worse than none.
    let default_id = request
        .default_id
        .filter(|id| !id.trim().is_empty() && templates.iter().any(|entry| entry.id == *id));
    *state.cover_template_default.write().await = default_id.clone();
    if let Some(auto) = request.auto {
        *state.cover_auto.write().await = auto;
    }
    persist_studio_settings(&state)
        .await
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(Json(serde_json::json!({
        "templates": templates,
        "default_id": default_id,
        "auto": *state.cover_auto.read().await,
    })))
}

/// The prompt a template turns into for one track, exactly as it would be sent.
async fn render_cover_template(
    State(state): State<AppState>,
    Json(request): Json<RenderCoverPromptRequest>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let mut facts = cover_prompt::TrackFacts {
        title: request.title.unwrap_or_default(),
        style: request.style.unwrap_or_default(),
        lyrics: request.lyrics.unwrap_or_default(),
        duration_seconds: 0.0,
    };
    if let Some(song_id) = request.song_id.as_deref().filter(|value| !value.trim().is_empty()) {
        let song = state
            .library
            .get_song(song_id)
            .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
            .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Song not found".into()))?;
        facts.title = song.title.clone();
        facts.style = song.caption.clone();
        facts.lyrics = song.lyrics.clone();
        facts.duration_seconds = song
            .metadata
            .get("duration_seconds")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
    }
    Ok(Json(serde_json::json!({ "prompt": cover_prompt::render(&request.template, &facts) })))
}

async fn library_cover(State(state): State<AppState>, Path(id): Path<String>) -> Result<axum::response::Response, (StatusCode, Json<ApiError>)> {
    let song = state.library.get_song(&id).map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Song not found".into()))?;
    let (path, media_type) = state.library.cover_path_for_song(&song).ok_or_else(|| api_error(StatusCode::NOT_FOUND, "This song has no stored cover image".into()))?;
    let bytes = tokio::fs::read(&path).await.map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("read cover: {error}")))?;
    Ok(axum::response::Response::builder()
        .header(header::CONTENT_TYPE, media_type)
        .header(header::CONTENT_LENGTH, bytes.len())
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from(bytes))
        .expect("valid cover response"))
}


/// Writes ID3 tags onto a stored MP3 from what the library knows about it.
///
/// Called after a track is stored, after its cover changes and after it is
/// renamed. Failure is logged and never fails the request: an untagged track
/// still plays, a lost one does not.
async fn tag_stored_song(state: &AppState, song_id: &str) {
    let Ok(Some(song)) = state.library.get_song(song_id) else { return };
    // `audio_path` is a full path, not a filename: resolve it the way playback
    // does, or tagging silently skips every track.
    let Some(audio_path) = state.library.media_path_for_song(&song) else { return };
    if audio_path.extension().and_then(|value| value.to_str()).map(str::to_lowercase).as_deref() != Some("mp3") {
        return;
    }
    let cover = state
        .library
        .cover_path_for_song(&song)
        .and_then(|(path, media_type)| std::fs::read(path).ok().map(|bytes| (media_type.to_string(), bytes)));
    let tags = tagging::TrackTags {
        title: song.title.clone(),
        album: "YuE2 Studio".to_string(),
        // The engine is the performer here; the studio is the label.
        artist: "YuE2".to_string(),
        genre: tagging::genre_from_caption(&song.caption),
        lyrics: Some(song.lyrics.clone()).filter(|value| !value.trim().is_empty()),
        bpm: tagging::bpm_from_caption(&song.caption),
        cover,
    };
    if let Err(error) = tagging::write_mp3_tags(&audio_path, &tags) {
        eprintln!("could not tag {}: {error}", audio_path.display());
    }
}

async fn store_library_cover(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<StoreCoverRequest>,
) -> Result<Json<library::Song>, (StatusCode, Json<ApiError>)> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let image = STANDARD
        .decode(request.image_base64.trim())
        .map_err(|error| api_error(StatusCode::BAD_REQUEST, format!("cover image is not valid base64: {error}")))?;
    let song = state
        .library
        .store_song_cover(&id, &image, &request.media_type)
        .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?;
    // The cover belongs in the file too, not only beside it.
    tag_stored_song(&state, &id).await;
    Ok(Json(song))
}

async fn create_library_song(State(state):State<AppState>,Json(input):Json<library::SongInput>)->Result<(StatusCode,Json<library::Song>),(StatusCode,Json<ApiError>)>{state.library.create_song(input).map(|s|(StatusCode::CREATED,Json(s))).map_err(|e|api_error(StatusCode::BAD_REQUEST,e.to_string()))}
async fn import_library_audio(State(state): State<AppState>, mut multipart: Multipart) -> Result<(StatusCode, Json<library::Song>), (StatusCode, Json<ApiError>)> {
    let mut title = None; let mut caption = String::new(); let mut lyrics = String::new(); let mut audio = None; let mut filename = None;
    while let Some(field) = multipart.next_field().await.map_err(|e| api_error(StatusCode::BAD_REQUEST, format!("read import form: {e}")))? {
        let name = field.name().unwrap_or_default().to_owned();
        if name == "audio" { filename = field.file_name().map(str::to_owned); audio = Some(field.bytes().await.map_err(|e| api_error(StatusCode::BAD_REQUEST, format!("read audio upload: {e}")))?.to_vec()); }
        else { let value = field.text().await.map_err(|e| api_error(StatusCode::BAD_REQUEST, format!("read import field: {e}")))?; match name.as_str() { "title" => title = Some(value), "caption" => caption = value, "lyrics" => lyrics = value, _ => {} } }
    }
    let filename = filename.ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "audio file is required".into()))?;
    let extension = std::path::Path::new(&filename).extension().and_then(|value| value.to_str()).unwrap_or_default().to_owned();
    let title = title.filter(|value| !value.trim().is_empty()).unwrap_or_else(|| std::path::Path::new(&filename).file_stem().and_then(|value| value.to_str()).unwrap_or("Imported audio").to_owned());
    let audio = audio.ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "audio file is required".into()))?;
    let duration = library::audio_duration_seconds(&audio, &extension.to_ascii_lowercase(), None);
    let song = state.library.import_audio_song(library::AudioImportInput { title, caption, lyrics, metadata: serde_json::json!({"imported_filename": filename, "duration_seconds": duration}), generation_settings: Value::Null, engine_id: "imported-audio".into(), profile_id: None, source: "audio_import".into(), audio_extension: extension, audio }).map_err(|e| api_error(StatusCode::BAD_REQUEST, e.to_string()))?.song;
    Ok((StatusCode::CREATED, Json(song)))
}
async fn update_library_song(State(state):State<AppState>,Path(id):Path<String>,Json(input):Json<library::SongInput>)->Result<Json<library::Song>,(StatusCode,Json<ApiError>)>{
    let song = state.library.update_song(&id,input).map_err(|e|api_error(StatusCode::BAD_REQUEST,e.to_string()))?.ok_or_else(||api_error(StatusCode::NOT_FOUND,"Song not found".into()))?;
    // A rename is a title change, and the file carries the title.
    tag_stored_song(&state, &id).await;
    Ok(Json(song))
}
async fn delete_library_song(State(state):State<AppState>,Path(id):Path<String>)->Result<StatusCode,(StatusCode,Json<ApiError>)>{
    let song = state.library.get_song(&id).map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if !state.library.delete_song(&id).map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))? {
        return Err(api_error(StatusCode::NOT_FOUND, "Song not found".into()));
    }
    if let Some(song) = song {
        for path in song_files(&state, &song) {
            if let Err(error) = std::fs::remove_file(&path) {
                eprintln!("[ERROR] delete song {id}: could not remove {}: {error}", path.display());
            }
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

/// The files in the media folder that belong to one song: its audio, its
/// stems and its cover. Anything outside the media folder is not ours to remove.
fn song_files(state: &AppState, song: &library::Song) -> Vec<PathBuf> {
    let media = state.library.media_dir();
    let mut files: Vec<PathBuf> = separation::STEMS.iter().map(|stem| stem_path(state, &song.id, stem)).collect();
    if let Some(audio) = song.audio_path.as_deref().map(PathBuf::from) {
        files.push(audio);
    }
    // a processed track owns its original and every version besides the one playing
    if let Some(original) = song.metadata.get("original_audio_path").and_then(Value::as_str) {
        files.push(PathBuf::from(original));
    }
    for version in song.metadata.get("audio_versions").and_then(Value::as_array).into_iter().flatten() {
        if let Some(file) = version.get("file").and_then(Value::as_str) {
            files.push(media.join(file));
        }
    }
    files.sort();
    files.dedup();
    if let Some((cover, _)) = state.library.cover_path_for_song(song) {
        files.push(cover);
    }
    files.retain(|path| path.parent() == Some(media) && path.is_file());
    files
}
async fn library_playlists(State(state):State<AppState>)->Result<Json<Vec<library::Playlist>>,(StatusCode,Json<ApiError>)>{state.library.list_playlists().map(Json).map_err(|e|api_error(StatusCode::INTERNAL_SERVER_ERROR,e.to_string()))}
async fn create_library_playlist(State(state):State<AppState>,Json(input):Json<library::PlaylistInput>)->Result<(StatusCode,Json<library::Playlist>),(StatusCode,Json<ApiError>)>{state.library.create_playlist(input).map(|p|(StatusCode::CREATED,Json(p))).map_err(|e|api_error(StatusCode::BAD_REQUEST,e.to_string()))}
async fn library_playlist(State(state):State<AppState>,Path(id):Path<String>)->Result<Json<library::Playlist>,(StatusCode,Json<ApiError>)>{state.library.get_playlist(&id).map_err(|e|api_error(StatusCode::INTERNAL_SERVER_ERROR,e.to_string()))?.map(Json).ok_or_else(||api_error(StatusCode::NOT_FOUND,"Playlist not found".into()))}
async fn update_library_playlist(State(state):State<AppState>,Path(id):Path<String>,Json(input):Json<library::PlaylistInput>)->Result<Json<library::Playlist>,(StatusCode,Json<ApiError>)>{state.library.update_playlist(&id,input).map_err(|e|api_error(StatusCode::BAD_REQUEST,e.to_string()))?.map(Json).ok_or_else(||api_error(StatusCode::NOT_FOUND,"Playlist not found".into()))}
async fn delete_library_playlist(State(state):State<AppState>,Path(id):Path<String>)->Result<StatusCode,(StatusCode,Json<ApiError>)>{if state.library.delete_playlist(&id).map_err(|e|api_error(StatusCode::INTERNAL_SERVER_ERROR,e.to_string()))?{Ok(StatusCode::NO_CONTENT)}else{Err(api_error(StatusCode::NOT_FOUND,"Playlist not found".into()))}}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
}

async fn health(State(state): State<AppState>) -> Json<Value> {
    let engine_ready = state.music_server.health().await;
    // Which program is actually answering here, and whether it can start an
    // engine at all. A second copy of the studio - a development build, say -
    // takes this port and the window then waits forever on an engine that
    // copy has no bundle for. Saying whose service this is turns a hang into
    // a sentence.
    let executable = std::env::current_exe().ok().map(|path| path.display().to_string());
    let engine_available = engine_location(*state.engine_options.read().await).bundle_root.is_dir();
    Json(serde_json::json!({
        "status": "ok",
        "runtime": "native",
        "service_executable": executable,
        "engine_bundle_present": engine_available,
        "music_engine": {
            "id": PRIMARY_MUSIC_ENGINE_ID,
            "base_url": state.music_server.base_url,
            "reachable": engine_ready,
        }
    }))
}

async fn configuration(State(state): State<AppState>) -> Json<StudioConfiguration> {
    Json(state.configuration.read().await.clone())
}

async fn update_configuration(
    State(state): State<AppState>,
    Json(update): Json<StudioConfiguration>,
) -> Json<StudioConfiguration> {
    // A page that changes one capability sends one selection. Storing the
    // request verbatim then erased every other choice - which is how a studio
    // with a downloaded engine started answering "the local music engine is not
    // configured" after the assistant was pointed at a local model.
    let configuration = {
        let mut stored = state.configuration.write().await;
        for selection in update.selections {
            match stored.selections.iter_mut().find(|existing| existing.capability == selection.capability) {
                Some(existing) => *existing = selection,
                None => stored.selections.push(selection),
            }
        }
        stored.clone()
    };

    // The choice has to reach the code that does the work, or the button is
    // decoration. Speech-to-text is done by the karaoke stack, and the writing
    // assistant has its own provider; both follow this page now.
    for selection in &configuration.selections {
        match selection.capability {
            Capability::SpeechToText => {
                let mut sync = state.lyrics_sync_config.write().await;
                sync.provider = match selection.mode {
                    ExecutionMode::OpenRouter => lyrics_sync::AsrProvider::OpenRouter,
                    ExecutionMode::Local => match selection.local_engine.as_deref() {
                        Some("whisper") => lyrics_sync::AsrProvider::Whisper,
                        Some("parakeet") => lyrics_sync::AsrProvider::Parakeet,
                        _ if state.lyrics_sync.parakeet_ready() => lyrics_sync::AsrProvider::Parakeet,
                        _ if state.lyrics_sync.whisper_binary().is_some() => lyrics_sync::AsrProvider::Whisper,
                        _ => sync.provider,
                    },
                };
                if selection.mode == ExecutionMode::OpenRouter {
                    sync.openrouter_model = selection.cloud_model.clone();
                }
            }
            Capability::PromptEnhancement => {
                let mut assistant = state.assistant.write().await;
                match selection.mode {
                    ExecutionMode::OpenRouter => {
                        assistant.provider = AssistantProvider::OpenRouter;
                        if let Some(model) = selection.cloud_model.clone() {
                            assistant.openrouter_model = Some(model);
                        }
                    }
                    ExecutionMode::Local => {
                        // Whichever local shape is set up: a managed model the
                        // studio downloaded, or a server the user runs.
                        assistant.provider = if assistant.managed_model.is_some() || assistant.managed_path.is_some() {
                            AssistantProvider::Managed
                        } else {
                            AssistantProvider::Local
                        };
                    }
                }
            }
            _ => {}
        }
    }

    let _ = persist_studio_settings(&state).await;
    Json(configuration)
}

async fn engine_options(State(state): State<AppState>) -> Json<Value> {
    let options = *state.engine_options.read().await;
    Json(serde_json::json!({
        "options": options,
        "effective_max_batch": options.effective_max_batch(),
        "restart_required_to_apply": true,
    }))
}

/// Stores the launch flags and restarts the engine if it is running, because
/// upstream reads them once at startup.
async fn update_engine_options(
    State(state): State<AppState>,
    Json(request): Json<EngineOptions>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    if request.max_batch.is_some_and(|value| value == 0 || value > 8) {
        return Err(api_error(StatusCode::BAD_REQUEST, "max_batch must be between 1 and 8".into()));
    }
    if request.max_seq.is_some_and(|value| !(4096..=24576).contains(&value)) {
        return Err(api_error(StatusCode::BAD_REQUEST, "max_seq must be between 4096 and 24576".into()));
    }
    if request.vae_core.is_some_and(|value| !(64..=4096).contains(&value)) {
        return Err(api_error(StatusCode::BAD_REQUEST, "vae_core must be between 64 and 4096".into()));
    }
    if request.vae_halo.is_some_and(|value| value > 256) {
        return Err(api_error(StatusCode::BAD_REQUEST, "vae_halo must be at most 256".into()));
    }
    let changed = {
        let mut options = state.engine_options.write().await;
        let changed = *options != request;
        *options = request;
        changed
    };
    let _ = persist_studio_settings(&state).await;

    let mut restarted = false;
    if changed && state.music_server.health().await {
        restart_engine(&state).await.map_err(|error| api_error(StatusCode::SERVICE_UNAVAILABLE, error))?;
        restarted = true;
    }
    Ok(Json(serde_json::json!({
        "options": request,
        "effective_max_batch": request.effective_max_batch(),
        "engine_restarted": restarted,
    })))
}

async fn restart_local_engine(State(state): State<AppState>) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    restart_engine(&state).await.map_err(|error| api_error(StatusCode::SERVICE_UNAVAILABLE, error))?;
    Ok(Json(serde_json::json!({ "engine_id": PRIMARY_MUSIC_ENGINE_ID, "restarted": true })))
}

async fn restart_engine(state: &AppState) -> Result<(), String> {
    let mut supervisor = state.engine.lock().await;
    let owned = supervisor.is_some();
    if let Some(engine) = supervisor.as_mut() {
        tokio::task::block_in_place(|| engine.stop(std::time::Duration::from_secs(10)))
            .map_err(|error| format!("stopping the local engine failed: {error}"))?;
    }
    // Launch flags are read once at engine startup. If something this service
    // does not own is still listening, starting again would silently reuse it
    // and the new flags would never take effect — report that instead of
    // claiming a restart that did not happen.
    if !owned && state.music_server.health().await {
        return Err(
            "An engine that this application did not start is already running on the engine port.              Close it and try again, otherwise the new options cannot be applied."
                .into(),
        );
    }
    // Nothing can start until the libraries the engine binary imports are on
    // disk: Windows resolves them before the process runs, so a missing cuBLAS
    // is not a slow start, it is no start at all. This is the path the studio
    // actually takes on launch, so the fetch belongs here rather than only in
    // the endpoint nothing calls.
    let options = *state.engine_options.read().await;
    let cuda = options.uses_cuda();
    if !state.engine_runtime.is_ready(cuda) {
        state
            .engine_runtime
            .install_missing(cuda)
            .await
            .map_err(|error| format!("the engine's runtime libraries could not be installed: {error}"))?;
    }
    // The engine loads eleven gigabytes of weights the moment it starts. If
    // the writing assistant is still holding the card, it does not finish.
    free_the_card_for_the_engine(state).await;
    let models = selected_engine_models(state).await?;
    let config = engine_location(options)
        .resolve(models)
        .map_err(|error| format!("the local engine runtime was not found: {error}"))?;
    let mut engine = music_engine::yue_server::YueServerSupervisor::new(config).map_err(|error| error.to_string())?;
    tokio::task::block_in_place(|| engine.ensure_started(std::time::Duration::from_secs(60)))
        .map_err(|error| format!("the local engine did not start: {error}"))?;
    *supervisor = Some(engine);
    Ok(())
}

/// Where the packaged or developer-built `yue-server` lives. Every value is
/// an explicit override or a documented default; nothing is downloaded here.
fn engine_bundle_root() -> PathBuf {
    env::var_os("YUE_ENGINE_ROOT")
        .map(PathBuf::from)
        .or_else(|| env::var_os("YUE_ENGINE_BIN").map(PathBuf::from).and_then(|path| path.parent().map(std::path::Path::to_path_buf)))
        .or_else(|| std::env::current_exe().ok().and_then(|path| path.parent().map(|parent| parent.join("resources").join("yue2-cpp"))))
        .unwrap_or_else(|| PathBuf::from("resources/yue2-cpp"))
}

fn engine_location(options: EngineOptions) -> music_engine::yue_server::YueServerLocation {
    music_engine::yue_server::YueServerLocation {
        bundle_root: engine_bundle_root(),
        configured_executable: env::var_os("YUE_ENGINE_BIN").map(PathBuf::from),
        host: env::var("YUE_ENGINE_HOST").ok(),
        port: env::var("YUE_ENGINE_PORT").ok().and_then(|value| value.parse().ok()),
        options: options.to_engine(),
    }
}

/// The weights the selected set resolves to, as paths the engine can open.
async fn selected_engine_models(state: &AppState) -> Result<music_engine::yue_server::YueModelFiles, String> {
    let selected_component_ids = state.selected_component_ids.read().await.clone();
    let selected_profile_id = state.selected_profile_id.read().await.clone();
    let files = match (selected_component_ids, selected_profile_id) {
        (Some(ids), _) => state.model_manager.installed_component_files(&ids),
        (None, Some(profile_id)) => state.model_manager.installed_profile_files(&profile_id),
        (None, None) => return Err("no model set is selected; choose one in Settings - Models".into()),
    }
    .map_err(|error| error.to_string())?;
    let root = state.model_manager.models_directory();
    let adapters = state.adapters.root().to_path_buf();
    fs::create_dir_all(&adapters).map_err(|error| format!("create the adapter folder {}: {error}", adapters.display()))?;
    Ok(music_engine::yue_server::YueModelFiles {
        backbone: root.join(&files.backbone),
        vae: root.join(&files.vae),
        transcriber: files.transcriber.map(|name| root.join(name)),
        adapters: Some(adapters),
    })
}

/// yue-server takes its weights at launch, so a different selection only takes
/// effect through a restart. A running engine this studio owns is restarted
/// when the files it serves are not the ones now selected; an engine that is
/// not running is left to the supervisor loop.
async fn reload_engine_if_models_changed(state: &AppState) {
    let Ok(wanted) = selected_engine_models(state).await else { return };
    let current = {
        let supervisor = state.engine.lock().await;
        supervisor.as_ref().map(|engine| engine.config().models.clone())
    };
    let Some(current) = current else { return };
    let same = |a: &std::path::Path, b: &std::path::Path| {
        fs::canonicalize(a).ok().zip(fs::canonicalize(b).ok()).is_some_and(|(a, b)| a == b)
    };
    let unchanged = same(&current.backbone, &wanted.backbone)
        && same(&current.vae, &wanted.vae)
        && match (&current.transcriber, &wanted.transcriber) {
            (Some(a), Some(b)) => same(a, b),
            (None, None) => true,
            _ => false,
        }
        && match (&current.adapters, &wanted.adapters) {
            (Some(a), Some(b)) => same(a, b),
            (None, None) => true,
            _ => false,
        };
    if unchanged {
        return;
    }
    music_engine::yue_server::note_in_log("the selected model set changed; restarting the engine on it");
    if let Err(error) = restart_engine(state).await {
        eprintln!("the engine did not restart on the new model set: {error}");
    }
}

async fn openrouter_catalog(State(state): State<AppState>) -> Json<Value> {
    // Fetch it if this process has not yet: an empty answer here made every
    // capability read "no model in the refreshed catalog", which is a lie -
    // the catalog had simply never been read.
    let catalog = catalog_for(&state).await.ok();
    let refreshed_at = state.openrouter_catalog.read().await.refreshed_at.clone();
    // What the studio would pick for each capability if the user picks
    // nothing. The panel shows these as the selection, so adding a key is
    // enough to start rather than the beginning of a shopping trip.
    let suggested = catalog.as_ref().map(|catalog| {
        serde_json::json!({
            "speech_to_text": providers::openrouter::suggested_model(catalog, Capability::SpeechToText),
            "prompt_enhancement": providers::openrouter::suggested_model(catalog, Capability::PromptEnhancement),
            "cover_art": providers::openrouter::suggested_model(catalog, Capability::CoverArt),
        })
    });
    Json(serde_json::json!({
        "models": catalog.map(|catalog| catalog.models),
        "refreshed_at": refreshed_at,
        "suggested": suggested,
    }))
}



/// Where the provider catalog is kept between runs.
fn openrouter_catalog_path() -> Option<PathBuf> {
    studio_data_root().map(|root| root.join("openrouter-catalog.json"))
}

/// Reads the catalog saved by the last refresh. Nothing here touches the
/// network: the studio refreshes when a key is connected and when the user
/// asks, and lives off this file the rest of the time.
fn load_cached_catalog() -> Option<providers::openrouter::CapabilityCatalog> {
    let path = openrouter_catalog_path()?;
    let body = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&body).ok()
}

fn save_cached_catalog(catalog: &providers::openrouter::CapabilityCatalog) {
    let Some(path) = openrouter_catalog_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(body) = serde_json::to_string(catalog) {
        let _ = std::fs::write(path, body);
    }
}

/// The capability catalog, fetched if this process has not got it yet.
///
/// The catalog lives in memory, so it is empty after every restart. Telling
/// the user to "refresh the catalog" at the moment they press a button is
/// asking them to do the program's job - and the refresh button lives on
/// another screen entirely.
/// The catalogue, and a fresh one when the model asked about is not in it.
///
/// The record is what the request is built from - the model's own parameters,
/// the efforts it accepts - so a catalogue that predates the model would have
/// the studio guessing about a model OpenRouter can describe exactly.
async fn catalog_describing(state: &AppState, model: &str) -> Result<providers::openrouter::CapabilityCatalog, String> {
    let catalog = catalog_for(state).await?;
    if model.is_empty() || catalog.models.iter().any(|entry| entry.id == model) {
        return Ok(catalog);
    }
    {
        let mut cached = state.openrouter_catalog.write().await;
        cached.catalog = None;
    }
    catalog_for(state).await
}

async fn catalog_for(state: &AppState) -> Result<providers::openrouter::CapabilityCatalog, String> {
    if let Some(catalog) = state.openrouter_catalog.read().await.catalog.clone() {
        return Ok(catalog);
    }
    // The last refresh, read from disk. A restart should not cost a request.
    if let Some(catalog) = load_cached_catalog() {
        let mut cached = state.openrouter_catalog.write().await;
        cached.catalog = Some(catalog.clone());
        return Ok(catalog);
    }
    let client = reqwest::Client::new();
    let fetch = |path: &'static str| {
        let client = client.clone();
        async move {
            client
                .get(format!("{}{}", providers::openrouter::API_BASE_URL, path))
                .send()
                .await?
                .error_for_status()?
                .text()
                .await
        }
    };
    let general = fetch(providers::openrouter::MODELS_PATH)
        .await
        .map_err(|error| format!("OpenRouter catalog request failed: {error}"))?;
    let transcription = fetch(providers::openrouter::TRANSCRIPTION_MODELS_PATH).await.unwrap_or_default();
    let images = fetch(providers::openrouter::IMAGE_MODELS_PATH).await.unwrap_or_default();
    let parsed = providers::openrouter::CapabilityCatalog::parse_merged(&general, &[&transcription, &images])
    .map_err(|error| format!("OpenRouter catalog parse failed: {error}"))?;
    save_cached_catalog(&parsed);
    let mut cached = state.openrouter_catalog.write().await;
    cached.catalog = Some(parsed.clone());
    cached.refreshed_at = Some(chrono_like_timestamp());
    Ok(parsed)
}

async fn refresh_openrouter_catalog(State(state): State<AppState>) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let client = reqwest::Client::new();
    let fetch = |path: &'static str| {
        let client = client.clone();
        async move {
            client
                .get(format!("{}{}", providers::openrouter::API_BASE_URL, path))
                .send()
                .await?
                .error_for_status()?
                .text()
                .await
        }
    };
    let general = fetch(providers::openrouter::MODELS_PATH)
        .await
        .map_err(|error| api_error(StatusCode::BAD_GATEWAY, format!("OpenRouter catalog request failed: {error}")))?;
    // The recognisers live behind their own filter; without this second call
    // the catalog contains no model that can return timings.
    let transcription = fetch(providers::openrouter::TRANSCRIPTION_MODELS_PATH).await.unwrap_or_default();
    let images = fetch(providers::openrouter::IMAGE_MODELS_PATH).await.unwrap_or_default();
    let parsed = providers::openrouter::CapabilityCatalog::parse_merged(&general, &[&transcription, &images])
    .map_err(|error| api_error(StatusCode::BAD_GATEWAY, format!("OpenRouter catalog parse failed: {error}")))?;
    let refreshed_at = chrono_like_timestamp();
    let models = parsed.models.clone();
    save_cached_catalog(&parsed);
    let mut cached = state.openrouter_catalog.write().await;
    cached.catalog = Some(parsed);
    cached.refreshed_at = Some(refreshed_at.clone());
    Ok(Json(serde_json::json!({ "models": models, "refreshed_at": refreshed_at })))
}

/// Sends an already catalog-validated OpenRouter JSON request. The frontend
/// supplies a model selection and input data, never an API key or endpoint.
async fn execute_openrouter_json(
    request: providers::openrouter::OpenRouterRequest,
) -> anyhow::Result<OpenRouterResponse> {
    let authenticated = providers::openrouter::authenticated_request_for(request)?;
    // Every cloud request passes through here, so this is where they are all
    // written down: which model, how long, and what came back when it was not
    // a success.
    let what = authenticated.request.path.trim_matches('/');
    let model = authenticated
        .request
        .body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string();
    request_log::asked(what, &model, authenticated.request.body.to_string().chars().count());
    let started = std::time::Instant::now();
    let response = reqwest::Client::new()
        .post(format!("{}{}", providers::openrouter::API_BASE_URL, authenticated.request.path))
        .bearer_auth(authenticated.api_key)
        .json(&authenticated.request.body)
        .send()
        .await
        .inspect_err(|error| request_log::failed(what, &model, &error.to_string()))?;
    let status = response.status();
    let response = match response.error_for_status_ref() {
        Ok(_) => response,
        Err(error) => {
            let text = response.text().await.unwrap_or_default();
            request_log::answered(what, &model, status.as_u16(), started.elapsed().as_secs_f64(), text.chars().count());
            request_log::unusable(what, &model, &error.to_string(), &text);
            return Err(error.into());
        }
    };
    let generation_id = response
        .headers()
        .get("X-Generation-Id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let body = response.json::<Value>().await?;
    request_log::answered(what, &model, status.as_u16(), started.elapsed().as_secs_f64(), body.to_string().chars().count());
    Ok(OpenRouterResponse { body, generation_id })
}

async fn create_openrouter_transcription(
    State(state): State<AppState>,
    Json(input): Json<OpenRouterTranscriptionRequest>,
) -> Result<Json<OpenRouterResponse>, (StatusCode, Json<ApiError>)> {
    let catalog = catalog_for(&state)
        .await
        .map_err(|error| api_error(StatusCode::BAD_GATEWAY, error))?;
    let request = providers::openrouter::stt_request_for(
        &catalog,
        &input.model_id,
        providers::openrouter::Base64AudioInput {
            timestamps: false,
            data: &input.audio_base64,
            format: &input.audio_format,
            language: input.language.as_deref(),
        },
    )
    .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?;
    execute_openrouter_json(request)
        .await
        .map(Json)
        .map_err(|error| api_error(StatusCode::BAD_GATEWAY, format!("OpenRouter transcription failed: {error}")))
}

async fn create_openrouter_cover(
    State(state): State<AppState>,
    Json(input): Json<OpenRouterCoverRequest>,
) -> Result<Json<OpenRouterResponse>, (StatusCode, Json<ApiError>)> {
    let catalog = catalog_for(&state)
        .await
        .map_err(|error| api_error(StatusCode::BAD_GATEWAY, error))?;
    let request = providers::openrouter::request_for(
        &catalog,
        Capability::CoverArt,
        &input.model_id,
        &input.prompt,
    )
    .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?;
    execute_openrouter_json(request)
        .await
        .map(Json)
        .map_err(|error| api_error(StatusCode::BAD_GATEWAY, format!("OpenRouter cover generation failed: {error}")))
}

/// Text assistance (caption and lyric drafting). The model must declare the
/// prompt-enhancement capability in the refreshed catalog, so the studio can
/// never send this to an image or audio-only endpoint.
async fn create_openrouter_completion(
    State(state): State<AppState>,
    Json(input): Json<OpenRouterCompletionRequest>,
) -> Result<Json<OpenRouterResponse>, (StatusCode, Json<ApiError>)> {
    let catalog = catalog_for(&state)
        .await
        .map_err(|error| api_error(StatusCode::BAD_GATEWAY, error))?;
    let request = providers::openrouter::request_for(
        &catalog,
        Capability::PromptEnhancement,
        &input.model_id,
        &input.prompt,
    )
    .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?;
    execute_openrouter_json(request)
        .await
        .map(Json)
        .map_err(|error| api_error(StatusCode::BAD_GATEWAY, format!("OpenRouter completion failed: {error}")))
}

/// The loopback port the studio serves on. Overridable for development, so a
/// second instance can run beside a released one.
fn listen_port() -> u16 {
    env::var("YUE_STUDIO_PORT").ok().and_then(|value| value.parse().ok()).unwrap_or(8791)
}

fn chrono_like_timestamp() -> String { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|value| value.as_secs().to_string()).unwrap_or_default() }

fn studio_settings_path() -> PathBuf {
    env::var_os("YUE_STUDIO_SETTINGS_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(default_studio_settings_path)
}

fn default_studio_settings_path() -> PathBuf {
    studio_data_root()
        .unwrap_or_else(|| env::temp_dir().join("yue2-studio"))
        .join("studio-settings.json")
}

/// Single per-user directory for every piece of Studio runtime data: settings,
/// library, media and locally stored provider credentials.
pub fn studio_data_root() -> Option<PathBuf> {
    if let Some(root) = env::var_os("YUE_STUDIO_DATA_ROOT") {
        return Some(PathBuf::from(root));
    }

    #[cfg(windows)]
    {
        if let Some(root) = env::var_os("LOCALAPPDATA").or_else(|| env::var_os("APPDATA")) {
            return Some(PathBuf::from(root).join("YuE2 Studio"));
        }
    }

    #[cfg(not(windows))]
    {
        if let Some(root) = env::var_os("XDG_DATA_HOME") {
            return Some(PathBuf::from(root).join("yue2-studio"));
        }
        if let Some(home) = env::var_os("HOME") {
            return Some(PathBuf::from(home).join(".local/share/yue2-studio"));
        }
    }

    None
}

fn load_studio_settings(path: &PathBuf) -> Option<PersistedStudioSettings> {
    fs::read_to_string(path).ok().and_then(|body| serde_json::from_str(&body).ok())
}

async fn persist_studio_settings(state: &AppState) -> anyhow::Result<()> {
    let settings = PersistedStudioSettings {
        engine_options: *state.engine_options.read().await,
        assistant: state.assistant.read().await.clone(),
        lyrics_sync: state.lyrics_sync_config.read().await.clone(),
        configuration: state.configuration.read().await.clone(),
        selected_profile_id: state.selected_profile_id.read().await.clone(),
        selected_component_ids: state.selected_component_ids.read().await.clone(),
        cover_templates: Some(state.cover_templates.read().await.clone()),
        cover_template_default: state.cover_template_default.read().await.clone(),
        separation: Some(state.separation_config.read().await.clone()),
        cover_auto: Some(*state.cover_auto.read().await),
    };
    if let Some(parent) = state.settings_path.parent() { fs::create_dir_all(parent)?; }
    let temporary = state.settings_path.with_extension("json.part");
    fs::write(&temporary, serde_json::to_vec_pretty(&settings)?)?;
    fs::rename(temporary, &state.settings_path)?;
    Ok(())
}

/// The set Studio will actually load. `None` means nothing has been selected
/// yet, so the manager falls back to the hardware recommendation for progress
/// reporting only — it still never downloads anything on its own.
async fn effective_install_target(state: &AppState) -> Option<InstallRequest> {
    if let Some(component_ids) = state.selected_component_ids.read().await.clone() {
        return Some(InstallRequest { profile_id: None, component_ids });
    }
    state
        .selected_profile_id
        .read()
        .await
        .clone()
        .map(|profile_id| InstallRequest { profile_id: Some(profile_id), component_ids: vec![] })
}

async fn compose_setup_status(state: &AppState, manager_status: model_manager::ManagerStatus) -> Value {
    let mut status = serde_json::to_value(manager_status).unwrap_or_else(|_| serde_json::json!({}));
    let selected_profile_id = state.selected_profile_id.read().await.clone();
    let selected_component_ids = state.selected_component_ids.read().await.clone();
    let selected_set_ready = match (&selected_profile_id, &selected_component_ids) {
        (_, Some(component_ids)) => state.model_manager.installed_component_files(component_ids).is_ok(),
        (Some(profile_id), None) => state.model_manager.installed_profile_files(profile_id).is_ok(),
        (None, None) => false,
    };
    if let Value::Object(ref mut fields) = status {
        fields.insert(
            "engine_ready".into(),
            Value::Bool(state.music_server.health().await),
        );
        fields.insert("engine_id".into(), Value::String(PRIMARY_MUSIC_ENGINE_ID.into()));
        fields.insert("selected_profile_id".into(), serde_json::to_value(selected_profile_id).unwrap_or(Value::Null));
        fields.insert("selected_component_ids".into(), serde_json::to_value(selected_component_ids).unwrap_or(Value::Null));
        fields.insert("hardware".into(), serde_json::to_value(hardware::hardware()).unwrap_or(Value::Null));
        fields.insert("engine_options".into(), serde_json::to_value(*state.engine_options.read().await).unwrap_or(Value::Null));
        fields.insert("effective_max_batch".into(), Value::from(state.engine_options.read().await.effective_max_batch()));
        // Where everything the studio owns actually lives. People complained
        // they could not find the ten gigabytes afterwards, let alone delete
        // them; the model root is already reported, this is the folder that
        // holds it along with the library, the media and the logs.
        fields.insert(
            "data_directory".into(),
            studio_data_root().map(|root| Value::String(root.display().to_string())).unwrap_or(Value::Null),
        );
        fields.insert("portable".into(), Value::Bool(is_portable_installation()));
        // Half a gigabyte of CUDA libraries arriving is the difference between
        // an engine that starts in three seconds and one that starts in ten
        // minutes. A spinner that says nothing for ten minutes is the same
        // screen as a spinner that is stuck.
        let runtime_total = engine_runtime::ASSETS.iter().map(|asset| asset.bytes).sum::<u64>();
        let runtime_active = state.engine_runtime.downloader().active().await;
        fields.insert(
            "engine_runtime".into(),
            serde_json::json!({
                "ready": state.engine_runtime.is_ready(state.engine_options.read().await.uses_cuda()),
                "downloading": runtime_active.is_some(),
                "downloaded_bytes": runtime_active.as_ref().map(|progress| progress.downloaded_bytes).unwrap_or(0),
                "total_bytes": runtime_total,
                "error": runtime_active.and_then(|progress| progress.error),
            }),
        );
        fields.insert("ready".into(), Value::Bool(selected_set_ready));
        fields.insert("first_run".into(), Value::Bool(!selected_set_ready));
        if selected_set_ready { fields.insert("download_pending".into(), Value::from(0_u64)); }
    }
    status
}

/// Whether this copy keeps everything beside its own executable.
///
/// The desktop shell decides it by the marker file next to the binary and then
/// hands the service the data root; the service reports it so the interface can
/// say "this folder is the whole studio" rather than sending people hunting
/// through AppData.
fn is_portable_installation() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|directory| directory.join("portable.flag")))
        .is_some_and(|marker| marker.is_file())
}

/// Recent native engine output: `/job` reports a phase, everything finer lives
/// in the log. While the engine is starting it has no HTTP log yet, so the file
/// it writes from its first line is what gets shown.
async fn engine_logs(State(state): State<AppState>) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let (lines, source) = match state.music_server.logs_snapshot(std::time::Duration::from_millis(700)).await {
        Ok(lines) => (lines, "engine"),
        Err(_) => (music_engine::yue_server::startup_log_tail(120), "startup"),
    };
    if lines.is_empty() {
        return Err(api_error(StatusCode::SERVICE_UNAVAILABLE, "the engine log is empty: the engine has not started yet".into()));
    }
    let progress = progress::from_log(&lines);
    Ok(Json(serde_json::json!({ "engine_id": PRIMARY_MUSIC_ENGINE_ID, "lines": lines, "source": source, "progress": progress })))
}

/// Live machine resources. ACE Studio's resource readout is kept, but every
/// value now comes from a real measurement on this machine.
async fn system_resources() -> Json<Value> {
    let snapshot = tokio::task::spawn_blocking(resources::snapshot)
        .await
        .unwrap_or_else(|_| resources::snapshot());
    Json(serde_json::json!({
        "poll_interval_ms": resources::SUGGESTED_INTERVAL.as_millis() as u64,
        "resources": snapshot,
    }))
}

/// Fetches a remote image on behalf of the video composer.
///
/// The canvas has to stay untainted to read frames back, which a cross-origin
/// image without CORS headers prevents. Only http(s) is accepted and the
/// response must actually be an image, so this cannot be used to reach local
/// services or to pull arbitrary files.
async fn proxy_image(
    State(state): State<AppState>,
    axum::extract::Query(request): axum::extract::Query<ProxyImageRequest>,
) -> Result<axum::response::Response, (StatusCode, Json<ApiError>)> {
    let url = reqwest::Url::parse(&request.url)
        .map_err(|error| api_error(StatusCode::BAD_REQUEST, format!("invalid image url: {error}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(api_error(StatusCode::BAD_REQUEST, "only http and https images can be proxied".into()));
    }
    if url.host_str().is_some_and(|host| host == "localhost" || host.starts_with("127.") || host == "0.0.0.0" || host == "[::1]") {
        return Err(api_error(StatusCode::BAD_REQUEST, "loopback addresses cannot be proxied".into()));
    }
    let response = state
        .music_server
        .http
        .get(url)
        .send()
        .await
        .map_err(|error| api_error(StatusCode::BAD_GATEWAY, format!("image request failed: {error}")))?;
    let status = response.status();
    if !status.is_success() {
        return Err(api_error(StatusCode::BAD_GATEWAY, format!("image request returned {status}")));
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    if !content_type.starts_with("image/") {
        return Err(api_error(StatusCode::UNSUPPORTED_MEDIA_TYPE, "the proxied url is not an image".into()));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|error| api_error(StatusCode::BAD_GATEWAY, format!("reading the image failed: {error}")))?;
    Ok(axum::response::Response::builder()
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, bytes.len())
        .body(Body::from(bytes.to_vec()))
        .expect("valid image response"))
}

async fn assistant_status(State(state): State<AppState>) -> Json<Value> {
    let config = state.assistant.read().await.clone();
    let runtime = state.assistant_runtime.status().await;
    // A managed model is only usable once its file and the runtime are on disk.
    let available = match config.provider {
        AssistantProvider::Managed => {
            let has_runtime = runtime.server_path.is_some();
            let downloaded = config
                .managed_model
                .as_deref()
                .is_some_and(|model| runtime.installed_models.iter().any(|id| id == model));
            let own_file = config
                .managed_path
                .as_deref()
                .is_some_and(|path| !path.trim().is_empty() && std::path::Path::new(path.trim()).is_file());
            has_runtime && (downloaded || own_file)
        }
        _ => config.available(),
    };
    Json(serde_json::json!({
        "available": available,
        "managed_model": config.managed_model,
        "managed_path": config.managed_path,
        "reasoning_effort": config.reasoning_effort,
        "runtime_ready": runtime.ready,
        "provider": config.provider,
        "local_base_url": config.local_base_url,
        "local_model": config.local_model,
        "openrouter_model": config.openrouter_model,
    }))
}

/// Every field optional, so the download page can set the provider without
/// blanking the model, the path and the reasoning effort it knows nothing of.
#[derive(Debug, Deserialize)]
struct AssistantSettingsRequest {
    provider: Option<AssistantProvider>,
    local_base_url: Option<Option<String>>,
    local_model: Option<Option<String>>,
    openrouter_model: Option<Option<String>>,
    managed_model: Option<Option<String>>,
    managed_path: Option<Option<String>>,
    reasoning_effort: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
struct LocalModelsQuery {
    base: String,
}

/// The models an OpenAI-compatible server the user runs offers, fetched through
/// the studio so the browser is never asked to reach another origin itself.
///
/// LM Studio, llama-server, Ollama's OpenAI shim - all answer `GET <base>/models`
/// with `{ "data": [ { "id": ... } ] }`. Typing the model name by hand, which
/// is what this replaces, meant a typo read as a server that answered nothing.
async fn assistant_local_models(
    axum::extract::Query(query): axum::extract::Query<LocalModelsQuery>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let base = query.base.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err(api_error(StatusCode::BAD_REQUEST, "no server address".into()));
    }
    let url = format!("{base}/models");
    let response = reqwest::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(8))
        .send()
        .await
        .map_err(|error| api_error(StatusCode::BAD_GATEWAY, format!("the server at {base} did not answer: {error}")))?;
    if !response.status().is_success() {
        return Err(api_error(StatusCode::BAD_GATEWAY, format!("{url} answered {}", response.status())));
    }
    let body = response
        .json::<Value>()
        .await
        .map_err(|error| api_error(StatusCode::BAD_GATEWAY, format!("{url} did not return JSON: {error}")))?;
    let models: Vec<String> = body
        .get("data")
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(|item| item.get("id").and_then(Value::as_str).map(str::to_string)).collect())
        .unwrap_or_default();
    Ok(Json(serde_json::json!({ "models": models })))
}

async fn update_assistant_settings(
    State(state): State<AppState>,
    Json(incoming): Json<AssistantSettingsRequest>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let request = {
        let current = state.assistant.read().await.clone();
        AssistantConfig {
            provider: incoming.provider.unwrap_or(current.provider),
            local_base_url: incoming.local_base_url.unwrap_or(current.local_base_url),
            local_model: incoming.local_model.unwrap_or(current.local_model),
            openrouter_model: incoming.openrouter_model.unwrap_or(current.openrouter_model),
            managed_model: incoming.managed_model.unwrap_or(current.managed_model),
            managed_path: incoming.managed_path.unwrap_or(current.managed_path),
            reasoning_effort: incoming.reasoning_effort.unwrap_or(current.reasoning_effort),
        }
    };
    if request.provider == AssistantProvider::Local {
        let base = request.local_base_url.as_deref().unwrap_or_default();
        let url = reqwest::Url::parse(base)
            .map_err(|error| api_error(StatusCode::BAD_REQUEST, format!("invalid assistant URL: {error}")))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(api_error(StatusCode::BAD_REQUEST, "the assistant URL must be http or https".into()));
        }
    }
    *state.assistant.write().await = request.clone();
    let _ = persist_studio_settings(&state).await;
    Ok(Json(serde_json::json!({ "available": request.available(), "provider": request.provider })))
}

#[derive(Debug, Deserialize)]
struct AssistantAssetRequest {
    asset_id: String,
    /// The model to install along with the runtime, when the capability has a
    /// choice of them. A runtime with no model does nothing.
    #[serde(default)]
    model_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AssistantModelRequest {
    #[serde(default)]
    model_id: String,
    /// A GGUF already on this machine, used instead of a downloaded one.
    #[serde(default)]
    model_path: Option<String>,
}

/// The runtime, and the choice it belongs to.
///
/// The panel reads this one address to draw itself, and the chosen engine is
/// kept with the assistant's settings rather than with its files - so without
/// it here the tabs came back to the first one on every open, whatever the user
/// had picked.
async fn assistant_runtime_status(State(state): State<AppState>) -> Json<Value> {
    let status = state.assistant_runtime.status().await;
    let provider = state.assistant.read().await.provider;
    let mut value = serde_json::to_value(&status).unwrap_or(Value::Null);
    let chosen = state.assistant.read().await.managed_model.clone();
    if let Value::Object(ref mut fields) = value {
        fields.insert("provider".into(), serde_json::to_value(provider).unwrap_or(Value::Null));
        // And which model, so the dropdown reopens on the one that was picked
        // rather than on whichever happens to be installed first.
        fields.insert("chosen_model".into(), serde_json::to_value(chosen).unwrap_or(Value::Null));
    }
    Json(value)
}

/// Starts one download. Nothing is fetched until this is called, and an
/// interrupted file resumes where it stopped.
/// The llama.cpp build for a device, with the CUDA libraries it needs.
///
/// The card build is useless without its runtime companion - two downloads
/// that are one decision, the same way a recogniser is.
fn assistant_set(device: &str) -> Vec<&'static str> {
    match device {
        "cpu" => vec!["llama-cpu"],
        _ => vec!["llama-cuda", "llama-cuda-runtime"],
    }
}

async fn assistant_runtime_install(
    State(state): State<AppState>,
    Json(request): Json<AssistantAssetRequest>,
) -> Result<Json<assistant_runtime::RuntimeStatus>, (StatusCode, Json<ApiError>)> {
    let set = match request.asset_id.as_str() {
        "auto" | "cuda" | "cpu" => assistant_set(&request.asset_id),
        _ => Vec::new(),
    };
    if !set.is_empty() {
        // Downloading a model is choosing it. Nothing else recorded which one,
        // so a freshly installed Gemma left the studio reporting that no
        // assistant was set up - with the model sitting on the disk.
        if let Some(model) = request.model_id.clone() {
            let mut assistant = state.assistant.write().await;
            assistant.managed_model = Some(model);
            if assistant.provider == AssistantProvider::None {
                assistant.provider = AssistantProvider::Managed;
            }
        }
        let _ = persist_studio_settings(&state).await;
        let runtime = state.assistant_runtime.clone();
        // The whole thing - runtime, CUDA libraries, model - as one download.
        // Starting them one after another only looked like a queue: each call
        // returned before its file had arrived, so the next one was refused and
        // the model, always last, was never fetched at all.
        let ids: Vec<String> = set
            .into_iter()
            .map(str::to_string)
            .chain(request.model_id.clone())
            .collect();
        tokio::spawn(async move {
            if let Err(error) = runtime.install_all(&ids).await {
                eprintln!("the assistant could not be installed: {error}");
            }
        });
        return Ok(Json(state.assistant_runtime.status().await));
    }
    state
        .assistant_runtime
        .install(&request.asset_id)
        .await
        .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?;
    Ok(Json(state.assistant_runtime.status().await))
}

/// How much room the local model gets.
///
/// A score edit sends the whole ABC score and gets the whole revision back,
/// two or three thousand tokens each way; a model that runs out mid-JSON
/// produces an answer nothing can parse.
const ASSISTANT_CONTEXT: u32 = 16384;

async fn assistant_runtime_start(
    State(state): State<AppState>,
    Json(request): Json<AssistantModelRequest>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let own_file = request.model_path.clone().unwrap_or_default();
    let reasoning = state.assistant.read().await.reasoning_effort.clone();
    let base_url = if own_file.trim().is_empty() {
        state.assistant_runtime.start(&request.model_id, ASSISTANT_CONTEXT, reasoning.as_deref()).await
    } else {
        state.assistant_runtime.start_path(std::path::Path::new(own_file.trim()), ASSISTANT_CONTEXT, reasoning.as_deref()).await
    }
    .map_err(|error| api_error(StatusCode::BAD_GATEWAY, error.to_string()))?;
    Ok(Json(serde_json::json!({ "base_url": base_url, "model_id": request.model_id, "model_path": own_file })))
}

async fn assistant_runtime_stop(State(state): State<AppState>) -> Json<Value> {
    state.assistant_runtime.stop().await;
    Json(serde_json::json!({ "running": false }))
}

#[derive(Debug, Deserialize)]
struct KaraokeRequest {
    /// Overrides the language guess for this one track.
    #[serde(default)]
    language: Option<String>,
}

/// What the chosen engine would install: its files, their weight, and how much
/// of it is already here.
fn set_progress(downloader: &crate::downloads::Downloader, set: &[&'static lyrics_sync::Asset]) -> Value {
    let total: u64 = set.iter().map(|asset| asset.bytes).sum();
    let installed: u64 = set.iter().filter(|asset| downloader.is_installed(asset)).map(|asset| asset.bytes).sum();
    serde_json::json!({
        "bytes": total,
        "installed_bytes": installed,
        "ready": !set.is_empty() && installed == total,
        "files": set.len(),
    })
}

async fn karaoke_status(State(state): State<AppState>) -> Json<Value> {
    let config = state.lyrics_sync_config.read().await.clone();
    let status = state.lyrics_sync.status(&config).await;
    let name = match config.provider {
        lyrics_sync::AsrProvider::Whisper => "whisper",
        _ => "parakeet",
    };
    let set = karaoke_set(name, config.runtime, config.whisper_model.as_deref());
    let mut value = serde_json::to_value(&status).unwrap_or(Value::Null);
    if let Value::Object(ref mut fields) = value {
        fields.insert("set".into(), set_progress(state.lyrics_sync.downloader(), &set));
    }
    Json(value)
}

/// Every field optional, so a panel that changes one thing changes one thing.
///
/// This took the whole configuration before: the download page, which knows
/// only which recogniser and which device were picked, would have blanked the
/// switch and both model choices by sending them absent.
#[derive(Debug, Deserialize)]
struct KaraokeSettingsRequest {
    enabled: Option<bool>,
    provider: Option<lyrics_sync::AsrProvider>,
    whisper_model: Option<Option<String>>,
    openrouter_model: Option<Option<String>>,
    runtime: Option<lyrics_sync::OnnxFlavour>,
}

async fn update_karaoke_settings(
    State(state): State<AppState>,
    Json(request): Json<KaraokeSettingsRequest>,
) -> Result<Json<lyrics_sync::SyncStatus>, (StatusCode, Json<ApiError>)> {
    let merged = {
        let mut config = state.lyrics_sync_config.write().await;
        if let Some(enabled) = request.enabled { config.enabled = enabled; }
        if let Some(provider) = request.provider { config.provider = provider; }
        if let Some(model) = request.whisper_model { config.whisper_model = model; }
        if let Some(model) = request.openrouter_model { config.openrouter_model = model; }
        if let Some(runtime) = request.runtime { config.runtime = runtime; }
        config.clone()
    };
    let _ = persist_studio_settings(&state).await;
    Ok(Json(state.lyrics_sync.status(&merged).await))
}

/// Frees the disk a karaoke recogniser takes.
/// Removes a recogniser the same way it was installed: whole.
async fn karaoke_remove(
    State(state): State<AppState>,
    Json(request): Json<AssistantAssetRequest>,
) -> Result<Json<lyrics_sync::SyncStatus>, (StatusCode, Json<ApiError>)> {
    let config = state.lyrics_sync_config.read().await.clone();
    let set = karaoke_set(&request.asset_id, config.runtime, config.whisper_model.as_deref());
    if !set.is_empty() {
        for asset in set {
            let _ = state.lyrics_sync.downloader().remove(asset);
        }
        return Ok(Json(state.lyrics_sync.status(&config).await));
    }
    let asset = lyrics_sync::asset(&request.asset_id)
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, format!("unknown karaoke asset: {}", request.asset_id)))?;
    state
        .lyrics_sync
        .downloader()
        .remove(asset)
        .map_err(|error| api_error(StatusCode::CONFLICT, error.to_string()))?;
    Ok(Json(state.lyrics_sync.status(&config).await))
}

/// Frees the disk the stem separation model takes.
async fn remove_separation_model(State(state): State<AppState>) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let freed = state
        .separator
        .downloader()
        .remove(&separation::MODEL)
        .map_err(|error| api_error(StatusCode::CONFLICT, error.to_string()))?;
    Ok(Json(serde_json::json!({ "freed_bytes": freed })))
}

/// Installs a recogniser, not a file.
///
/// Parakeet is six downloads and Whisper is two, and which two depends on the
/// card. Asking a person to work that out from a list of file names - and to
/// notice that one of them is a runtime - is not a setup screen, it is a quiz.
/// The whole set is named here, in the order it is used.
fn karaoke_set(name: &str, device: lyrics_sync::OnnxFlavour, whisper_model: Option<&str>) -> Vec<&'static lyrics_sync::Asset> {
    let mut wanted: Vec<String> = Vec::new();
    match name {
        "parakeet" => {
            wanted.push("onnxruntime".into());
            if !matches!(device, lyrics_sync::OnnxFlavour::Cpu) {
                wanted.extend(CARD_ASSETS.map(String::from));
            }
            // The precision is chosen the same way a Whisper model is: through
            // the dropdown, which names one of the two encoders.
            if whisper_model.is_some_and(|id| id.contains("fp32")) {
                wanted.extend(lyrics_sync::PARAKEET_FP32_ASSET_IDS.map(String::from));
            } else {
                wanted.extend(lyrics_sync::PARAKEET_ASSET_IDS.map(String::from));
            }
        }
        "whisper" => {
            // One binary whichever device is chosen; the card needs CUDA 11's
            // libraries beside it, and without them CTranslate2 silently uses
            // the processor instead of saying so.
            wanted.push("whisper-engine".into());
            if !matches!(device, lyrics_sync::OnnxFlavour::Cpu) {
                wanted.push("whisper-cublas".into());
                wanted.push("whisper-cudnn".into());
            }
            // A model is a directory of files, and it is useless one file
            // short, so the whole set goes together.
            let chosen = whisper_model.unwrap_or("whisper-large-v3-turbo");
            if let Some(size) = chosen.strip_prefix("whisper-") {
                let prefix = format!("models/whisper/faster-whisper-{size}/");
                wanted.extend(
                    lyrics_sync::ASSETS
                        .iter()
                        .filter(|asset| asset.relative_path.starts_with(&prefix))
                        .map(|asset| asset.id.to_string()),
                );
            }
        }
        _ => {}
    }
    wanted.iter().filter_map(|id| lyrics_sync::asset(id)).collect()
}

async fn karaoke_install(
    State(state): State<AppState>,
    Json(request): Json<AssistantAssetRequest>,
) -> Result<Json<lyrics_sync::SyncStatus>, (StatusCode, Json<ApiError>)> {
    let config = state.lyrics_sync_config.read().await.clone();
    let set = karaoke_set(
        &request.asset_id,
        config.runtime,
        request.model_id.as_deref().or(config.whisper_model.as_deref()),
    );
    if !set.is_empty() {
        // In the background, so the panel keeps answering while half a
        // gigabyte arrives; the whole set is one button, not eight.
        let sync = state.lyrics_sync.clone();
        let installed_state = state.clone();
        let recogniser = match request.asset_id.as_str() {
            "parakeet" => Some(lyrics_sync::AsrProvider::Parakeet),
            "whisper" => Some(lyrics_sync::AsrProvider::Whisper),
            _ => None,
        };
        tokio::spawn(async move {
            match sync.downloader().install_all("karaoke", &set).await {
                Err(error) => eprintln!("the karaoke recogniser could not be installed: {error}"),
                // Installing a recogniser is choosing it: the timings button
                // appears once it is on disk, without a second trip to Settings.
                Ok(_) => {
                    if let Some(provider) = recogniser {
                        {
                            let mut config = installed_state.lyrics_sync_config.write().await;
                            if config.provider == lyrics_sync::AsrProvider::None {
                                config.provider = provider;
                            }
                            config.enabled = true;
                        }
                        let _ = persist_studio_settings(&installed_state).await;
                    }
                }
            }
        });
        return Ok(Json(state.lyrics_sync.status(&config).await));
    }

    let asset = lyrics_sync::asset(&request.asset_id)
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, format!("unknown karaoke asset: {}", request.asset_id)))?;
    state
        .lyrics_sync
        .downloader()
        .install(asset)
        .await
        .map_err(|error| api_error(StatusCode::CONFLICT, error.to_string()))?;
    Ok(Json(state.lyrics_sync.status(&config).await))
}

/// Times one track's own lyrics and stores the result with it.
///
/// Recognition is CPU or GPU bound and takes tens of seconds, so it runs on a
/// blocking thread rather than holding an async worker hostage.
async fn create_song_karaoke(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<KaraokeRequest>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let config = state.lyrics_sync_config.read().await.clone();
    if !config.available() {
        return Err(api_error(StatusCode::CONFLICT, "karaoke.off".into()));
    }
    // Pressing the button on a track is the instruction to time it. If the
    // chosen local recogniser is not on disk yet, that is a download to start,
    // not a refusal to hand back.
    if !ensure_local_recogniser(&state, &config, &id).await {
        return Err(api_error(StatusCode::CONFLICT, "karaoke.model-missing".into()));
    }
    let song = state
        .library
        .get_song(&id)
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "no such song".into()))?;
    let audio = song
        .audio_path
        .clone()
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "this track has no audio to listen to".into()))?;
    if !auto_title::has_sung_lines(&song.lyrics) {
        return Err(api_error(StatusCode::BAD_REQUEST, "karaoke.instrumental".into()));
    }

    let words = match config.provider {
        lyrics_sync::AsrProvider::None => {
            return Err(api_error(StatusCode::CONFLICT, "karaoke.no-recogniser".into()))
        }
        lyrics_sync::AsrProvider::Parakeet => {
            let sync = state.lyrics_sync.clone();
            let path = std::path::PathBuf::from(&audio);
            tokio::task::spawn_blocking(move || sync.parakeet_words(&path))
                .await
                .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
        }
        lyrics_sync::AsrProvider::Whisper => {
            let sync = state.lyrics_sync.clone();
            let config = config.clone();
            let path = std::path::PathBuf::from(&audio);
            let language = request.language.clone();
            let lyrics = song.lyrics.clone();
            tokio::task::spawn_blocking(move || sync.whisper_words(&config, &path, language.as_deref(), &lyrics))
                .await
                .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
        }
        lyrics_sync::AsrProvider::OpenRouter => {
            karaoke_words_from_openrouter(&state, &config, &audio, request.language.as_deref()).await
        }
    }
    .map_err(|error| api_error(StatusCode::BAD_GATEWAY, error.to_string()))?;

    // Word by word, because that is what karaoke means: a line time alone
    // leaves a player sweeping the highlight linearly through the line, which
    // drifts off the singing immediately.
    let lines = lyrics_sync::align_lyrics_words(&words, &song.lyrics);
    if lines.is_empty() {
        return Err(api_error(StatusCode::BAD_GATEWAY, "karaoke.no-match".into()));
    }
    let lrc = lyrics_sync::enhanced_lrc(&lines);
    state
        .library
        .set_song_lrc(&id, &lrc)
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(Json(serde_json::json!({ "lrc": lrc, "lines": lines.len(), "provider": config.provider })))
}

async fn delete_song_karaoke(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    state
        .library
        .set_song_lrc(&id, "")
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(Json(serde_json::json!({ "lrc": Value::Null })))
}

/// The cloud path: base64 the audio, ask for verbose output, read the times.
async fn karaoke_words_from_openrouter(
    state: &AppState,
    config: &lyrics_sync::LyricsSyncConfig,
    audio: &str,
    language: Option<&str>,
) -> anyhow::Result<Vec<(f64, String)>> {
    use base64::Engine as _;
    let catalog = catalog_for(state).await.map_err(|error| anyhow::anyhow!(error))?;
    let model = config
        .openrouter_model
        .clone()
        .filter(|value| !value.trim().is_empty())
        // Only the Whisper family returns timings, and that is what karaoke is.
        .or_else(|| providers::openrouter::suggested_model(&catalog, Capability::SpeechToText))
        .unwrap_or_default();
    let bytes = std::fs::read(audio)?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let format = std::path::Path::new(audio)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("mp3")
        .to_ascii_lowercase();
    let request = providers::openrouter::stt_request_for(
        &catalog,
        &model,
        providers::openrouter::Base64AudioInput { timestamps: true, data: &encoded, format: &format, language },
    )?;
    let response = execute_openrouter_json(request).await?;
    let segments = lyrics_sync::segments_from_verbose_json(&response.body);
    if segments.is_empty() {
        anyhow::bail!("{model} answered without timings; pick a model that returns them");
    }
    Ok(segments)
}

/// Writes lyrics and/or the structured caption. Optional by design: with no
/// provider configured this answers 409 and the manual form is unaffected.

/// The same request as `assistant_write`, reported while it happens.
///
/// A model can take a minute, and a button that only spins says nothing about
/// whether the request even left the machine. This sends the stages as they
/// occur - the request going out, the first token coming back - and then the
/// text itself, piece by piece, so the fields fill in front of the user.

/// The OpenRouter model the writing assistant should use.
///
/// Two screens name this: the provider page, where every capability picks its
/// model, and the assistant page, which has a field of its own. They disagreed,
/// and the request went to whichever the code happened to read - so the panel
/// showed one model while another answered. The provider selection wins,
/// because that page is where every other capability is chosen.
async fn assistant_openrouter_model(state: &AppState, config: &AssistantConfig) -> String {
    let selected = state
        .configuration
        .read()
        .await
        .selections
        .iter()
        .find(|selection| selection.capability == Capability::PromptEnhancement)
        .and_then(|selection| selection.cloud_model.clone())
        .filter(|model| !model.trim().is_empty());
    if let Some(model) = selected {
        return model;
    }
    if let Some(model) = config.openrouter_model.clone().filter(|model| !model.trim().is_empty()) {
        return model;
    }
    catalog_for(state)
        .await
        .ok()
        .and_then(|catalog| providers::openrouter::suggested_model(&catalog, Capability::PromptEnhancement))
        .unwrap_or_default()
}

async fn assistant_write_stream(
    State(state): State<AppState>,
    Json(request): Json<assistant::AssistRequest>,
) -> Result<axum::response::Response, (StatusCode, Json<ApiError>)> {
    let config = state.assistant.read().await.clone();
    if !config.available() {
        return Err(api_error(
            StatusCode::CONFLICT,
            "No writing assistant is configured. The manual form does not need one.".into(),
        ));
    }
    let (system, required) = assistant::instructions(&request);
    let user = assistant::user_message(&request);

    let (sender, receiver) = tokio::sync::mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(64);
    let emit = |sender: tokio::sync::mpsc::Sender<Result<axum::body::Bytes, std::io::Error>>, event: Value| async move {
        let line = format!("data: {event}\n\n");
        let _ = sender.send(Ok(axum::body::Bytes::from(line))).await;
    };

    tokio::spawn(async move {
        emit(sender.clone(), serde_json::json!({ "stage": "preparing" })).await;

        // Where the request goes, and with which model.
        let (base, model, key): (String, String, Option<String>) = match config.provider {
            AssistantProvider::OpenRouter => {
                let model = assistant_openrouter_model(&state, &config).await;
                let key = match credentials::openrouter_api_key().map(|(key, _)| key) {
                    Some(key) => key,
                    None => {
                        emit(sender.clone(), serde_json::json!({ "error": "no OpenRouter key is stored" })).await;
                        return;
                    }
                };
                ("https://openrouter.ai/api/v1".to_string(), model, Some(key))
            }
            AssistantProvider::Managed => {
                let own_file = config.managed_path.clone().unwrap_or_default();
                let id = config.managed_model.clone().unwrap_or_default();
                let reasoning = config.reasoning_effort.clone();
                let started = if own_file.trim().is_empty() {
                    state.assistant_runtime.start(&id, ASSISTANT_CONTEXT, reasoning.as_deref()).await
                } else {
                    state.assistant_runtime.start_path(std::path::Path::new(own_file.trim()), ASSISTANT_CONTEXT, reasoning.as_deref()).await
                };
                match started {
                    Ok(base) => (base, if own_file.trim().is_empty() { id } else { "local-model".to_string() }, None),
                    Err(error) => {
                        emit(sender.clone(), serde_json::json!({ "error": format!("the local assistant did not start: {error}") })).await;
                        return;
                    }
                }
            }
            _ => (
                config.local_base_url.clone().unwrap_or_default(),
                config.local_model.clone().unwrap_or_default(),
                None,
            ),
        };

        // A local llama-server enforces the shape while it samples, so the
        // answer cannot come back as prose or as a list where a string belongs.
        let schema = matches!(config.provider, AssistantProvider::Managed | AssistantProvider::Local)
            .then(|| assistant::draft_schema(&required));
        // What the model publishes for itself, exactly as the non-streaming
        // path uses it. Passing nothing here meant every streamed request went
        // out with the studio's own temperature on top of models that had
        // stated their own - a different request from the one the catalogue
        // describes, and the streamed path is the one the window uses.
        let entry = if matches!(config.provider, AssistantProvider::OpenRouter) {
            catalog_describing(&state, &model)
                .await
                .ok()
                .and_then(|catalog| catalog.models.iter().find(|item| item.id == model).cloned())
        } else {
            None
        };
        let published = entry.as_ref().map(|entry| serde_json::to_value(&entry.defaults).unwrap_or(Value::Null));
        // Thinking on the model's own terms: it publishes which efforts it
        // takes, which one it prefers, and whether it can be asked not to
        // think at all. A setting of ours that is not on its list becomes the
        // one it named, because naming an unknown effort is refused outright.
        let effort = match (&entry, config.provider) {
            (Some(entry), AssistantProvider::OpenRouter) => entry
                .reasoning
                .as_ref()
                .and_then(|reasoning| reasoning.effort_for(config.reasoning_effort.as_deref())),
            (_, AssistantProvider::OpenRouter) => None,
            _ => config.reasoning_effort.clone(),
        };
        let mut body = assistant::chat_body_constrained(&model, &system, &user, effort.as_deref(), published.as_ref(), schema);
        body["stream"] = Value::Bool(true);

        emit(sender.clone(), serde_json::json!({ "stage": "sent", "model": model })).await;
        request_log::asked("assistant", &model, system.chars().count() + user.chars().count());
        let started = std::time::Instant::now();

        let client = reqwest::Client::new();
        let mut outgoing = client
            .post(format!("{}/chat/completions", base.trim_end_matches('/')))
            .json(&body)
            .timeout(std::time::Duration::from_secs(600));
        if let Some(key) = key {
            outgoing = outgoing
                .header(reqwest::header::AUTHORIZATION, format!("Bearer {key}"))
                .header("HTTP-Referer", "https://github.com/timoncool/YuE2-Studio")
                .header("X-Title", "YuE2 Studio");
        }

        let response = match outgoing.send().await {
            Ok(response) => response,
            Err(error) => {
                request_log::failed("assistant", &model, &error.to_string());
                emit(sender.clone(), serde_json::json!({ "error": format!("the assistant is unreachable: {error}") })).await;
                return;
            }
        };
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            request_log::answered("assistant", &model, status.as_u16(), started.elapsed().as_secs_f64(), text.chars().count());
            request_log::unusable("assistant", &model, &format!("http {status}"), &text);
            emit(sender.clone(), serde_json::json!({ "error": format!("the assistant returned {status}: {text}") })).await;
            return;
        }

        // Server-sent events, one JSON object per `data:` line, with the text in
        // `choices[0].delta.content`.
        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        let mut first = true;
        let mut whole = String::new();
        while let Some(chunk) = stream.next().await {
            let Ok(chunk) = chunk else { break };
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(line_end) = buffer.find('\n') {
                let line = buffer[..line_end].trim().to_string();
                buffer.drain(..line_end + 1);
                let Some(payload) = line.strip_prefix("data:") else { continue };
                let payload = payload.trim();
                if payload == "[DONE]" {
                    continue;
                }
                let Ok(event): Result<Value, _> = serde_json::from_str(payload) else { continue };
                let delta = event
                    .get("choices")
                    .and_then(|choices| choices.get(0))
                    .and_then(|choice| choice.get("delta"))
                    .and_then(|delta| delta.get("content"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if delta.is_empty() {
                    continue;
                }
                if first {
                    first = false;
                    // The moment the model started answering. Without it a log
                    // of a run that never came back cannot say whether it was
                    // thinking or simply gone.
                    request_log::answered("assistant first token", &model, 200, started.elapsed().as_secs_f64(), 0);
                    emit(sender.clone(), serde_json::json!({ "stage": "writing" })).await;
                }
                whole.push_str(delta);
                emit(sender.clone(), serde_json::json!({ "delta": delta })).await;
            }
        }

        request_log::answered("assistant", &model, 200, started.elapsed().as_secs_f64(), whole.chars().count());
        // The answer is kept whenever it cannot be turned into a draft. That is
        // the case this log exists for: the window shows one red line, and
        // without this the text behind it is gone the moment it is closed.
        if let Err(error) = assistant::parse_draft(&whole, &required) {
            request_log::unusable("assistant", &model, &error.to_string(), &whole);
        }
        emit(sender.clone(), serde_json::json!({ "stage": "done", "text": whole })).await;
        // The card belongs to whatever runs next unless the user asked for
        // everything to stay resident.
        release_assistant_unless_kept(&state).await;
    });

    // A channel of chunks becomes the response body; the receiver is turned into
    // a stream by hand to avoid another dependency for four lines.
    let stream = futures_util::stream::unfold(receiver, |mut receiver| async move {
        receiver.recv().await.map(|item| (item, receiver))
    });
    let body = Body::from_stream(stream);
    Ok(axum::response::Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(body)
        .expect("valid stream response"))
}

async fn assistant_write(
    State(state): State<AppState>,
    Json(request): Json<assistant::AssistRequest>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let config = state.assistant.read().await.clone();
    if !config.available() {
        return Err(api_error(
            StatusCode::CONFLICT,
            "No writing assistant is configured. The manual form does not need one.".into(),
        ));
    }
    let (system, required) = assistant::instructions(&request);
    let user = assistant::user_message(&request);

    let response: Value = match config.provider {
        AssistantProvider::Local | AssistantProvider::Managed => {
            // A managed model is started on first use and then stays loaded, so
            // the second request does not pay for the load again.
            let (base, model) = match config.provider {
                AssistantProvider::Managed => {
                    let own_file = config.managed_path.clone().unwrap_or_default();
                    let id = config.managed_model.clone().unwrap_or_default();
                    let reasoning = config.reasoning_effort.as_deref();
                    let base = if own_file.trim().is_empty() {
                        state.assistant_runtime.start(&id, 8192, reasoning).await
                    } else {
                        state.assistant_runtime.start_path(std::path::Path::new(own_file.trim()), 8192, reasoning).await
                    }
                    .map_err(|error| api_error(StatusCode::BAD_GATEWAY, error.to_string()))?;
                    (base, if own_file.trim().is_empty() { id } else { own_file })
                }
                _ => (
                    config.local_base_url.clone().unwrap_or_default(),
                    config.local_model.clone().unwrap_or_default(),
                ),
            };
            let sent = reqwest::Client::new()
                .post(format!("{}/chat/completions", base.trim_end_matches('/')))
                .json(&assistant::fit_to_task(assistant::chat_body_constrained(
                    &model,
                    &system,
                    &user,
                    None,
                    None,
                    matches!(config.provider, AssistantProvider::Managed | AssistantProvider::Local)
                        .then(|| assistant::draft_schema(&required)),
                ), request.target))
                .timeout(std::time::Duration::from_secs(180))
                .send()
                .await
                .map_err(|error| {
                    // A sidecar that died mid-request leaves nothing but a
                    // refused connection unless its own log is quoted back.
                    let tail = state.assistant_runtime.log_tail();
                    let detail = if tail.is_empty() { String::new() } else { format!("
{tail}") };
                    api_error(StatusCode::BAD_GATEWAY, format!("the local assistant is unreachable: {error}{detail}"))
                })?;
            let status = sent.status();
            let body = sent.text().await.unwrap_or_default();
            if !status.is_success() {
                return Err(api_error(StatusCode::BAD_GATEWAY, format!("the local assistant returned {status}: {body}")));
            }
            serde_json::from_str(&body)
                .map_err(|error| api_error(StatusCode::BAD_GATEWAY, format!("invalid assistant response: {error}")))?
        }
        AssistantProvider::OpenRouter => {
            let catalog_now = catalog_for(&state).await.ok();
            let model = assistant_openrouter_model(&state, &config).await;
            let catalog = catalog_for(&state)
        .await
        .map_err(|error| api_error(StatusCode::BAD_GATEWAY, error))?;
            catalog
                .selected(Capability::PromptEnhancement, &model)
                .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?;
            let authenticated = providers::openrouter::authenticated_request_for(providers::openrouter::OpenRouterRequest {
                method: providers::openrouter::HttpMethod::Post,
                path: providers::openrouter::CHAT_COMPLETIONS_PATH,
                // Whatever this model publishes for itself; the studio's own
                // temperature is only for models that publish nothing.
                body: {
                    let entry = catalog_now.as_ref().and_then(|catalog| catalog.models.iter().find(|entry| entry.id == model));
                    let effort = entry
                        .and_then(|entry| entry.reasoning.as_ref())
                        .and_then(|reasoning| reasoning.effort_for(config.reasoning_effort.as_deref()));
                    assistant::fit_to_task(
                        assistant::chat_body_full(
                            &model,
                            &system,
                            &user,
                            effort.as_deref(),
                            entry.map(|entry| serde_json::to_value(&entry.defaults).unwrap_or(Value::Null)).as_ref(),
                        ),
                        request.target,
                    )
                },
            })
            .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?;
            execute_openrouter_json(authenticated.request)
                .await
                .map_err(|error| api_error(StatusCode::BAD_GATEWAY, format!("OpenRouter assistant failed: {error}")))?
                .body
        }
        AssistantProvider::None => return Err(api_error(StatusCode::CONFLICT, "No writing assistant is configured.".into())),
    };

    release_assistant_unless_kept(&state).await;
    let content = assistant::content_of(&response).map_err(|error| {
        request_log::unusable("assistant", "", &error.to_string(), &response.to_string());
        api_error(StatusCode::BAD_GATEWAY, error.to_string())
    })?;
    let draft = assistant::parse_draft(&content, required).map_err(|error| {
        // The answer, kept: this is the difference between "invalid JSON" and
        // seeing that the model wrote an apology instead of a song.
        request_log::unusable("assistant", "", &error.to_string(), &content);
        api_error(StatusCode::BAD_GATEWAY, error.to_string())
    })?;
    Ok(Json(serde_json::to_value(draft).unwrap_or(Value::Null)))
}

/// Frees the assistant's five gigabytes as soon as it has answered.
///
/// "Keep models in VRAM between jobs" is off by default, and it means what it
/// says: nothing stays loaded. The assistant was the exception nobody chose -
/// it wrote a draft, kept the card, and the engine then had too little to load its
/// own weights. With the setting on, it stays, because that is what the
/// setting is for. Either way the next request starts it again.
async fn release_assistant_unless_kept(state: &AppState) {
    if state.engine_options.read().await.keep_loaded {
        return;
    }
    if state.assistant_runtime.base_url().await.is_some() {
        state.assistant_runtime.stop().await;
    }
}

/// What was asked of the cloud and what came back, newest last.
///
/// A failed draft used to leave one red line and nothing behind it; this is
/// where the answer itself is kept, so a model that wrote almost the right
/// thing can be told from one that wrote nothing.
async fn openrouter_logs() -> Json<Value> {
    Json(serde_json::json!({ "path": request_log::path().display().to_string(), "lines": request_log::tail(400) }))
}

async fn openrouter_settings() -> Json<Value> {
    let source = credentials::openrouter_source();
    Json(serde_json::json!({
        "configured": source.is_some(),
        "source": source,
        "environment_variable": credentials::OPENROUTER_ENV_VAR,
    }))
}

async fn update_openrouter_settings(
    State(state): State<AppState>,
    Json(request): Json<OpenRouterSettingsRequest>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let source = credentials::store_openrouter_api_key(request.api_key.as_deref())
        .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?;

    // Connecting a key is the moment to learn what it can reach. After this
    // the catalog is served from the cache on disk until the user asks for a
    // refresh, so the studio never goes to the network on its own again.
    let mut refreshed = false;
    if source.is_some() {
        {
            let mut cached = state.openrouter_catalog.write().await;
            cached.catalog = None;
        }
        refreshed = catalog_for(&state).await.is_ok();
    }

    Ok(Json(serde_json::json!({
        "configured": source.is_some(),
        "source": source,
        "environment_variable": credentials::OPENROUTER_ENV_VAR,
        "catalog_refreshed": refreshed,
    })))
}

async fn setup_status(State(state): State<AppState>) -> Json<Value> {
    let target = effective_install_target(&state).await;
    let manager_status = state.model_manager.status(target).await;
    Json(compose_setup_status(&state, manager_status).await)
}

/// Frees the disk a set of components takes.
///
/// The studio downloads ten gigabytes on request; it must be able to give them
/// back on request too, without sending anyone to hunt through a profile folder.
async fn setup_remove(
    State(state): State<AppState>,
    Json(request): Json<SetupDownloadRequest>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let report = state
        .model_manager
        .remove(&request.ids)
        .await
        .map_err(|error| api_error(StatusCode::CONFLICT, error.to_string()))?;
    Ok(Json(serde_json::json!({
        "removed": report.removed,
        "freed_bytes": report.freed_bytes,
    })))
}

/// Takes models the user already has instead of downloading them again.
///
/// Anyone who has run yue2.cpp by hand already has these weights on disk, and
/// they are gigabytes each. This opens a folder picker,
/// looks for the files the catalogue names - by name, then by matching size -
/// and hard-links or copies them into the studio's own model directory.
async fn setup_adopt(State(state): State<AppState>) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let Some(models_root) = studio_data_root().map(|root| root.join("models").join(model_manager::ENGINE_ID)) else {
        return Err(api_error(StatusCode::INTERNAL_SERVER_ERROR, "the studio has no data directory".into()));
    };
    let catalog = state.model_manager.catalog();
    let picked = tokio::task::spawn_blocking(move || {
        rfd::FileDialog::new().set_title("Folder with YuE2 models").pick_folder()
    })
    .await
    .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    let Some(folder) = picked else {
        return Ok(Json(serde_json::json!({ "picked": false, "adopted": [] })));
    };

    let _ = std::fs::create_dir_all(&models_root);
    let mut adopted: Vec<String> = Vec::new();
    let entries: Vec<std::path::PathBuf> = std::fs::read_dir(&folder)
        .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect();

    for component in &catalog.components {
        let target = models_root.join(component.filename);
        if target.is_file() {
            continue;
        }
        // The name first, because that is unambiguous; then the exact size,
        // because other builds rename the same file.
        let source = entries
            .iter()
            .find(|path| path.file_name().is_some_and(|name| name == component.filename))
            .or_else(|| {
                entries.iter().find(|path| {
                    std::fs::metadata(path).map(|meta| meta.len() == component.bytes).unwrap_or(false)
                })
            });
        let Some(source) = source else { continue };
        // A hard link costs nothing and keeps one copy on disk; a folder on
        // another drive cannot have one, so that falls back to a copy.
        if std::fs::hard_link(source, &target).is_err() && std::fs::copy(source, &target).is_err() {
            continue;
        }
        adopted.push(component.id.to_string());
    }

    let target = effective_install_target(&state).await;
    let status = state.model_manager.status(target).await;
    Ok(Json(serde_json::json!({
        "picked": true,
        "folder": folder.display().to_string(),
        "adopted": adopted,
        "status": compose_setup_status(&state, status).await,
    })))
}

/// Opens the studio's own folder in the system file manager.
///
/// Saying where the ten gigabytes are is half an answer; the other half is
/// getting there without retyping a path from a settings screen.
async fn open_data_directory() -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let Some(root) = studio_data_root() else {
        return Err(api_error(StatusCode::NOT_FOUND, "the studio has no data directory".into()));
    };
    let _ = std::fs::create_dir_all(&root);
    #[cfg(windows)]
    let opened = std::process::Command::new("explorer.exe").arg(&root).spawn();
    #[cfg(target_os = "macos")]
    let opened = std::process::Command::new("open").arg(&root).spawn();
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let opened = std::process::Command::new("xdg-open").arg(&root).spawn();
    opened.map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(Json(serde_json::json!({ "opened": root.display().to_string() })))
}

async fn setup_catalog(State(state): State<AppState>) -> Json<model_manager::Catalog> {
    Json(state.model_manager.catalog())
}

async fn setup_download(
    State(state): State<AppState>,
    Json(request): Json<SetupDownloadRequest>,
) -> Result<(StatusCode, Json<model_manager::DownloadJob>), (StatusCode, Json<ApiError>)> {
    let job = state
        .model_manager
        .install(InstallRequest {
            profile_id: request.profile_id,
            component_ids: request.ids,
        })
        .await
        .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?;
    let state_for_completion = state.clone();
    let job_id = job.id.clone();
    tokio::spawn(async move { persist_completed_download_profile(state_for_completion, job_id).await });
    Ok((StatusCode::ACCEPTED, Json(job)))
}

async fn persist_completed_download_profile(state: AppState, job_id: String) {
    loop {
        let Some(job) = state.model_manager.download_job(&job_id).await else { return; };
        match job.status {
            model_manager::DownloadStatus::Completed => {
                if let Some(profile_id) = job.profile_id.or_else(|| model_manager::profile_matching(&job.component_ids).map(str::to_owned)) {
                    *state.selected_profile_id.write().await = Some(profile_id);
                    *state.selected_component_ids.write().await = None;
                } else if state.model_manager.installed_component_files(&job.component_ids).is_ok() {
                    *state.selected_profile_id.write().await = None;
                    *state.selected_component_ids.write().await = Some(job.component_ids);
                }
                let _ = persist_studio_settings(&state).await;
                reload_engine_if_models_changed(&state).await;
                return;
            }
            model_manager::DownloadStatus::Cancelled | model_manager::DownloadStatus::Failed => return,
            model_manager::DownloadStatus::Downloading => tokio::time::sleep(std::time::Duration::from_millis(250)).await,
        }
    }
}

/// Uses a set that is already on disk.
///
/// Downloading was the only way to change which quantisation the studio runs,
/// so a machine with two sets installed was stuck on whichever arrived last.
/// This switches between what is already there, and refuses a set with a
/// missing file rather than failing at generation time.
async fn setup_select(
    State(state): State<AppState>,
    Json(request): Json<SetupSelectRequest>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let matched = request.component_ids.as_deref().and_then(model_manager::profile_matching).map(str::to_owned);
    if let Some(profile_id) = request.profile_id.clone().filter(|value| !value.trim().is_empty()).or(matched) {
        let known = state.model_manager.catalog().profiles.iter().any(|profile| profile.id == profile_id);
        if !known {
            return Err(api_error(StatusCode::BAD_REQUEST, format!("unknown profile {profile_id}")));
        }
        *state.selected_profile_id.write().await = Some(profile_id);
        *state.selected_component_ids.write().await = None;
    } else {
        let ids = request.component_ids.unwrap_or_default();
        if ids.is_empty() {
            return Err(api_error(StatusCode::BAD_REQUEST, "nothing selected".to_string()));
        }
        state
            .model_manager
            .installed_component_files(&ids)
            .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?;
        *state.selected_profile_id.write().await = None;
        *state.selected_component_ids.write().await = Some(ids);
    }
    let _ = persist_studio_settings(&state).await;
    reload_engine_if_models_changed(&state).await;
    let target = effective_install_target(&state).await;
    let manager_status = state.model_manager.status(target).await;
    Ok(Json(serde_json::to_value(compose_setup_status(&state, manager_status).await).unwrap_or(Value::Null)))
}

async fn setup_cancel(State(state): State<AppState>) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let target = effective_install_target(&state).await;
    let manager_status = state
        .model_manager
        .cancel(target)
        .await
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(Json(compose_setup_status(&state, manager_status).await))
}

async fn capabilities(State(state): State<AppState>) -> Json<CapabilitiesResponse> {
    let primary_installed = state.music_server.health().await;
    let parakeet_installed = state.lyrics_sync.parakeet_ready();
    let whisper_installed = state.lyrics_sync.whisper_binary().is_some();
    let assistant = state.assistant.read().await.clone();
    let assistant_installed = assistant.available();
    Json(CapabilitiesResponse {
        engines: capability_engines_with(primary_installed, parakeet_installed, whisper_installed, assistant_installed),
    })
}

fn capability_engines(primary_installed: bool) -> Vec<EngineDescriptor> {
    capability_engines_with(primary_installed, false, false, false)
}

/// The engines the studio can offer, including the local ones it only has when
/// their models are installed. Without these the "Local" button on the provider
/// page was disabled for ever: the studio recognises speech and writes captions
/// locally, but never said so here.
fn capability_engines_with(
    primary_installed: bool,
    parakeet_installed: bool,
    whisper_installed: bool,
    assistant_installed: bool,
) -> Vec<EngineDescriptor> {
    let openrouter_capabilities = vec![
        Capability::SpeechToText,
        Capability::PromptEnhancement,
        Capability::CoverArt,
    ];
    vec![
        EngineDescriptor {
            id: PRIMARY_MUSIC_ENGINE_ID.into(),
            display_name: "YuE2 (yue2.cpp)".into(),
            capabilities: vec![Capability::MusicGeneration],
            execution_mode: ExecutionMode::Local,
            installed: primary_installed,
        },
        // Two different recognisers, named. "Parakeet / Whisper" was not a
        // choice, it was a shrug.
        EngineDescriptor {
            id: "parakeet".into(),
            display_name: "Parakeet TDT 0.6B (local)".into(),
            capabilities: vec![Capability::SpeechToText],
            execution_mode: ExecutionMode::Local,
            installed: parakeet_installed,
        },
        EngineDescriptor {
            id: "whisper".into(),
            display_name: "Whisper.cpp (local)".into(),
            capabilities: vec![Capability::SpeechToText],
            execution_mode: ExecutionMode::Local,
            installed: whisper_installed,
        },
        EngineDescriptor {
            id: "local-assistant".into(),
            display_name: "Local GGUF model".into(),
            capabilities: vec![Capability::PromptEnhancement],
            execution_mode: ExecutionMode::Local,
            installed: assistant_installed,
        },
        EngineDescriptor {
            id: "openrouter".into(),
            display_name: "OpenRouter".into(),
            capabilities: openrouter_capabilities,
            execution_mode: ExecutionMode::OpenRouter,
            installed: false,
        },
    ]
}

/// Gets the writing assistant off the graphics card before the engine needs it.
///
/// There is one card: Gemma holds five gigabytes from the moment it writes
/// a draft, and YuE2 needs its own few on top. The assistant starts itself on the next request it
/// receives, so stopping it here costs a reload later and nothing else.
async fn free_the_card_for_the_engine(state: &AppState) {
    if state.assistant_runtime.base_url().await.is_some() {
        state.assistant_runtime.stop().await;
    }
}

/// What the engine's own log says about why it is not there any more.
///
/// A card that ran out of memory says so in the log and then the process is
/// gone; the studio saw only a refused connection, and told the user to
/// download models that were already on disk.
fn engine_failure_reason() -> Option<String> {
    let tail = music_engine::yue_server::startup_log_tail(80).join("\n").to_lowercase();
    describes_exhausted_memory(&tail)
        .then(|| "The graphics card ran out of memory while the engine was loading the models. Choose a smaller quantisation in the model manager, or close whatever else is using the card - the writing assistant holds several gigabytes of its own.".to_string())
}

/// Whether a lowercased log says the card ran out of room.
fn describes_exhausted_memory(log: &str) -> bool {
    [
        "out of memory",
        "cudamalloc",
        "failed to allocate",
        "insufficient memory",
        "cudaerrormemoryallocation",
        "bad_alloc",
    ]
    .iter()
    .any(|marker| log.contains(marker))
}

async fn create_music_job(
    State(state): State<AppState>,
    Json(request): Json<CreateMusicJobRequest>,
) -> (StatusCode, Json<MusicJob>) {
    let engine_id = selected_local_music_engine(&*state.configuration.read().await)
        .unwrap_or_else(|| "unconfigured".into());
    if engine_id != PRIMARY_MUSIC_ENGINE_ID {
        let job = queued_not_configured_job(request, engine_id);
        state.jobs.write().await.insert(job.id.clone(), job.clone());
        return (StatusCode::ACCEPTED, Json(job));
    }

    if state.training.active_run().await.is_some() {
        let error = "a training run has the card; songs can be made once it finishes or is stopped".to_string();
        return (StatusCode::CONFLICT, Json(failed_request_job(request, engine_id, error)));
    }
    if let Some(missing) = request.adapters.iter().find(|adapter| !state.adapters.exists(&adapter.id)).map(|adapter| adapter.id.clone()) {
        let error = format!("adapter {missing} is not installed; add it again on the LoRA page");
        return (StatusCode::BAD_REQUEST, Json(failed_request_job(request, engine_id, error)));
    }
    let max_batch = state.engine_options.read().await.effective_max_batch();
    let body = match yue_request_from(&request, max_batch) {
        Ok(value) => value,
        Err(error) => return (StatusCode::BAD_REQUEST, Json(failed_request_job(request, engine_id, error))),
    };
    match state.music_server.submit(engine_submission(&body)).await {
        Ok(remote) => {
            let job = MusicJob {
                id: remote.id,
                engine_id,
                cover_prompt: request.cover_prompt.clone(),
                title: Some(titled(&request)),
                status: MusicJobStatus::Queued,
                dispatch: MusicJobDispatch::Local,
                phase: MusicJobPhase::Queued,
                style: request.style,
                lyrics: request.lyrics,
                duration_seconds: request.duration_seconds.unwrap_or_default(),
                generation_settings: body,
                song: None,
                songs: vec![],
                message: "Submitted to yue-server.".into(),
            };
            state.jobs.write().await.insert(job.id.clone(), job.clone());
            spawn_job_watcher(state.clone(), job.id.clone());
            (StatusCode::ACCEPTED, Json(job))
        }
        Err(error) => {
            let job = failed_request_job(request, engine_id, format!("the engine refused the job: {error}"));
            state.jobs.write().await.insert(job.id.clone(), job.clone());
            (StatusCode::SERVICE_UNAVAILABLE, Json(job))
        }
    }
}

/// Re-renders a track from its semantic stream: the prefix and the codes
/// prefill in one forward, so only the flow-matching side (steps, noise seed,
/// variations, output encoding) can change while the music stays the same.
async fn replay_music_job(
    State(state): State<AppState>,
    Json(request): Json<ReplayMusicJobRequest>,
) -> Result<(StatusCode, Json<MusicJob>), (StatusCode, Json<ApiError>)> {
    if selected_local_music_engine(&*state.configuration.read().await).as_deref() != Some(PRIMARY_MUSIC_ENGINE_ID) {
        return Err(api_error(StatusCode::CONFLICT, "Re-rendering requires the local YuE2 engine.".into()));
    }
    let mut source_title = None;
    let replay = match (&request.song_id, &request.replay_request) {
        (Some(_), Some(_)) => return Err(api_error(StatusCode::BAD_REQUEST, "Provide either song_id or replay_request, not both.".into())),
        (Some(song_id), None) => {
            let song = state.library.get_song(song_id).map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
                .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Song not found.".into()))?;
            source_title = Some(song.title.clone());
            song.replay_request.ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "This track has no replay request: it was not generated by YuE2.".into()))?
        }
        (None, Some(replay)) => replay.clone(),
        (None, None) => return Err(api_error(StatusCode::BAD_REQUEST, "Provide song_id or replay_request.".into())),
    };
    if state.training.active_run().await.is_some() {
        return Err(api_error(StatusCode::CONFLICT, "a training run has the card; re-render once it finishes or is stopped".into()));
    }
    let body = prepare_replay_synthesis(replay, &request).map_err(|error| api_error(StatusCode::BAD_REQUEST, error))?;
    let style = body.get("style").and_then(Value::as_str).unwrap_or_default().to_owned();
    let lyrics = body.get("lyrics").and_then(Value::as_str).unwrap_or_default().to_owned();
    let remote = state
        .music_server
        .submit(engine_submission(&body))
        .await
        .map_err(|error| api_error(StatusCode::SERVICE_UNAVAILABLE, format!("the engine refused the job: {error}")))?;
    let title = request.title.clone().filter(|value| !value.trim().is_empty()).or(source_title);
    let job = MusicJob {
        cover_prompt: None,
        id: remote.id,
        engine_id: PRIMARY_MUSIC_ENGINE_ID.into(),
        title,
        status: MusicJobStatus::Queued,
        dispatch: MusicJobDispatch::Local,
        phase: MusicJobPhase::Queued,
        style,
        lyrics,
        duration_seconds: body.get("duration").and_then(Value::as_f64).unwrap_or_default(),
        generation_settings: body,
        song: None,
        songs: vec![],
        message: "Submitted a re-render: the semantic stream is present, so the autoregressive stage is skipped.".into(),
    };
    state.jobs.write().await.insert(job.id.clone(), job.clone());
    spawn_job_watcher(state.clone(), job.id.clone());
    Ok((StatusCode::ACCEPTED, Json(job)))
}

fn prepare_replay_synthesis(mut replay: Value, overrides: &ReplayMusicJobRequest) -> Result<Value, String> {
    let object = replay.as_object_mut().ok_or("replay_request must be a JSON object")?;
    let tokens = object.get("semantic_tokens").and_then(Value::as_str).unwrap_or_default();
    validate_semantic_tokens(tokens)?;
    if tokens.trim().is_empty() {
        return Err("replay_request has no semantic_tokens; it cannot skip the autoregressive stage".into());
    }
    if let Some(steps) = overrides.steps {
        if steps < 1 { return Err("steps must be at least 1".into()); }
        object.insert("steps".into(), Value::from(steps));
    }
    if let Some(seed) = overrides.seed { object.insert("seed".into(), Value::from(seed)); }
    if let Some(variations) = overrides.synth_batch_size {
        if !(1..=9).contains(&variations) { return Err("synth_batch_size must be between 1 and 9".into()); }
        object.insert("synth_batch_size".into(), Value::from(variations));
    }
    if let Some(format) = &overrides.output_format {
        validate_output_format(format)?;
        object.insert("output_format".into(), Value::String(format.clone()));
    }
    if let Some(peak_clip) = overrides.peak_clip {
        if peak_clip < 0 { return Err("peak_clip cannot be negative".into()); }
        object.insert("peak_clip".into(), Value::from(peak_clip));
    }
    if let Some(bitrate) = overrides.mp3_bitrate {
        validate_mp3_bitrate(bitrate)?;
        object.insert("mp3_bitrate".into(), Value::from(bitrate));
    }
    // A replay is one song by construction; the engine ignores the counter
    // but the stored provenance should not claim a batch.
    object.insert("lm_batch_size".into(), Value::from(1));
    Ok(replay)
}

/// Covers and karaoke timings follow every finished track, local or cloud.
fn after_import(state: &AppState, song_id: &str) {
    let (cover_state, cover_song) = (state.clone(), song_id.to_owned());
    let (timing_state, timing_song) = (state.clone(), song_id.to_owned());
    tokio::spawn(async move { draw_cover_for(cover_state, cover_song).await });
    tokio::spawn(async move { time_lyrics_for(timing_state, timing_song).await });
}

/// The jobs still in flight, oldest first, so a reloaded window can show them.
async fn list_active_music_jobs(State(state): State<AppState>) -> Json<Vec<MusicJob>> {
    let mut active: Vec<MusicJob> = state
        .jobs
        .read()
        .await
        .values()
        .filter(|job| matches!(job.status, MusicJobStatus::Queued | MusicJobStatus::Running))
        .cloned()
        .collect();
    active.sort_by(|a, b| a.id.cmp(&b.id));
    Json(active)
}

async fn music_job_status(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> Result<Json<MusicJob>, (StatusCode, Json<ApiError>)> {
    state
        .jobs
        .read()
        .await
        .get(&job_id)
        .cloned()
        .map(Json)
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Music job was not found.".into()))
}

/// Follows one engine job to its end and imports what it made.
///
/// The service owns this, not the window: a track finished while the
/// interface was reloading, closed or on another page still lands in the
/// library, and only one task ever imports a result.
fn spawn_job_watcher(state: AppState, job_id: String) {
    tokio::spawn(async move {
        let mut unreachable = 0u32;
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
            let Some(existing) = state.jobs.read().await.get(&job_id).cloned() else { return };
            if matches!(existing.status, MusicJobStatus::Completed | MusicJobStatus::Failed | MusicJobStatus::Cancelled) {
                return;
            }
            let remote = match state.music_server.job(&job_id).await {
                Ok(remote) => {
                    unreachable = 0;
                    remote
                }
                Err(error) => {
                    // The engine restarting drops its job table; a few missed
                    // polls are a restart, a minute of them is a lost job.
                    unreachable += 1;
                    if unreachable >= 60 {
                        if let Some(job) = state.jobs.write().await.get_mut(&job_id) {
                            job.status = MusicJobStatus::Failed;
                            job.phase = MusicJobPhase::Failed;
                            job.message = format!("The engine stopped answering about this job: {error}");
                        }
                        return;
                    }
                    continue;
                }
            };
            if remote.status == "done" {
                let imported = import_completed_result(&state, &existing, &job_id).await;
                let mut jobs = state.jobs.write().await;
                let Some(job) = jobs.get_mut(&job_id) else { return };
                match imported {
                    Ok(songs) => {
                        job.status = MusicJobStatus::Completed;
                        job.phase = MusicJobPhase::Completed;
                        job.song = songs.first().cloned();
                        job.songs = songs;
                        job.message = "The engine finished this job and its tracks were imported into the library.".into();
                    }
                    Err(error) => {
                        job.status = MusicJobStatus::Failed;
                        job.phase = MusicJobPhase::Failed;
                        job.message = format!("The engine finished the job, but the studio could not import its result: {error}");
                    }
                }
                return;
            }
            if let Some(job) = state.jobs.write().await.get_mut(&job_id) {
                apply_remote_status(job, &remote.status);
            }
        }
    });
}

async fn import_completed_result(state: &AppState, job: &MusicJob, job_id: &str) -> anyhow::Result<Vec<CompletedSong>> {
    let result = state.music_server.result(job_id).await?;
    let tracks = engine_result::parse_multipart_result(&result.content_type, &result.body)?;
    let profile_id = state.selected_profile_id.read().await.clone();
    // The engine's defaults fill whatever the request left out, so a stored
    // track records every value it was made with, not only the ones typed.
    let engine_defaults = state.music_server.props().await.ok().and_then(|props| props.get("defaults").cloned());
    let count = tracks.len();
    let mut imported = Vec::with_capacity(count);
    for (index, track) in tracks.into_iter().enumerate() {
        let mut replay = track.replay_request;
        let style = replay.get("style").and_then(Value::as_str).unwrap_or_default().to_owned();
        let lyrics = replay.get("lyrics").and_then(Value::as_str).unwrap_or_default().to_owned();
        let semantic_tokens = replay
            .get("semantic_tokens")
            .filter(|value| value.as_str().is_some_and(|value| !value.is_empty()))
            .context("the engine returned a track without its semantic stream")?
            .clone();
        // The replay request is sparse: fields at their default are omitted.
        // Start from what was submitted and let the per-track values - the
        // seeds it consumed, the score it wrote - win over it.
        let mut generation_settings = job.generation_settings.clone();
        match (generation_settings.as_object_mut(), replay.as_object()) {
            (Some(target), Some(source)) => {
                for (key, value) in source {
                    target.insert(key.clone(), value.clone());
                }
            }
            _ => generation_settings = replay.clone(),
        }
        let settings = generation_settings.as_object_mut().context("generation settings are not a JSON object")?;
        if let Some(Value::Object(defaults)) = &engine_defaults {
            for (key, value) in defaults {
                if !settings.contains_key(key) && !matches!(key.as_str(), "semantic_tokens" | "lm_seed" | "seed") {
                    settings.insert(key.clone(), value.clone());
                }
            }
        }
        settings.remove("semantic_tokens");
        settings.insert("lm_batch_size".into(), Value::from(1));
        settings.insert("synth_batch_size".into(), Value::from(1));
        let mut extension = engine_result::audio_extension(&track.audio_content_type)?;
        let mut audio = track.audio;
        if studio_encodes_mp3(&job.generation_settings) && extension == "wav" {
            let kbps = job.generation_settings.get("mp3_bitrate").and_then(Value::as_u64).map_or(DEFAULT_MP3_KBPS, |value| value as u32);
            let peak_clip = job.generation_settings.get("peak_clip").and_then(Value::as_u64).map_or(DEFAULT_PEAK_CLIP, |value| value as u32);
            audio = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<u8>> {
                let mut stereo = audio_pcm::decode_stereo_bytes(audio, "wav")?;
                audio_post::encode::normalize_peak(&mut stereo, peak_clip);
                audio_post::encode::mp3(&stereo, kbps)
            })
            .await
            .context("the MP3 encoder stopped")??;
            extension = "mp3";
            // the track says what it is: an MP3 at the rate LAME wrote
            let written = Value::from(audio_post::encode::mp3_bitrate(kbps));
            for record in [&mut *settings, replay.as_object_mut().context("the replay request is not a JSON object")?] {
                record.insert("output_format".into(), Value::from("mp3"));
                record.insert("mp3_bitrate".into(), written.clone());
            }
        }
        let metadata = serde_json::json!({
            "duration_seconds": library::audio_duration_seconds(
                &audio,
                extension,
                replay.get("mp3_bitrate").and_then(Value::as_u64).map(|value| value as u32),
            ),
            "seed": replay.get("seed"),
            "lm_seed": replay.get("lm_seed"),
            "cot": generation_settings.get("cot"),
            "output_format": generation_settings.get("output_format"),
            "cover_prompt": job.cover_prompt.clone(),
        });
        // Several tracks from one request share its name; number them so the
        // library can tell the takes apart.
        let title = match (&job.title, count) {
            (Some(title), count) if count > 1 => Some(format!("{title} ({})", index + 1)),
            (title, _) => title.clone(),
        };
        let imported_song = state.library.import_generated_song(library::GeneratedSongInput {
            title,
            metadata,
            caption: style,
            lyrics,
            generation_settings,
            replay_request: Some(replay),
            audio_codes: Some(semantic_tokens),
            engine_id: job.engine_id.clone(),
            profile_id: profile_id.clone(),
            source: "local_generation".into(),
            audio_extension: extension,
            audio,
        })?;
        let audio_url = format!("/v1/library/media/{}", imported_song.song.id);
        tag_stored_song(state, &imported_song.song.id).await;
        after_import(state, &imported_song.song.id);
        imported.push(CompletedSong { id: imported_song.song.id.clone(), song: imported_song.song, audio_url });
    }
    Ok(imported)
}

async fn cancel_music_job(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> Result<Json<MusicJob>, (StatusCode, Json<ApiError>)> {
    let existing = state
        .jobs
        .read()
        .await
        .get(&job_id)
        .cloned()
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Music job was not found.".into()))?;
    if existing.engine_id != PRIMARY_MUSIC_ENGINE_ID {
        return Err(api_error(
            StatusCode::NOT_IMPLEMENTED,
            format!("The selected engine '{}' has no cancel adapter.", existing.engine_id),
        ));
    }
    let remote = state.music_server.cancel(&job_id).await.map_err(|error| {
        api_error(StatusCode::SERVICE_UNAVAILABLE, format!("the engine did not accept the cancel: {error}"))
    })?;
    let mut jobs = state.jobs.write().await;
    let job = jobs
        .get_mut(&job_id)
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Music job was not found.".into()))?;
    apply_remote_status(job, &remote.status);
    Ok(Json(job.clone()))
}

/// The engine's own defaults, version and the weights it serves: the source
/// of truth for every placeholder in the request form.
async fn local_music_model_catalog(
    State(state): State<AppState>,
) -> Result<Json<LocalMusicModelCatalog>, (StatusCode, Json<ApiError>)> {
    let engine_id = selected_local_music_engine(&*state.configuration.read().await).ok_or_else(|| {
        api_error(StatusCode::SERVICE_UNAVAILABLE, "No local music engine is selected in the capability configuration.".into())
    })?;
    if engine_id != PRIMARY_MUSIC_ENGINE_ID {
        return Err(api_error(StatusCode::NOT_IMPLEMENTED, format!("The selected engine '{engine_id}' has no catalog adapter.")));
    }
    let mut catalog = state.music_server.props().await.map_err(|error| {
        api_error(StatusCode::SERVICE_UNAVAILABLE, format!("the engine catalog is unavailable: {error}"))
    })?;
    let transcriber = {
        let supervisor = state.engine.lock().await;
        supervisor.as_ref().and_then(|engine| engine.config().models.transcriber.clone())
    };
    if let Value::Object(fields) = &mut catalog {
        fields.insert("transcriber".into(), transcriber.map(|path| Value::String(path.display().to_string())).unwrap_or(Value::Null));
        fields.insert("max_batch".into(), Value::from(state.engine_options.read().await.effective_max_batch()));
    }
    Ok(Json(LocalMusicModelCatalog { engine_id, catalog }))
}

/// A job whose answer is a score: a transcription of a recording, or a
/// composition from a style and lyrics.
#[derive(Debug, Serialize)]
struct ScoreJob {
    id: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    abc: Option<String>,
    /// The token seed a composition drew its score with.
    #[serde(skip_serializing_if = "Option::is_none")]
    lm_seed: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl ScoreJob {
    fn running(id: String) -> Self {
        Self { id, status: "running".into(), abc: None, lm_seed: None, error: None }
    }
}

#[derive(Debug, Deserialize)]
struct ComposeScoreRequest {
    #[serde(default)]
    style: String,
    #[serde(default)]
    lyrics: String,
    #[serde(default)]
    cot: Option<String>,
    #[serde(default)]
    lm_seed: Option<i64>,
    #[serde(default)]
    abc_sampling: Option<SamplingPreset>,
}

/// The engine request that runs the planning stage and next to nothing else.
/// The score is written from the prompt alone - the duration never reaches the
/// prompt, it only caps the semantic stage - so a one second budget, one solver
/// step and one track return the same score a full song would have been sung
/// from, the equivalent of yue2.cpp's `yue-plan`.
fn compose_request_from(request: &ComposeScoreRequest) -> Result<Value, String> {
    if request.style.trim().is_empty() && request.lyrics.trim().is_empty() {
        return Err("write a style or lyrics: the engine needs at least one of them".into());
    }
    let cot = request.cot.as_deref().unwrap_or("full");
    if !matches!(cot, "full" | "melody") {
        return Err("a score is composed in full or melody mode".into());
    }
    let mut body = serde_json::json!({
        "style": request.style,
        "lyrics": request.lyrics.replace("\r\n", "\n"),
        "cot": cot,
        "duration": 1.0,
        "steps": 1,
        "lm_batch_size": 1,
        "synth_batch_size": 1,
        "output_format": "wav16",
    });
    if let Some(seed) = request.lm_seed.filter(|seed| *seed >= 0) {
        body["lm_seed"] = Value::from(seed);
    }
    if let Some(sampling) = &request.abc_sampling {
        sampling.validate("abc_sampling")?;
        body["abc_sampling"] = serde_json::to_value(sampling).map_err(|error| error.to_string())?;
    }
    Ok(body)
}

async fn compose_score(
    State(state): State<AppState>,
    Json(request): Json<ComposeScoreRequest>,
) -> Result<(StatusCode, Json<ScoreJob>), (StatusCode, Json<ApiError>)> {
    let body = compose_request_from(&request).map_err(|error| api_error(StatusCode::BAD_REQUEST, error))?;
    let remote = state
        .music_server
        .submit(body)
        .await
        .map_err(|error| api_error(StatusCode::SERVICE_UNAVAILABLE, format!("the engine refused the composition: {error}")))?;
    Ok((StatusCode::ACCEPTED, Json(ScoreJob::running(remote.id))))
}

/// Reads a recording into the ABC score a cover takes as its `abc`. The audio
/// is either uploaded (`audio` part) or a library track (`song_id` field);
/// `melody_only` drops the chord symbols, which is what the `melody` mode wants.
async fn create_transcription(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<ScoreJob>), (StatusCode, Json<ApiError>)> {
    let transcriber_loaded = {
        let supervisor = state.engine.lock().await;
        supervisor.as_ref().map(|engine| engine.config().models.transcriber.is_some())
    };
    if transcriber_loaded == Some(false) {
        return Err(api_error(
            StatusCode::CONFLICT,
            "The running engine has no SheetSage2 transcriber. Add one to the model set in Settings - Models.".into(),
        ));
    }
    let mut audio: Option<(Vec<u8>, String)> = None;
    let mut melody_only = false;
    while let Some(field) = multipart.next_field().await.map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))? {
        match field.name().unwrap_or_default() {
            "audio" => {
                let name = field.file_name().unwrap_or("input.audio").to_owned();
                let bytes = field.bytes().await.map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?;
                audio = Some((bytes.to_vec(), name));
            }
            "song_id" => {
                let song_id = field.text().await.map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?;
                let song = state.library.get_song(song_id.trim()).map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
                    .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Song not found.".into()))?;
                let path = state.library.media_path_for_song(&song).ok_or_else(|| api_error(StatusCode::NOT_FOUND, "The track's audio is not in the library.".into()))?;
                let bytes = tokio::fs::read(&path).await.map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("read {}: {error}", path.display())))?;
                let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| "input.audio".into());
                audio = Some((bytes, name));
            }
            "melody_only" => {
                let value = field.text().await.unwrap_or_default();
                melody_only = matches!(value.trim(), "1" | "true" | "yes");
            }
            _ => {}
        }
    }
    let (bytes, name) = audio.ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "Send an audio part or a song_id.".into()))?;
    if bytes.is_empty() {
        return Err(api_error(StatusCode::BAD_REQUEST, "The audio is empty.".into()));
    }
    let remote = state
        .music_server
        .transcribe(bytes, name, melody_only)
        .await
        .map_err(|error| api_error(StatusCode::SERVICE_UNAVAILABLE, format!("the engine refused the transcription: {error}")))?;
    Ok((StatusCode::ACCEPTED, Json(ScoreJob::running(remote.id))))
}

/// The score a finished job carries. A transcription answers with JSON; a
/// composition with the engine's multipart result, whose replay request holds
/// the score it wrote and the seed it drew.
fn score_from_result(content_type: &str, body: &[u8]) -> Result<(String, Option<i64>), String> {
    let (abc, lm_seed) = if content_type.starts_with("multipart/") {
        let tracks = engine_result::parse_multipart_result(content_type, body).map_err(|error| error.to_string())?;
        let replay = tracks.into_iter().next().ok_or("the composition returned no track")?.replay_request;
        (replay.get("abc").and_then(Value::as_str).map(str::to_owned), replay.get("lm_seed").and_then(Value::as_i64))
    } else {
        let value: Value = serde_json::from_slice(body).map_err(|error| format!("the result is not JSON: {error}"))?;
        (value.get("abc").and_then(Value::as_str).map(str::to_owned), None)
    };
    let abc = abc.filter(|value| !value.trim().is_empty()).ok_or("the engine returned no score")?;
    Ok((abc.trim_end().to_owned(), lm_seed))
}

async fn score_job_status(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> Result<Json<ScoreJob>, (StatusCode, Json<ApiError>)> {
    let remote = state.music_server.job(&job_id).await.map_err(|error| api_error(StatusCode::SERVICE_UNAVAILABLE, error.to_string()))?;
    let mut job = ScoreJob { status: remote.status.clone(), ..ScoreJob::running(job_id.clone()) };
    match remote.status.as_str() {
        "done" => {
            let result = state.music_server.result(&job_id).await.map_err(|error| api_error(StatusCode::SERVICE_UNAVAILABLE, error.to_string()))?;
            let (abc, lm_seed) = score_from_result(&result.content_type, &result.body).map_err(|error| api_error(StatusCode::BAD_GATEWAY, error))?;
            job.abc = Some(abc);
            job.lm_seed = lm_seed;
        }
        "failed" => job.error = Some(engine_failure_reason().unwrap_or_else(|| "The engine could not write this score.".into())),
        _ => {}
    }
    Ok(Json(job))
}

async fn cancel_score_job(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> Result<Json<ScoreJob>, (StatusCode, Json<ApiError>)> {
    let remote = state.music_server.cancel(&job_id).await.map_err(|error| api_error(StatusCode::SERVICE_UNAVAILABLE, error.to_string()))?;
    Ok(Json(ScoreJob { status: remote.status, ..ScoreJob::running(job_id) }))
}

impl EngineClient {
    fn from_environment() -> Self {
        let base_url = env::var("YUE_ENGINE_BASE_URL")
            .unwrap_or_else(|_| format!("http://127.0.0.1:{}", music_engine::yue_server::DEFAULT_PORT))
            .trim_end_matches('/')
            .to_owned();
        Self { base_url, http: reqwest::Client::new(), health_cache: Arc::new(std::sync::Mutex::new(None)) }
    }

    async fn health(&self) -> bool {
        const FRESH: std::time::Duration = std::time::Duration::from_millis(1500);
        if let Some((at, up)) = *self.health_cache.lock().expect("health cache") {
            if at.elapsed() < FRESH {
                return up;
            }
        }
        let up = self
            .http
            .get(self.url("/health"))
            .timeout(std::time::Duration::from_millis(500))
            .send()
            .await
            .map(|response| response.status().is_success())
            .unwrap_or(false);
        *self.health_cache.lock().expect("health cache") = Some((std::time::Instant::now(), up));
        up
    }

    async fn props(&self) -> anyhow::Result<Value> {
        self.json_response(self.http.get(self.url("/props")).send().await?).await
    }

    /// Upstream `GET /logs` is an endless SSE stream that replays its ring and
    /// then waits; the ring is read until the stream goes quiet.
    async fn logs_snapshot(&self, quiet_period: std::time::Duration) -> anyhow::Result<Vec<String>> {
        let response = self.http.get(self.url("/logs")).send().await?;
        let status = response.status();
        if !status.is_success() {
            anyhow::bail!("yue-server returned {status} for /logs");
        }
        let mut stream = response.bytes_stream();
        let mut buffer = Vec::new();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(quiet_period.min(remaining), stream.next()).await {
                Ok(Some(chunk)) => buffer.extend_from_slice(&chunk?),
                Ok(None) | Err(_) => break,
            }
        }
        Ok(String::from_utf8_lossy(&buffer)
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(|line| line.trim().to_owned())
            .filter(|line| !line.is_empty())
            .collect())
    }

    async fn submit(&self, request: Value) -> anyhow::Result<EngineSubmitResponse> {
        self.json_response(self.http.post(self.url("/synth")).json(&request).send().await?).await
    }

    async fn transcribe(&self, audio: Vec<u8>, filename: String, melody_only: bool) -> anyhow::Result<EngineSubmitResponse> {
        let mut form = reqwest::multipart::Form::new().part("audio", reqwest::multipart::Part::bytes(audio).file_name(filename));
        if melody_only {
            form = form.text("melody_only", "1");
        }
        self.json_response(self.http.post(self.url("/transcribe")).multipart(form).send().await?).await
    }

    async fn job(&self, job_id: &str) -> anyhow::Result<EngineJobResponse> {
        self.json_response(self.http.get(self.url("/job")).query(&[("id", job_id)]).send().await?).await
    }

    async fn cancel(&self, job_id: &str) -> anyhow::Result<EngineJobResponse> {
        self.json_response(self.http.post(self.url("/job")).query(&[("id", job_id), ("cancel", "1")]).send().await?).await
    }

    async fn result(&self, job_id: &str) -> anyhow::Result<EngineResultResponse> {
        let response = self.http.get(self.url("/job")).query(&[("id", job_id), ("result", "1")]).send().await?;
        let status = response.status();
        if !status.is_success() {
            anyhow::bail!("yue-server returned {status}: {}", response.text().await?);
        }
        let content_type = response.headers().get(reqwest::header::CONTENT_TYPE).and_then(|value| value.to_str().ok()).unwrap_or("").to_owned();
        Ok(EngineResultResponse { content_type, body: response.bytes().await?.to_vec() })
    }

    async fn json_response<T: serde::de::DeserializeOwned>(&self, response: reqwest::Response) -> anyhow::Result<T> {
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            let message = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|value| value.get("error").and_then(Value::as_str).map(str::to_owned))
                .unwrap_or(body);
            anyhow::bail!("yue-server returned {status}: {message}");
        }
        Ok(serde_json::from_str(&body)?)
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }
}

fn initial_configuration() -> StudioConfiguration {
    let mut configuration = StudioConfiguration::default();
    if let Some(selection) = configuration
        .selections
        .iter_mut()
        .find(|selection| selection.capability == Capability::MusicGeneration)
    {
        selection.mode = ExecutionMode::Local;
        selection.local_engine = Some(PRIMARY_MUSIC_ENGINE_ID.into());
        selection.cloud_model = None;
    }
    configuration
}

/// Settings may name local engines this build does not ship; any local engine
/// not declared by `capability_engines` is dropped so the interface never
/// offers a provider nobody can serve.
fn sanitize_persisted_configuration(mut configuration: StudioConfiguration) -> StudioConfiguration {
    let declared = capability_engines(false);
    for selection in &mut configuration.selections {
        let engine_serves_capability = selection.local_engine.as_deref().is_some_and(|engine_id| {
            declared.iter().any(|engine| {
                engine.id == engine_id
                    && engine.execution_mode == ExecutionMode::Local
                    && engine.capabilities.contains(&selection.capability)
            })
        });
        if engine_serves_capability {
            continue;
        }
        selection.local_engine = None;
        if selection.mode == ExecutionMode::Local {
            selection.mode = ExecutionMode::OpenRouter;
        }
    }
    configuration
}

fn selected_local_music_engine(configuration: &StudioConfiguration) -> Option<String> {
    configuration
        .selections
        .iter()
        .find(|selection| selection.capability == Capability::MusicGeneration && selection.mode == ExecutionMode::Local)
        .and_then(|selection| selection.local_engine.clone())
}

fn validate_output_format(format: &str) -> Result<(), String> {
    if matches!(format, "mp3" | "wav16" | "wav24" | "wav32") {
        Ok(())
    } else {
        Err("output_format must be one of: mp3, wav16, wav24, wav32".into())
    }
}

fn validate_mp3_bitrate(bitrate: u32) -> Result<(), String> {
    if (32..=320).contains(&bitrate) {
        Ok(())
    } else {
        Err("mp3_bitrate must be between 32 and 320 kbps".into())
    }
}

fn validate_semantic_tokens(tokens: &str) -> Result<(), String> {
    let invalid = tokens
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .find(|value| value.parse::<u32>().map(|code| code >= 32768).unwrap_or(true));
    match invalid {
        Some(value) => Err(format!("semantic_tokens must be comma-separated codes below 32768; found `{value}`")),
        None => Ok(()),
    }
}

/// Builds the yue-server request. Only what the user set travels: an absent
/// field is the engine's protocol default, and the replay request the engine
/// returns records the values it actually used.
/// The bitrate a track is encoded at when the request names none.
const DEFAULT_MP3_KBPS: u32 = 320;
/// Samples per million allowed to clip when the level is set, as the engines do.
const DEFAULT_PEAK_CLIP: u32 = 10;

/// Whether the studio makes this track's MP3 itself; the engine's own default
/// output is MP3, so a request naming no format counts.
fn studio_encodes_mp3(settings: &Value) -> bool {
    settings.get("output_format").and_then(Value::as_str).is_none_or(|format| format == "mp3")
}

/// What the engine is asked for. An MP3 is made by the studio with LAME from
/// the engine's unencoded 32-bit float output - the model's own rate and
/// precision - so no track is ever encoded twice or by an engine's own encoder.
fn engine_submission(body: &Value) -> Value {
    let mut engine = body.clone();
    if studio_encodes_mp3(body) {
        if let Some(fields) = engine.as_object_mut() {
            fields.insert("output_format".into(), Value::from("wav32"));
            fields.remove("mp3_bitrate");
        }
    }
    engine
}

fn yue_request_from(request: &CreateMusicJobRequest, max_batch: u32) -> Result<Value, String> {
    let semantic_tokens = request.semantic_tokens.as_deref().map(str::trim).filter(|value| !value.is_empty());
    if request.style.trim().is_empty() && request.lyrics.trim().is_empty() && semantic_tokens.is_none() {
        return Err("write a style or lyrics: the engine needs at least one of them".into());
    }
    if let Some(cot) = request.cot.as_deref() {
        if !matches!(cot, "full" | "melody" | "off") {
            return Err("cot must be full, melody or off".into());
        }
    }
    if let Some(duration) = request.duration_seconds {
        if !duration.is_finite() || !(1.0..=360.0).contains(&duration) {
            return Err("duration_seconds must be between 1 and 360".into());
        }
    }
    if request.steps.is_some_and(|steps| !(1..=200).contains(&steps)) {
        return Err("steps must be between 1 and 200".into());
    }
    if request.lm_batch_size.is_some_and(|size| size < 1 || size > max_batch) {
        return Err(format!("lm_batch_size must be between 1 and {max_batch}; raise the song limit in Settings - Engine for more"));
    }
    if request.synth_batch_size.is_some_and(|size| !(1..=9).contains(&size)) {
        return Err("synth_batch_size must be between 1 and 9".into());
    }
    if request.cfg_scale.is_some_and(|value| !value.is_finite() || value > 10.0) {
        return Err("cfg_scale must be a finite number up to 10".into());
    }
    if request.peak_clip.is_some_and(|value| value < 0) {
        return Err("peak_clip cannot be negative".into());
    }
    if let Some(format) = request.output_format.as_deref() {
        validate_output_format(format)?;
    }
    if let Some(bitrate) = request.mp3_bitrate {
        validate_mp3_bitrate(bitrate)?;
    }
    if let Some(tokens) = semantic_tokens {
        validate_semantic_tokens(tokens)?;
    }
    let mut body = serde_json::json!({
        "style": request.style,
        "lyrics": request.lyrics.replace("\r\n", "\n"),
    });
    if let Some(abc) = request.abc.as_deref().map(str::trim).filter(|value| !value.is_empty()) {
        body["abc"] = Value::String(format!("{abc}\n"));
    }
    insert_optional(&mut body, "cot", request.cot.clone());
    insert_optional(&mut body, "duration", request.duration_seconds);
    insert_optional(&mut body, "lm_seed", request.lm_seed.filter(|seed| *seed >= 0));
    insert_optional(&mut body, "seed", request.seed.filter(|seed| *seed >= 0));
    insert_optional(&mut body, "steps", request.steps);
    insert_optional(&mut body, "lm_batch_size", request.lm_batch_size);
    insert_optional(&mut body, "synth_batch_size", request.synth_batch_size);
    insert_optional(&mut body, "cfg_scale", request.cfg_scale.filter(|value| *value >= 0.0));
    insert_optional(&mut body, "semantic_tokens", semantic_tokens);
    insert_optional(&mut body, "peak_clip", request.peak_clip);
    insert_optional(&mut body, "output_format", request.output_format.clone());
    insert_optional(&mut body, "mp3_bitrate", request.mp3_bitrate);
    for (key, preset) in [("abc_sampling", &request.abc_sampling), ("semantic_sampling", &request.semantic_sampling)] {
        if let Some(preset) = preset.as_ref().filter(|preset| !preset.is_empty()) {
            preset.validate(key)?;
            body[key] = serde_json::to_value(preset).map_err(|error| error.to_string())?;
        }
    }
    if !request.adapters.is_empty() {
        body["adapters"] = Value::Array(adapter_fields(&request.adapters)?);
    }
    Ok(body)
}

/// The engine's own spelling of an adapter list: the folder as `name`, and
/// `<slot>_scale` for every slot, zero where the request leaves one out.
fn adapter_fields(uses: &[AdapterUse]) -> Result<Vec<Value>, String> {
    let slots = music_engine::yue_server::ADAPTER_SLOTS;
    uses.iter()
        .map(|adapter| {
            if adapter.id.trim().is_empty() {
                return Err("an adapter has no id".to_string());
            }
            if let Some(unknown) = adapter.scales.keys().find(|key| !slots.iter().any(|slot| slot.id == key.as_str())) {
                return Err(format!("adapter {} names an unknown slot {unknown}", adapter.id));
            }
            let mut entry = serde_json::json!({ "name": adapter.id });
            for slot in slots {
                let scale = adapter.scales.get(slot.id).copied().unwrap_or(0.0);
                if !scale.is_finite() || !(-4.0..=4.0).contains(&scale) {
                    return Err(format!("adapter {} strength must be between -4 and 4", adapter.id));
                }
                entry[format!("{}_scale", slot.id)] = serde_json::json!(scale);
            }
            Ok(entry)
        })
        .collect()
}

fn insert_optional<T: Serialize>(body: &mut Value, key: &str, value: Option<T>) {
    if let Some(value) = value {
        body[key] = serde_json::to_value(value).expect("serializable request value");
    }
}

fn queued_not_configured_job(request: CreateMusicJobRequest, engine_id: String) -> MusicJob {
    MusicJob {
        cover_prompt: None,
        id: format!("unconfigured-{}", uuid_suffix()),
        engine_id,
        title: request.title.clone(),
        status: MusicJobStatus::Queued,
        dispatch: MusicJobDispatch::NotConfigured,
        phase: MusicJobPhase::Queued,
        style: request.style,
        lyrics: request.lyrics,
        duration_seconds: request.duration_seconds.unwrap_or_default(),
        generation_settings: Value::Null,
        song: None,
        songs: vec![],
        message: "The selected local music engine is not configured; this job remains queued and no inference has started.".into(),
    }
}

fn failed_request_job(request: CreateMusicJobRequest, engine_id: String, error: String) -> MusicJob {
    MusicJob {
        cover_prompt: None,
        title: request.title.clone(),
        id: format!("rejected-{}", uuid_suffix()),
        engine_id,
        status: MusicJobStatus::Failed,
        dispatch: MusicJobDispatch::NotConfigured,
        phase: MusicJobPhase::Failed,
        style: request.style,
        lyrics: request.lyrics,
        duration_seconds: request.duration_seconds.unwrap_or_default(),
        generation_settings: Value::Null,
        song: None,
        songs: vec![],
        message: error,
    }
}

fn uuid_suffix() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// yue-server has no queued state: a job is `running` from the moment it is
/// accepted, whether the worker has reached it or not.
fn apply_remote_status(job: &mut MusicJob, remote_status: &str) {
    match remote_status {
        "running" => {
            job.status = MusicJobStatus::Running;
            job.phase = MusicJobPhase::Running;
            job.message = "The engine has this job.".into();
        }
        "done" => {
            job.status = MusicJobStatus::Completed;
            job.phase = MusicJobPhase::Completed;
            job.message = "The engine finished this job.".into();
        }
        "failed" => {
            job.status = MusicJobStatus::Failed;
            job.phase = MusicJobPhase::Failed;
            job.message = engine_failure_reason().unwrap_or_else(|| "The engine reported a failed job; its log has the reason.".into());
        }
        "cancelled" => {
            job.status = MusicJobStatus::Cancelled;
            job.dispatch = MusicJobDispatch::Cancelled;
            job.phase = MusicJobPhase::Cancelled;
            job.message = "The engine cancelled this job.".into();
        }
        other => {
            job.status = MusicJobStatus::Failed;
            job.phase = MusicJobPhase::Failed;
            job.message = format!("The engine returned an unknown job status: {other}");
        }
    }
}

fn api_error(status: StatusCode, error: String) -> (StatusCode, Json<ApiError>) {
    (status, Json(ApiError { error }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every file the editor page loads has to be embedded; a missing
    /// WaveSurfer bundle left the editor blank in 1.0.0 to 1.0.3.
    #[test]
    fn the_editor_page_loads_only_embedded_files() {
        let page = EDITOR
            .get_file("index.html")
            .and_then(|file| file.contents_utf8())
            .expect("the editor page is embedded");
        let mut checked = 0;
        for attribute in ["src=\"", "href=\""] {
            for (at, _) in page.match_indices(attribute) {
                let rest = &page[at + attribute.len()..];
                let target = &rest[..rest.find('"').expect("a closed attribute")];
                if target.contains(':') || target.starts_with('#') || target.is_empty() {
                    continue;
                }
                assert!(EDITOR.get_file(target).is_some(), "the editor page loads {target}, which is not embedded");
                checked += 1;
            }
        }
        assert!(checked > 20, "only {checked} local references were found in the editor page");
    }

    /// Nothing stays in VRAM unless the user asked for it. This is the setting
    /// the assistant's unload is tied to, and it is off to begin with.
    #[test]
    fn nothing_is_kept_in_memory_by_default() {
        assert!(!EngineOptions::default().keep_loaded);
        assert!(!EngineOptions::default().to_engine().keep_loaded);
    }

    /// A card that ran out of memory has to say so. The studio used to answer
    /// with "download the five components", pointing at models already on disk.
    #[test]
    fn an_out_of_memory_engine_is_named_as_one() {
        for line in [
            "ggml_backend_cuda_buffer_type_alloc_buffer: allocating 4096 MB on device 0: cudaMalloc failed: out of memory",
            "CUDA error: out of memory",
            "std::bad_alloc",
        ] {
            assert!(
                describes_exhausted_memory(&line.to_lowercase()),
                "this is what running out of memory looks like and it was not recognised: {line}"
            );
        }
        assert!(!describes_exhausted_memory("loading model from disk"));
    }

    #[test]
    fn primary_engine_is_selected_by_default() {
        assert_eq!(
            selected_local_music_engine(&initial_configuration()).as_deref(),
            Some(PRIMARY_MUSIC_ENGINE_ID)
        );
    }

    fn sample_request() -> CreateMusicJobRequest {
        CreateMusicJobRequest {
            style: "warm piano pop, female voice, 88 BPM".into(),
            lyrics: "[Verse]\r\none line".into(),
            ..CreateMusicJobRequest::default()
        }
    }

    #[test]
    fn the_engine_is_asked_for_float_when_the_studio_makes_the_mp3() {
        let mp3 = serde_json::json!({ "style": "x", "output_format": "mp3", "mp3_bitrate": 320 });
        let sent = engine_submission(&mp3);
        assert_eq!(sent["output_format"], "wav32");
        assert!(sent.get("mp3_bitrate").is_none());
        assert_eq!(engine_submission(&serde_json::json!({ "style": "x" }))["output_format"], "wav32");
        let wav = serde_json::json!({ "style": "x", "output_format": "wav24" });
        assert_eq!(engine_submission(&wav), wav);
    }

    #[test]
    fn adapters_travel_as_engine_fields_with_every_slot_spelled_out() {
        let mut scales = std::collections::BTreeMap::new();
        scales.insert("ar".to_string(), 0.75);
        let request = CreateMusicJobRequest {
            adapters: vec![AdapterUse { id: "yue2-instrumental".into(), scales }],
            ..sample_request()
        };
        let body = yue_request_from(&request, 1).unwrap();
        assert_eq!(body["adapters"], serde_json::json!([{ "name": "yue2-instrumental", "ar_scale": 0.75, "nar_scale": 0.0 }]));
        assert!(yue_request_from(&sample_request(), 1).unwrap().get("adapters").is_none());

        let mut unknown = std::collections::BTreeMap::new();
        unknown.insert("dit".to_string(), 1.0);
        let refused = CreateMusicJobRequest { adapters: vec![AdapterUse { id: "x".into(), scales: unknown }], ..sample_request() };
        assert!(yue_request_from(&refused, 1).unwrap_err().contains("unknown slot"));
        let mut huge = std::collections::BTreeMap::new();
        huge.insert("nar".to_string(), 40.0);
        let refused = CreateMusicJobRequest { adapters: vec![AdapterUse { id: "x".into(), scales: huge }], ..sample_request() };
        assert!(yue_request_from(&refused, 1).is_err());
    }

    #[test]
    fn a_sparse_request_stays_sparse() {
        let body = yue_request_from(&sample_request(), 1).unwrap();
        let object = body.as_object().unwrap();
        assert_eq!(object.len(), 2, "only style and lyrics travel when nothing else was set: {body}");
        assert_eq!(body["lyrics"], "[Verse]\none line");
    }

    #[test]
    fn every_set_field_reaches_the_engine_under_its_own_name() {
        let request = CreateMusicJobRequest {
            abc: Some("X:1\nK:C\nC".into()),
            cot: Some("melody".into()),
            duration_seconds: Some(95.0),
            lm_seed: Some(42),
            seed: Some(-1),
            steps: Some(40),
            lm_batch_size: Some(2),
            synth_batch_size: Some(3),
            cfg_scale: Some(1.2),
            output_format: Some("wav24".into()),
            mp3_bitrate: Some(320),
            peak_clip: Some(0),
            abc_sampling: Some(SamplingPreset { temperature: Some(0.8), ..SamplingPreset::default() }),
            semantic_sampling: Some(SamplingPreset::default()),
            ..sample_request()
        };
        let body = yue_request_from(&request, 2).unwrap();
        assert_eq!(body["abc"], "X:1\nK:C\nC\n");
        assert_eq!(body["cot"], "melody");
        assert_eq!(body["duration"], 95.0);
        assert_eq!(body["lm_seed"], 42);
        assert!(body.get("seed").is_none(), "a negative seed is the engine's random draw");
        assert_eq!(body["steps"], 40);
        assert_eq!(body["lm_batch_size"], 2);
        assert_eq!(body["synth_batch_size"], 3);
        assert_eq!(body["cfg_scale"], 1.2);
        assert_eq!(body["output_format"], "wav24");
        assert_eq!(body["peak_clip"], 0);
        assert_eq!(body["abc_sampling"], serde_json::json!({ "temperature": 0.8 }));
        assert!(body.get("semantic_sampling").is_none(), "an empty preset is the checkpoint preset");
    }

    #[test]
    fn requests_the_engine_would_refuse_are_refused_first_with_a_reason() {
        let cases: Vec<(CreateMusicJobRequest, &str)> = vec![
            (CreateMusicJobRequest { style: " ".into(), lyrics: String::new(), ..CreateMusicJobRequest::default() }, "style or lyrics"),
            (CreateMusicJobRequest { cot: Some("half".into()), ..sample_request() }, "cot"),
            (CreateMusicJobRequest { lm_batch_size: Some(2), ..sample_request() }, "lm_batch_size"),
            (CreateMusicJobRequest { synth_batch_size: Some(10), ..sample_request() }, "synth_batch_size"),
            (CreateMusicJobRequest { output_format: Some("flac".into()), ..sample_request() }, "output_format"),
            (CreateMusicJobRequest { duration_seconds: Some(400.0), ..sample_request() }, "duration"),
            (CreateMusicJobRequest { semantic_tokens: Some("1,2,x".into()), ..sample_request() }, "semantic_tokens"),
            (CreateMusicJobRequest { semantic_tokens: Some("1,40000".into()), ..sample_request() }, "semantic_tokens"),
            (CreateMusicJobRequest { abc_sampling: Some(SamplingPreset { top_p: Some(1.5), ..SamplingPreset::default() }), ..sample_request() }, "top_p"),
            (CreateMusicJobRequest { semantic_sampling: Some(SamplingPreset { min_tokens: Some(10), max_tokens: Some(5), ..SamplingPreset::default() }), ..sample_request() }, "min_tokens"),
        ];
        for (request, expected) in cases {
            let error = yue_request_from(&request, 1).expect_err(expected);
            assert!(error.contains(expected), "{expected}: {error}");
        }
    }

    #[test]
    fn codes_alone_are_a_valid_request() {
        let request = CreateMusicJobRequest { semantic_tokens: Some("12, 8433 ,22418".into()), ..CreateMusicJobRequest::default() };
        let body = yue_request_from(&request, 1).unwrap();
        assert_eq!(body["semantic_tokens"], "12, 8433 ,22418");
    }

    #[test]
    fn persisted_settings_round_trip_a_complete_custom_component_selection() {
        let settings = PersistedStudioSettings {
            engine_options: EngineOptions { keep_loaded: true, max_batch: Some(2), ..EngineOptions::default() },
            assistant: AssistantConfig { provider: AssistantProvider::Local, local_base_url: Some("http://127.0.0.1:8080/v1".into()), local_model: Some("gemma".into()), openrouter_model: None, managed_model: None, managed_path: None, reasoning_effort: None },
            configuration: initial_configuration(),
            // Karaoke is off by default and has to survive a restart the same
            // way the assistant does.
            lyrics_sync: lyrics_sync::LyricsSyncConfig {
                enabled: true,
                provider: lyrics_sync::AsrProvider::Parakeet,
                whisper_model: None,
                openrouter_model: None,
                runtime: lyrics_sync::OnnxFlavour::default(),
            },
            selected_profile_id: None,
            selected_component_ids: Some(vec!["backbone-q8".into(), "vae-f32".into()]),
            cover_templates: Some(cover_prompt::default_templates()),
            cover_auto: Some(true),
            separation: Some(separation::SeparationConfig::default()),
            cover_template_default: Some("photographic".into()),
        };
        let restored: PersistedStudioSettings = serde_json::from_slice(&serde_json::to_vec(&settings).unwrap()).unwrap();
        assert!(restored.lyrics_sync.available());
        assert_eq!(restored.lyrics_sync.provider, lyrics_sync::AsrProvider::Parakeet);
        assert!(restored.selected_profile_id.is_none());
        assert_eq!(restored.selected_component_ids.unwrap(), vec!["backbone-q8", "vae-f32"]);
        // Engine flags survive a restart, and the songs-per-request ceiling is
        // derived from them rather than assumed.
        assert!(restored.engine_options.keep_loaded);
        assert_eq!(restored.engine_options.effective_max_batch(), 2);
        // Four by default: songs decoded together are nearly free after the
        // first, and the upstream default of 1 left the slider disabled.
        // Nothing is reserved that nobody asked for.
        assert_eq!(EngineOptions::default().effective_max_batch(), 1);
        // And whatever it is, the engine is started with it: the request
        // carries `lm_batch_size`, and the engine refuses anything above the
        // ceiling it was loaded with. Offering more in the panel than the
        // engine was given is what made a request for two songs fail at once.
        assert_eq!(EngineOptions::default().to_engine().max_batch, Some(1));
        assert_eq!(EngineOptions { max_batch: Some(3), ..EngineOptions::default() }.to_engine().max_batch, Some(3));
        let vulkan = EngineOptions { backend: music_engine::yue_server::ComputeBackend::Vulkan, ..EngineOptions::default() }.to_engine();
        assert!(vulkan.clamp_fp16, "Vulkan runs clamp hidden states to FP16");
        let cuda = EngineOptions { backend: music_engine::yue_server::ComputeBackend::Cuda, ..EngineOptions::default() }.to_engine();
        assert!(!cuda.clamp_fp16);
        // The assistant is optional: it must survive a restart when configured,
        // and stay unavailable when it is not.
        assert!(restored.assistant.available());
        assert!(!AssistantConfig::default().available());
    }

    #[test]
    fn remote_statuses_never_claim_success_for_an_unknown_value() {
        let mut job = queued_not_configured_job(sample_request(), PRIMARY_MUSIC_ENGINE_ID.into());
        apply_remote_status(&mut job, "not-a-real-status");
        assert!(matches!(job.status, MusicJobStatus::Failed));
    }

    #[test]
    fn capabilities_use_the_engines_envelope_and_music_stays_local() {
        let after_refresh = CapabilitiesResponse { engines: capability_engines(false) };
        assert!(!after_refresh.engines.iter().find(|engine| engine.id == "openrouter").unwrap().capabilities.contains(&Capability::MusicGeneration));
        // The music engine, two recognisers, the local assistant and
        // OpenRouter: everything listed is something the studio can actually do.
        assert_eq!(serde_json::to_value(after_refresh).unwrap()["engines"].as_array().unwrap().len(), 5);
    }

    #[test]
    fn a_composition_runs_the_planning_stage_and_next_to_nothing_else() {
        let request = ComposeScoreRequest { style: "folk rock".into(), lyrics: "[Verse]\r\nline".into(), cot: Some("melody".into()), lm_seed: Some(7), abc_sampling: None };
        let body = compose_request_from(&request).unwrap();
        assert_eq!(body["cot"], "melody");
        assert_eq!(body["duration"], 1.0);
        assert_eq!(body["steps"], 1);
        assert_eq!(body["lm_batch_size"], 1);
        assert_eq!(body["lm_seed"], 7);
        assert_eq!(body["lyrics"], "[Verse]\nline");
        assert!(body.get("abc").is_none());
    }

    #[test]
    fn a_composition_needs_a_mode_that_writes_a_score_and_a_prompt() {
        let off = ComposeScoreRequest { style: "pop".into(), lyrics: String::new(), cot: Some("off".into()), lm_seed: None, abc_sampling: None };
        assert!(compose_request_from(&off).is_err());
        let empty = ComposeScoreRequest { style: " ".into(), lyrics: String::new(), cot: None, lm_seed: None, abc_sampling: None };
        assert!(compose_request_from(&empty).is_err());
    }

    #[test]
    fn a_transcription_result_is_read_as_json() {
        let (abc, seed) = score_from_result("application/json", br#"{"abc":"X:1\nK:C\n|C|\n"}"#).unwrap();
        assert_eq!(abc, "X:1\nK:C\n|C|");
        assert_eq!(seed, None);
        assert!(score_from_result("application/json", br#"{"abc":""}"#).is_err());
    }

    fn replay_overrides() -> ReplayMusicJobRequest {
        ReplayMusicJobRequest { song_id: None, replay_request: None, steps: None, seed: None, synth_batch_size: None, output_format: None, peak_clip: None, mp3_bitrate: None, title: None }
    }

    #[test]
    fn a_rerender_keeps_the_music_and_changes_only_the_acoustic_side() {
        let request = ReplayMusicJobRequest { steps: Some(48), seed: Some(9), synth_batch_size: Some(2), output_format: Some("wav24".into()), mp3_bitrate: Some(192), ..replay_overrides() };
        let replay = serde_json::json!({"style":"piano pop","lyrics":"[Verse] hi","abc":"X:1\nK:C\n","semantic_tokens":"1,2,3","lm_seed":123,"seed":1,"steps":32,"cot":"full"});
        let prepared = prepare_replay_synthesis(replay, &request).unwrap();
        assert_eq!(prepared["semantic_tokens"], "1,2,3");
        assert_eq!(prepared["abc"], "X:1\nK:C\n");
        assert_eq!(prepared["lm_seed"], 123);
        assert_eq!(prepared["steps"], 48);
        assert_eq!(prepared["seed"], 9);
        assert_eq!(prepared["synth_batch_size"], 2);
        assert_eq!(prepared["output_format"], "wav24");
        assert_eq!(prepared["mp3_bitrate"], 192);
        assert_eq!(prepared["lm_batch_size"], 1);
    }

    #[test]
    fn a_track_without_its_semantic_stream_cannot_be_rerendered() {
        assert!(prepare_replay_synthesis(serde_json::json!({"style":"s","lyrics":"l"}), &replay_overrides()).is_err());
        assert!(prepare_replay_synthesis(serde_json::json!({"style":"s","semantic_tokens":"1,oops"}), &replay_overrides()).is_err());
    }

    #[test]
    fn engine_options_become_launch_flags_with_a_batch_ceiling() {
        let options = EngineOptions { max_seq: Some(8192), vae_core: Some(256), ..EngineOptions::default() }.to_engine();
        assert_eq!(options.max_batch, Some(1));
        assert_eq!(options.max_seq, Some(8192));
        assert_eq!(options.vae_core, Some(256));
    }
}
