//! The studio as an MCP server: Streamable HTTP, stateless JSON-RPC at `/mcp`.
//!
//! Every tool is a route of the studio's own API, called inside the process
//! through the same router the page talks to, so an agent does exactly what
//! the page does through the same code: create songs, write lyrics and styles,
//! prepare datasets, train, install LoRA, draw covers, split stems. A file an
//! agent names by its path is sent to the route as the multipart upload the
//! page would send.

use std::collections::HashMap;
use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use serde_json::{json, Value};
use tower::ServiceExt;

/// The studio's API router, set once the service has built it.
static API: OnceLock<Router> = OnceLock::new();

pub fn install(api: Router) {
    let _ = API.set(api);
}

const PROTOCOL: &str = "2025-06-18";
/// The skill an agent reads, served as a resource and a prompt.
const SKILL: &str = include_str!("../../../docs/mcp-skill.md");
const SKILL_URI: &str = "studio://skill";
const GUIDE_URI: &str = "studio://guide/";
/// Answers longer than this are cut: a library of hundreds of songs is more
/// than an agent reads in one call.
const LIMIT: usize = 60_000;

/// What a tool sends to its route.
enum Payload {
    None,
    Json(Value),
    /// Multipart form fields and files, as the page uploads them.
    Form { fields: Vec<(String, String)>, files: Vec<(String, PathBuf, String)> },
    /// A command for the studio's window: what is on screen, the buttons, the
    /// player, the video editor. Answered by the page itself.
    Window { command: &'static str, args: Value, seconds: u64 },
}

struct Call {
    method: Method,
    path: String,
    payload: Payload,
}

struct Tool {
    name: &'static str,
    description: &'static str,
    schema: fn() -> Value,
    call: fn(&Value) -> Result<Call, String>,
}

fn get(path: String) -> Result<Call, String> {
    Ok(Call { method: Method::GET, path, payload: Payload::None })
}

fn post(path: String, body: Value) -> Result<Call, String> {
    Ok(Call { method: Method::POST, path, payload: Payload::Json(body) })
}

fn send(method: Method, path: String, body: Value) -> Result<Call, String> {
    Ok(Call { method, path, payload: Payload::Json(body) })
}

fn window(command: &'static str, args: &Value, seconds: u64) -> Result<Call, String> {
    Ok(Call { method: Method::GET, path: String::new(), payload: Payload::Window { command, args: args.clone(), seconds } })
}

fn composite(kind: &'static str) -> Result<Call, String> {
    Ok(Call { method: Method::GET, path: format!("composite:{kind}"), payload: Payload::None })
}

/// A GET inside the process, as JSON.
async fn fetch(path: &str) -> Value {
    match call_route(Call { method: Method::GET, path: path.into(), payload: Payload::None }).await {
        Ok((_, text)) => serde_json::from_str(&text).unwrap_or(Value::String(text)),
        Err(problem) => json!({ "error": problem }),
    }
}

fn compact_training(training: &Value) -> Value {
    let datasets: Vec<Value> = training["datasets"].as_array().into_iter().flatten().map(|dataset| {
        let items = dataset["items"].as_array().cloned().unwrap_or_default();
        let count = |field: &str, value: &str| items.iter().filter(|item| item[field] == value).count();
        json!({
            "id": dataset["id"], "name": dataset["name"], "trigger": dataset["trigger"], "songs": items.len(),
            "ready": items.iter().filter(|item| item["lyrics_state"] == "done" && item["style_state"] == "done").count(),
            "lyrics": { "wanted": count("lyrics_state", "wanted"), "found": count("lyrics_state", "found"), "done": count("lyrics_state", "done") },
            "style": { "wanted": count("style_state", "wanted"), "heard": count("style_state", "heard"), "done": count("style_state", "done") },
        })
    }).collect();
    let runs: Vec<Value> = training["runs"].as_array().into_iter().flatten().map(compact_run).collect();
    json!({
        "trainer_installed": training["pack_ready"],
        "listening_installed": training["listen"]["ready"],
        "download": training["download"],
        "preparation": compact_preparation(&training["prepare"]),
        "active_run": training["active"],
        "datasets": datasets,
        "runs": runs,
        "recipe_defaults": training["recipe_defaults"],
    })
}

fn compact_run(run: &Value) -> Value {
    let steps = run["steps"].as_array().cloned().unwrap_or_default();
    let last = steps.last().cloned().unwrap_or(Value::Null);
    json!({
        "id": run["id"], "name": run["name"], "dataset": run["dataset_name"], "trigger": run["trigger"], "status": run["status"], "stage": run["stage"],
        "steps_done": steps.len(), "step_limit": run["recipe"]["steps"], "stop": run["recipe"]["stop"], "target_kl": run["recipe"]["target_kl"],
        "last_step": last, "checkpoints": run["checkpoints"], "installed": run["installed"], "error": run["error"],
    })
}

fn compact_preparation(prepare: &Value) -> Value {
    if prepare.is_null() {
        return Value::Null;
    }
    json!({
        "dataset": prepare["dataset"], "finished": prepare["finished"], "cancelled": prepare["cancelled"],
        "stages": prepare["stages"], "songs_left": prepare["pending"].as_array().map_or(0, Vec::len),
        "failures": prepare["failures"], "notices": prepare["notices"], "run": prepare["run"],
    })
}

/// What an agent is answered: the parts it acts on, unless it asked for
/// every field with response_format detailed.
fn shape(name: &str, args: &Value, value: Value) -> Value {
    let detailed = args.get("response_format").and_then(Value::as_str) == Some("detailed");
    match name {
        "training_status" if !detailed => compact_training(&value),
        "dataset_get" => {
            let wanted = args.get("dataset_id").and_then(Value::as_str).unwrap_or_default();
            let Some(dataset) = value["datasets"].as_array().into_iter().flatten().find(|dataset| dataset["id"] == wanted).cloned() else {
                return json!({ "error": format!("No dataset {wanted}; training_status lists them.") });
            };
            match args.get("song_id").and_then(Value::as_str) {
                Some(song) => dataset["items"].as_array().into_iter().flatten().find(|item| item["id"] == song).cloned().unwrap_or_else(|| json!({ "error": format!("No song {song} in the dataset.") })),
                None => dataset,
            }
        }
        "library_songs_list" => {
            let query = args.get("query").and_then(Value::as_str).unwrap_or_default().to_lowercase();
            let songs = value.as_array().cloned().unwrap_or_default().into_iter().filter(|song| {
                query.is_empty() || song["title"].as_str().unwrap_or_default().to_lowercase().contains(&query) || song["caption"].as_str().unwrap_or_default().to_lowercase().contains(&query)
            });
            if detailed {
                return Value::Array(songs.collect());
            }
            Value::Array(songs.map(|song| {
                let style: String = song["caption"].as_str().unwrap_or_default().chars().take(90).collect();
                json!({ "id": song["id"], "title": song["title"], "made": song["created_at"], "style": style })
            }).collect())
        }
        _ => value,
    }
}

async fn status_summary() -> Value {
    let (jobs, activity, training, processing) = tokio::join!(fetch("/v1/music/jobs"), fetch("/v1/activity"), fetch("/v1/training"), fetch("/v1/processing"));
    let running_activity: Vec<Value> = activity["activity"].as_array().into_iter().flatten().filter(|entry| entry["state"] == "running").cloned().collect();
    let active_run = training["runs"].as_array().into_iter().flatten().find(|run| Some(run["id"].as_str().unwrap_or_default()) == training["active"].as_str()).map(compact_run);
    json!({
        "window_open": bridge().windows.load(Ordering::Relaxed) > 0,
        "song_jobs": jobs,
        "covers_and_karaoke": running_activity,
        "preparation": compact_preparation(&training["prepare"]),
        "training": active_run,
        "processing": processing,
    })
}

/// Waits for a job, the preparation, training or everything, a slice at a time.
async fn wait_for(args: &Value) -> Value {
    let seconds = args.get("seconds").and_then(Value::as_u64).unwrap_or(60).clamp(2, 240);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
    let job = args.get("job_id").and_then(Value::as_str).map(str::to_string);
    let until = args.get("until").and_then(Value::as_str).unwrap_or("idle").to_string();
    loop {
        let (done, now) = match &job {
            Some(job) => {
                let mut state = fetch(&format!("/v1/music/jobs/{}", segment(job))).await;
                if state.get("error").is_some() || state.get("status").is_none() {
                    state = fetch(&format!("/v1/scores/{}", segment(job))).await;
                }
                let status = state["status"].as_str().unwrap_or_default().to_string();
                (["completed", "failed", "cancelled", "done", "error"].contains(&status.as_str()), state)
            }
            None => {
                let summary = status_summary().await;
                let preparing = summary["preparation"].is_object() && summary["preparation"]["finished"] != true;
                let training = !summary["training"].is_null();
                let songs = summary["song_jobs"].as_array().is_some_and(|jobs| !jobs.is_empty());
                let done = match until.as_str() {
                    "preparation" => !preparing,
                    "training" => !training,
                    _ => !preparing && !training && !songs,
                };
                (done, summary)
            }
        };
        if done {
            return json!({ "done": true, "state": now });
        }
        if tokio::time::Instant::now() >= deadline {
            return json!({ "done": false, "note": "still running; call studio_wait again to keep waiting", "state": now });
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// MCP tool annotations, from what each tool does.
fn annotations(name: &str) -> Value {
    const READS: &[&str] = &["_get", "_status", "_list", "_catalog", "_files", "_logs", "_log", "_runtime", "_local_models", "_hf_files", "_search_hf", "_capabilities", "_system", "_state", "_read_page", "_screenshot"];
    let read_only = READS.iter().any(|part| name.ends_with(part) || name.contains(&format!("{part}_"))) || name.starts_with("writing_") || name == "lyrics_find" || name == "cover_prompt_render" || name == "studio_wait" || name == "engine_presets_get";
    let destructive = name.ends_with("_delete") || name.ends_with("_remove") || name == "processing_discard" || name.ends_with("_cancel") || name.contains("_cancel_");
    let title = name.replace('_', " ");
    json!({ "title": title, "readOnlyHint": read_only, "destructiveHint": destructive, "idempotentHint": read_only || name.ends_with("_set") || name.contains("_select"), "openWorldHint": name.starts_with("openrouter_") || name.contains("_hf") || name == "lyrics_find" })
}

/// The window's side of the bridge: the page subscribes to the commands and
/// posts each answer back.
struct Bridge {
    commands: tokio::sync::broadcast::Sender<String>,
    pending: Mutex<HashMap<String, tokio::sync::oneshot::Sender<Result<Value, String>>>>,
    windows: AtomicUsize,
    sequence: AtomicU64,
}

fn bridge() -> &'static Bridge {
    static BRIDGE: OnceLock<Bridge> = OnceLock::new();
    BRIDGE.get_or_init(|| Bridge { commands: tokio::sync::broadcast::channel(64).0, pending: Mutex::new(HashMap::new()), windows: AtomicUsize::new(0), sequence: AtomicU64::new(0) })
}

/// Counts the window while its stream is open.
struct Listening;

impl Drop for Listening {
    fn drop(&mut self) {
        bridge().windows.fetch_sub(1, Ordering::Relaxed);
    }
}

/// The stream of commands the studio's page executes.
pub async fn window_events() -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
    bridge().windows.fetch_add(1, Ordering::Relaxed);
    let receiver = bridge().commands.subscribe();
    let stream = futures_util::stream::unfold((receiver, Listening), |(mut receiver, listening)| async move {
        loop {
            match receiver.recv().await {
                Ok(command) => return Some((Ok(Event::default().data(command)), (receiver, listening))),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[derive(serde::Deserialize)]
pub struct WindowAnswer {
    id: String,
    #[serde(default)]
    result: Value,
    #[serde(default)]
    error: Option<String>,
}

/// The page's answer to one command.
pub async fn window_result(Json(answer): Json<WindowAnswer>) -> StatusCode {
    let waiting = bridge().pending.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).remove(&answer.id);
    match waiting {
        Some(sender) => {
            let _ = sender.send(match answer.error {
                Some(error) => Err(error),
                None => Ok(answer.result),
            });
            StatusCode::NO_CONTENT
        }
        None => StatusCode::NOT_FOUND,
    }
}

async fn ask_window(command: &str, args: Value, seconds: u64) -> Result<Value, String> {
    if bridge().windows.load(Ordering::Relaxed) == 0 {
        return Err("The studio's window is not open. Open YuE2 Studio and call the tool again; everything else works without it.".into());
    }
    let id = format!("w{}", bridge().sequence.fetch_add(1, Ordering::Relaxed));
    let (sender, receiver) = tokio::sync::oneshot::channel();
    bridge().pending.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).insert(id.clone(), sender);
    let _ = bridge().commands.send(json!({ "id": id, "command": command, "args": args }).to_string());
    match tokio::time::timeout(Duration::from_secs(seconds), receiver).await {
        Ok(Ok(answer)) => answer,
        _ => {
            bridge().pending.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).remove(&id);
            Err(format!("The window did not answer '{command}' within {seconds} s."))
        }
    }
}

/// A required text argument, or a message saying which is missing.
fn text(args: &Value, name: &str) -> Result<String, String> {
    args.get(name).and_then(Value::as_str).map(str::trim).filter(|value| !value.is_empty()).map(str::to_string).ok_or_else(|| format!("'{name}' is required"))
}

/// A path segment, escaped.
fn segment(value: &str) -> String {
    value.bytes().map(|byte| if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) { (byte as char).to_string() } else { format!("%{byte:02X}") }).collect()
}

/// The arguments without the ones that went into the path.
fn body_without(args: &Value, taken: &[&str]) -> Value {
    let mut body = args.as_object().cloned().unwrap_or_default();
    for name in taken {
        body.remove(*name);
    }
    Value::Object(body)
}

fn object(properties: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": properties, "required": required })
}

fn id_only(name: &str, what: &str) -> Value {
    object(json!({ name: { "type": "string", "description": what } }), &[name])
}

fn nothing() -> Value {
    object(json!({}), &[])
}

/// Audio, lyrics and cue files under a folder, with their path inside it:
/// the folders a song sits in name its artist when its tags do not.
fn folder_files(folder: &Path) -> Result<Vec<(String, PathBuf, String)>, String> {
    let root = folder.parent().unwrap_or(folder);
    let mut found = Vec::new();
    let mut stack = vec![folder.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).map_err(|error| format!("read {}: {error}", dir.display()))?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let extension = path.extension().and_then(|value| value.to_str()).unwrap_or_default().to_lowercase();
            if ["wav", "mp3", "flac", "ogg", "m4a", "txt", "lrc", "cue"].contains(&extension.as_str()) {
                let relative = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
                found.push(("files".to_string(), path, relative));
            }
        }
    }
    found.sort_by(|a, b| a.2.cmp(&b.2));
    if found.is_empty() {
        return Err(format!("no audio in {}", folder.display()));
    }
    Ok(found)
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| "audio".into())
}

fn tools() -> &'static [Tool] {
    static TOOLS: OnceLock<Vec<Tool>> = OnceLock::new();
    TOOLS.get_or_init(|| {
        vec![
            // ---------------------------------------------------------------- the studio
            Tool {
                name: "studio_status",
                description: "What the studio is doing now, in one short summary: song jobs, covers and karaoke, the dataset preparation and its stages, the training run with its step, loss and KL, audio processing, and whether the studio's window is open. Call it first, and use studio_wait to wait.",
                schema: nothing,
                call: |_| composite("status"),
            },
            Tool {
                name: "studio_wait",
                description: "Wait for work to finish instead of polling: a song or score job (job_id), the dataset preparation (until: preparation), the training run (until: training), or everything (until: idle). Returns when it is done or after seconds (60 by default, at most 240) with how far it got; call it again to keep waiting.",
                schema: || object(json!({ "job_id": { "type": "string" }, "until": { "type": "string", "enum": ["preparation", "training", "idle"] }, "seconds": { "type": "integer" } }), &[]),
                call: |_| composite("wait"),
            },
            Tool {
                name: "studio_system",
                description: "The graphics card, its free video memory, RAM, CPU load and the engine's memory.",
                schema: nothing,
                call: |_| get("/v1/system/resources".into()),
            },
            Tool {
                name: "models_status",
                description: "The installed model set, what is downloaded and what is missing, and download progress.",
                schema: nothing,
                call: |_| get("/setup/status".into()),
            },
            Tool {
                name: "models_catalog",
                description: "Every model set and file the studio can download, with sizes and the video memory each needs.",
                schema: nothing,
                call: |_| get("/setup/catalog".into()),
            },
            Tool {
                name: "models_download",
                description: "Download a model set (profile_id from models_catalog) or single components (ids). Resumes what is partly there.",
                schema: || object(json!({ "profile_id": { "type": "string" }, "ids": { "type": "array", "items": { "type": "string" } } }), &[]),
                call: |args| post("/setup/download".into(), args.clone()),
            },
            Tool {
                name: "models_select",
                description: "Use an installed model set (profile_id) or a custom mix of components (component_ids) for generation.",
                schema: || object(json!({ "profile_id": { "type": "string" }, "component_ids": { "type": "array", "items": { "type": "string" } } }), &[]),
                call: |args| post("/setup/select".into(), args.clone()),
            },
            Tool {
                name: "engine_options_get",
                description: "The engine's launch options: backend (auto, cuda, vulkan, cpu), keep models loaded, and the rest.",
                schema: nothing,
                call: |_| get("/engine/options".into()),
            },
            Tool {
                name: "engine_options_set",
                description: "Change the engine's launch options (the whole object as engine_options_get returns it, with your changes); engine_restart applies them.",
                schema: || json!({ "type": "object" }),
                call: |args| send(Method::PUT, "/engine/options".into(), args.clone()),
            },
            Tool {
                name: "engine_restart",
                description: "Restart the music engine, for example after changing its options or models.",
                schema: nothing,
                call: |_| post("/engine/restart".into(), json!({})),
            },
            // ---------------------------------------------------------------- the window: what the user sees
            Tool {
                name: "ui_screenshot",
                description: "A picture of the studio's window as the user sees it now. Use it to check what a change looks like, and before clicking anything.",
                schema: || object(json!({ "max_width": { "type": "integer", "description": "pixels, 1600 by default" } }), &[]),
                call: |args| window("screenshot", args, 30),
            },
            Tool {
                name: "ui_read_page",
                description: "Every visible control of the window, one per line: its ref (e12), kind, label and value. Refs are what ui_click, ui_type and ui_select take; read the page again after it changes.",
                schema: nothing,
                call: |args| window("read_page", args, 15),
            },
            Tool {
                name: "ui_click",
                description: "Click a control of the window like the user would: by ref from ui_read_page, or by its visible label in text.",
                schema: || object(json!({ "ref": { "type": "string" }, "text": { "type": "string" } }), &[]),
                call: |args| window("click", args, 15),
            },
            Tool {
                name: "ui_type",
                description: "Type into a field of the window (replaces its text); submit presses Enter after.",
                schema: || object(json!({ "ref": { "type": "string" }, "text": { "type": "string", "description": "the field's label, when there is no ref" }, "value": { "type": "string" }, "submit": { "type": "boolean" } }), &["value"]),
                call: |args| window("type", args, 15),
            },
            Tool {
                name: "ui_select",
                description: "Choose an option of a list in the window.",
                schema: || object(json!({ "ref": { "type": "string" }, "text": { "type": "string" }, "value": { "type": "string" } }), &["value"]),
                call: |args| window("select", args, 15),
            },
            Tool {
                name: "ui_press_key",
                description: "Press a key in the window: Enter, Escape (closes a dialog), Tab, ArrowDown...",
                schema: || id_only("key", "the key"),
                call: |args| window("press_key", args, 15),
            },
            Tool {
                name: "ui_scroll",
                description: "Scroll the page (direction down or up, amount in pixels), or bring a control into view by ref or text.",
                schema: || object(json!({ "direction": { "type": "string", "enum": ["down", "up"] }, "amount": { "type": "integer" }, "ref": { "type": "string" }, "text": { "type": "string" } }), &[]),
                call: |args| window("scroll", args, 15),
            },
            Tool {
                name: "ui_navigate",
                description: "Open a page of the studio: create, library, search, playlist, adapters (LoRA and training), tools, news.",
                schema: || object(json!({ "view": { "type": "string", "enum": ["create", "library", "search", "playlist", "adapters", "tools", "news"] } }), &["view"]),
                call: |args| window("navigate", args, 15),
            },
            Tool {
                name: "ui_open_settings",
                description: "Open the settings window, at a section when given.",
                schema: || object(json!({ "section": { "type": "string" } }), &[]),
                call: |args| window("open_settings", args, 15),
            },
            // ---------------------------------------------------------------- the player
            Tool {
                name: "player_state",
                description: "What the studio's player plays: the song, playing or paused, position, length, volume, shuffle, repeat.",
                schema: nothing,
                call: |args| window("player_state", args, 15),
            },
            Tool {
                name: "player_play",
                description: "Play a library song (song_id) in the studio's player, or resume what is loaded.",
                schema: || object(json!({ "song_id": { "type": "string" } }), &[]),
                call: |args| window("player_play", args, 15),
            },
            Tool {
                name: "player_pause",
                description: "Pause the studio's player.",
                schema: nothing,
                call: |args| window("player_pause", args, 15),
            },
            Tool {
                name: "player_seek",
                description: "Move the player to a position in seconds.",
                schema: || object(json!({ "seconds": { "type": "number" } }), &["seconds"]),
                call: |args| window("player_seek", args, 15),
            },
            Tool {
                name: "player_next",
                description: "Play the next song of the queue.",
                schema: nothing,
                call: |args| window("player_next", args, 15),
            },
            Tool {
                name: "player_previous",
                description: "Play the previous song of the queue.",
                schema: nothing,
                call: |args| window("player_previous", args, 15),
            },
            Tool {
                name: "player_set",
                description: "Set the player's volume (0 to 1), shuffle, and repeat (none, all, one).",
                schema: || object(json!({ "volume": { "type": "number" }, "shuffle": { "type": "boolean" }, "repeat": { "type": "string", "enum": ["none", "all", "one"] } }), &[]),
                call: |args| window("player_set", args, 15),
            },
            // ---------------------------------------------------------------- the video editor
            Tool {
                name: "video_open",
                description: "Open the video editor for a library song: an audio-reactive clip with a visualiser, background, cover, text layers and karaoke lyrics, rendered to MP4.",
                schema: || id_only("song_id", "library song id"),
                call: |args| window("video_open", args, 15),
            },
            Tool {
                name: "video_get",
                description: "Everything the open video editor is set to: preset and the list of presets, aspect ratio, colours, positions, effects and their intensities, text layers, lyrics overlay, background, cover, playback, and the render's progress and saved file.",
                schema: nothing,
                call: |args| window("video_get", args, 15),
            },
            Tool {
                name: "video_set",
                description: "Change the open video editor; any part, the rest stays. config: {preset, aspect_ratio 16:9|9:16|1:1, primaryColor, secondaryColor, bgDim 0-1, particleCount, visualizerX/Y 0-100, visualizerScale 0.3-2, lyricsX/Y}. effects: {shake, glitch, vhs, cctv, scanlines, chromatic, bloom, filmGrain, pixelate, strobe, vignette, hueShift, letterbox: true/false}; intensities: the same names, 0-1. text_layers: [{text, x, y, size, color, font}], replaces all. lyrics: {enabled, style lines|scroll|karaoke, position, font_size, lines, show_sections, color, background_color, background_opacity, highlight_color, offset_seconds}. background: {image_path | video_path | image_url | video_url | type: random}. album_art_path, or album_art: null for the song cover.",
                schema: || object(json!({
                    "config": { "type": "object" },
                    "effects": { "type": "object" },
                    "intensities": { "type": "object" },
                    "text_layers": { "type": "array", "items": { "type": "object" } },
                    "lyrics": { "type": "object" },
                    "background": { "type": "object" },
                    "album_art_path": { "type": "string" },
                    "album_art": { "type": ["string", "null"] }
                }), &[]),
                call: |args| {
                    let mut args = args.clone();
                    // files on this computer reach the page as data URLs
                    let data_url = |path: &str| -> Result<String, String> {
                        use base64::Engine;
                        let path = PathBuf::from(path);
                        let bytes = std::fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
                        let kind = match path.extension().and_then(|value| value.to_str()).unwrap_or_default().to_lowercase().as_str() {
                            "jpg" | "jpeg" => "image/jpeg",
                            "png" => "image/png",
                            "webp" => "image/webp",
                            "gif" => "image/gif",
                            "mp4" => "video/mp4",
                            "webm" => "video/webm",
                            "mov" => "video/quicktime",
                            other => return Err(format!("{other} files cannot be a background or a cover")),
                        };
                        Ok(format!("data:{kind};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes)))
                    };
                    if let Some(background) = args.get_mut("background").and_then(Value::as_object_mut) {
                        if let Some(path) = background.remove("image_path").and_then(|value| value.as_str().map(str::to_string)) {
                            background.insert("image".into(), data_url(&path)?.into());
                        }
                        if let Some(path) = background.remove("video_path").and_then(|value| value.as_str().map(str::to_string)) {
                            background.insert("video".into(), data_url(&path)?.into());
                        }
                        if let Some(url) = background.remove("image_url") {
                            background.insert("image".into(), url);
                        }
                        if let Some(url) = background.remove("video_url") {
                            background.insert("video".into(), url);
                        }
                    }
                    if let Some(path) = args.get("album_art_path").and_then(Value::as_str).map(str::to_string) {
                        args["album_art"] = data_url(&path)?.into();
                        args.as_object_mut().map(|fields| fields.remove("album_art_path"));
                    }
                    window("video_set", &args, 30)
                },
            },
            Tool {
                name: "video_render",
                description: "Render the open video editor's clip to MP4. It runs in the window and returns at once; video_get shows the progress, and export.saved names the file on this computer when it is done.",
                schema: || object(json!({ "name": { "type": "string", "description": "file name, the song title by default" } }), &[]),
                call: |args| window("video_render", args, 15),
            },
            Tool {
                name: "video_play",
                description: "Play the clip's preview in the video editor.",
                schema: nothing,
                call: |args| window("video_play", args, 15),
            },
            Tool {
                name: "video_pause",
                description: "Pause the clip's preview.",
                schema: nothing,
                call: |args| window("video_pause", args, 15),
            },
            Tool {
                name: "video_seek",
                description: "Move the clip's preview to a position in seconds, to look at a frame with ui_screenshot.",
                schema: || object(json!({ "seconds": { "type": "number" } }), &["seconds"]),
                call: |args| window("video_seek", args, 15),
            },
            Tool {
                name: "video_close",
                description: "Close the video editor.",
                schema: nothing,
                call: |args| window("video_close", args, 15),
            },
            // ---------------------------------------------------------------- songs
            Tool {
                name: "song_create",
                description: "Generate a song with YuE2. style: one English sentence (language, genre, vocal, instruments, mood, production, 'N BPM'). lyrics: sections tagged [Verse 1], [Chorus], [Bridge]... one per line, blank line between sections; empty for an instrumental. abc: a score to sing (from score_compose or an edited one), else the model writes its own. cot: full (melody and chords), melody, or off. adapters: installed LoRA ids with strengths per slot (see lora_list). Returns a job; poll song_job_get until completed, the song then is in the library.",
                schema: || object(json!({
                    "style": { "type": "string" },
                    "lyrics": { "type": "string" },
                    "title": { "type": "string" },
                    "abc": { "type": "string" },
                    "cot": { "type": "string", "enum": ["full", "melody", "off"] },
                    "duration_seconds": { "type": "number" },
                    "seed": { "type": "integer" },
                    "lm_seed": { "type": "integer" },
                    "steps": { "type": "integer" },
                    "cfg_scale": { "type": "number" },
                    "output_format": { "type": "string", "enum": ["mp3", "wav16", "wav24", "wav32"] },
                    "cover_prompt": { "type": "string", "description": "What the cover should show" },
                    "adapters": { "type": "array", "items": { "type": "object", "properties": { "id": { "type": "string" }, "scales": { "type": "object", "description": "slot -> strength, e.g. {\"ar\": 1, \"nar\": 1}" } }, "required": ["id"] } }
                }), &["style"]),
                call: |args| post("/v1/music/jobs".into(), args.clone()),
            },
            Tool {
                name: "song_job_get",
                description: "A song job's status and progress; when completed it names the library song.",
                schema: || id_only("job_id", "from song_create or song_replay"),
                call: |args| get(format!("/v1/music/jobs/{}", segment(&text(args, "job_id")?))),
            },
            Tool {
                name: "song_jobs_list",
                description: "Song jobs queued or running now.",
                schema: nothing,
                call: |_| get("/v1/music/jobs".into()),
            },
            Tool {
                name: "song_job_cancel",
                description: "Stop a queued or running song job.",
                schema: || id_only("job_id", "the job to stop"),
                call: |args| post(format!("/v1/music/jobs/{}", segment(&text(args, "job_id")?)), json!({})),
            },
            Tool {
                name: "song_replay",
                description: "Render a library song again from its saved audio codes, bit for bit or with other steps, a new sound seed, a batch of variations, or another format - without composing again.",
                schema: || object(json!({ "song_id": { "type": "string" }, "steps": { "type": "integer" }, "seed": { "type": "integer" }, "synth_batch_size": { "type": "integer" }, "output_format": { "type": "string" }, "title": { "type": "string" } }), &["song_id"]),
                call: |args| post("/v1/music/replay".into(), args.clone()),
            },
            Tool {
                name: "score_compose",
                description: "Write only the score (ABC notation) for a style and lyrics, without singing it. Returns a job; poll score_job_get. Read and edit the score, then pass it to song_create as abc.",
                schema: || object(json!({ "style": { "type": "string" }, "lyrics": { "type": "string" }, "cot": { "type": "string", "enum": ["full", "melody"] }, "lm_seed": { "type": "integer" } }), &["style"]),
                call: |args| post("/v1/scores".into(), args.clone()),
            },
            Tool {
                name: "score_job_get",
                description: "A score job's status; when done it holds the ABC score. Works for score_compose and score_transcribe jobs.",
                schema: || id_only("job_id", "from score_compose or score_transcribe"),
                call: |args| get(format!("/v1/scores/{}", segment(&text(args, "job_id")?))),
            },
            Tool {
                name: "score_job_cancel",
                description: "Stop a score_compose or score_transcribe job.",
                schema: || id_only("job_id", "the job to stop"),
                call: |args| post(format!("/v1/scores/{}", segment(&text(args, "job_id")?)), json!({})),
            },
            Tool {
                name: "score_transcribe",
                description: "SheetSage2 listens to a recording and writes its melody (and chords) as a score, for a cover: pass song_id of a library track, or path of an audio file on this computer. melody_only leaves the chords out. Poll score_job_get.",
                schema: || object(json!({ "song_id": { "type": "string" }, "path": { "type": "string" }, "melody_only": { "type": "boolean" } }), &[]),
                call: |args| {
                    let mut fields = Vec::new();
                    let mut files = Vec::new();
                    if let Some(path) = args.get("path").and_then(Value::as_str).filter(|path| !path.trim().is_empty()) {
                        let path = PathBuf::from(path.trim());
                        files.push(("audio".to_string(), path.clone(), file_name(&path)));
                    } else {
                        fields.push(("song_id".to_string(), text(args, "song_id").map_err(|_| "song_id or path is required".to_string())?));
                    }
                    if args.get("melody_only").and_then(Value::as_bool).unwrap_or(false) {
                        fields.push(("melody_only".into(), "1".into()));
                    }
                    Ok(Call { method: Method::POST, path: "/v1/transcriptions".into(), payload: Payload::Form { fields, files } })
                },
            },
            // ---------------------------------------------------------------- how to write for the model
            Tool {
                name: "writing_guide",
                description: "How YuE2 wants to be written for - the rules the studio's own assistant follows. Read it before writing a style, lyrics or a score yourself. topic: song, style, lyrics, score, transcript, sections; none lists them.",
                schema: || object(json!({ "topic": { "type": "string", "enum": ["song", "style", "lyrics", "score", "transcript", "sections"] } }), &[]),
                call: |args| get(format!("/v1/writing/guide?topic={}", segment(args.get("topic").and_then(Value::as_str).unwrap_or_default()))),
            },
            Tool {
                name: "writing_examples",
                description: "The official YuE2 requests (style and lyrics) closest to a brief such as 'russian folk rock with accordion': write in their shape.",
                schema: || id_only("brief", "the idea, genre or mood"),
                call: |args| get(format!("/v1/writing/examples?brief={}", segment(&text(args, "brief")?))),
            },
            // ---------------------------------------------------------------- the writing assistant
            Tool {
                name: "assistant_write",
                description: "The studio's writing assistant. target: all (lyrics, style, title and cover prompt from an idea in description), lyrics (rewrite the lyrics to fit style), style (write the style for the lyrics), score (edit abc as instruction asks), transcript (lay out recognised words as a lyric sheet). Returns a draft to use in song_create.",
                schema: || object(json!({
                    "target": { "type": "string", "enum": ["all", "lyrics", "style", "score", "transcript"] },
                    "description": { "type": "string", "description": "The idea, or the text to work on" },
                    "instruction": { "type": "string" },
                    "lyrics": { "type": "string" },
                    "style": { "type": "string" },
                    "abc": { "type": "string" },
                    "duration_seconds": { "type": "number" }
                }), &["target"]),
                call: |args| post("/v1/assistant/write".into(), args.clone()),
            },
            Tool {
                name: "assistant_status",
                description: "Which writing assistant is set up (local model or OpenRouter) and whether it is running.",
                schema: nothing,
                call: |_| get("/v1/assistant/status".into()),
            },
            // ---------------------------------------------------------------- the library
            Tool {
                name: "library_songs_list",
                description: "The songs in the library, newest first: id, title, when made and the start of the style. query filters by title or style; library_song_get gives one song whole. response_format detailed gives every field of every song.",
                schema: || object(json!({ "query": { "type": "string" }, "response_format": { "type": "string", "enum": ["concise", "detailed"] } }), &[]),
                call: |_| get("/v1/library/songs".into()),
            },
            Tool {
                name: "library_song_get",
                description: "One library song with its request, lyrics, score and settings.",
                schema: || id_only("song_id", "library song id"),
                call: |args| get(format!("/v1/library/songs/{}", segment(&text(args, "song_id")?))),
            },
            Tool {
                name: "library_song_update",
                description: "Change a library song: send the song as library_song_get returns it with your changes (title, caption, lyrics, metadata).",
                schema: || object(json!({ "song_id": { "type": "string" }, "song": { "type": "object" } }), &["song_id", "song"]),
                call: |args| send(Method::PUT, format!("/v1/library/songs/{}", segment(&text(args, "song_id")?)), args.get("song").cloned().unwrap_or_default()),
            },
            Tool {
                name: "library_song_delete",
                description: "Remove a song from the library.",
                schema: || id_only("song_id", "library song id"),
                call: |args| send(Method::DELETE, format!("/v1/library/songs/{}", segment(&text(args, "song_id")?)), json!({})),
            },
            Tool {
                name: "library_import_audio",
                description: "Add an audio file from this computer to the library.",
                schema: || object(json!({ "path": { "type": "string" }, "title": { "type": "string" } }), &["path"]),
                call: |args| {
                    let path = PathBuf::from(text(args, "path")?);
                    let title = args.get("title").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| path.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default());
                    let name = file_name(&path);
                    Ok(Call { method: Method::POST, path: "/v1/library/import".into(), payload: Payload::Form { fields: vec![("title".into(), title)], files: vec![("audio".into(), path, name)] } })
                },
            },
            Tool {
                name: "playlist_list",
                description: "The library's playlists.",
                schema: nothing,
                call: |_| get("/v1/library/playlists".into()),
            },
            Tool {
                name: "playlist_create",
                description: "Make a playlist of library songs.",
                schema: || object(json!({ "name": { "type": "string" }, "description": { "type": "string" }, "song_ids": { "type": "array", "items": { "type": "string" } } }), &["name"]),
                call: |args| post("/v1/library/playlists".into(), args.clone()),
            },
            Tool {
                name: "playlist_update",
                description: "Rename a playlist or set its songs.",
                schema: || object(json!({ "playlist_id": { "type": "string" }, "name": { "type": "string" }, "description": { "type": "string" }, "song_ids": { "type": "array", "items": { "type": "string" } } }), &["playlist_id", "name"]),
                call: |args| send(Method::PUT, format!("/v1/library/playlists/{}", segment(&text(args, "playlist_id")?)), body_without(args, &["playlist_id"])),
            },
            Tool {
                name: "playlist_delete",
                description: "Remove a playlist; its songs stay in the library.",
                schema: || id_only("playlist_id", "playlist id"),
                call: |args| send(Method::DELETE, format!("/v1/library/playlists/{}", segment(&text(args, "playlist_id")?)), json!({})),
            },
            // ---------------------------------------------------------------- covers, karaoke, stems, processing
            Tool {
                name: "cover_draw",
                description: "Draw a library song's cover now, from its cover prompt or the cover template.",
                schema: || id_only("song_id", "library song id"),
                call: |args| post(format!("/v1/library/songs/{}/cover/auto", segment(&text(args, "song_id")?)), json!({})),
            },
            Tool {
                name: "cover_templates_get",
                description: "The cover prompt templates, with {title}, {style} and {excerpt} placeholders.",
                schema: nothing,
                call: |_| get("/v1/cover-templates".into()),
            },
            Tool {
                name: "cover_templates_set",
                description: "Save the cover templates (the object as cover_templates_get returns it, with your changes).",
                schema: || json!({ "type": "object" }),
                call: |args| send(Method::PUT, "/v1/cover-templates".into(), args.clone()),
            },
            Tool {
                name: "karaoke_make",
                description: "Time every word of a library song's lyrics against its vocals (enhanced LRC). language helps the recogniser.",
                schema: || object(json!({ "song_id": { "type": "string" }, "language": { "type": "string" } }), &["song_id"]),
                call: |args| post(format!("/v1/library/songs/{}/karaoke", segment(&text(args, "song_id")?)), body_without(args, &["song_id"])),
            },
            Tool {
                name: "stems_split",
                description: "Split a library song into six stems (drums, bass, other, vocals, guitar, piano) with HT-Demucs. Poll stems_get.",
                schema: || id_only("song_id", "library song id"),
                call: |args| post(format!("/v1/library/songs/{}/stems", segment(&text(args, "song_id")?)), json!({})),
            },
            Tool {
                name: "stems_get",
                description: "The stems of a library song and the progress of a split.",
                schema: || id_only("song_id", "library song id"),
                call: |args| get(format!("/v1/library/songs/{}/stems", segment(&text(args, "song_id")?))),
            },
            Tool {
                name: "processing_start",
                description: "Run audio processing on a library song: any of denoise, lifter (Spectral Lifter), naturalize (vocal naturaliser), vst (plugin chain), master (to a reference). Each is an object of its settings; processing_get shows the result, then processing_keep or processing_discard.",
                schema: || object(json!({ "song_id": { "type": "string" }, "denoise": { "type": "object" }, "lifter": { "type": "object" }, "naturalize": { "type": "object" }, "vst": { "type": "array" }, "master": { "type": "object" } }), &["song_id"]),
                call: |args| post(format!("/v1/library/songs/{}/process", segment(&text(args, "song_id")?)), body_without(args, &["song_id"])),
            },
            Tool {
                name: "processing_get",
                description: "The processing at work or its finished result, waiting to be kept or discarded.",
                schema: nothing,
                call: |_| get("/v1/processing".into()),
            },
            Tool {
                name: "processing_keep",
                description: "Keep the processed audio as a new version of the song.",
                schema: nothing,
                call: |_| post("/v1/processing/keep".into(), json!({})),
            },
            Tool {
                name: "processing_discard",
                description: "Throw the processed audio away.",
                schema: nothing,
                call: |_| post("/v1/processing/discard".into(), json!({})),
            },
            // ---------------------------------------------------------------- LoRA
            Tool {
                name: "lora_list",
                description: "Installed LoRA and the catalogue of ready ones: ids, names, trigger words, slots and default strengths, download progress.",
                schema: nothing,
                call: |_| get("/v1/adapters".into()),
            },
            Tool {
                name: "lora_install_catalog",
                description: "Download LoRA from the studio's catalogue by id.",
                schema: || object(json!({ "ids": { "type": "array", "items": { "type": "string" } } }), &["ids"]),
                call: |args| post("/v1/adapters/install".into(), args.clone()),
            },
            Tool {
                name: "lora_search_hf",
                description: "Search Hugging Face for LoRA for this model.",
                schema: || object(json!({ "q": { "type": "string" } }), &[]),
                call: |args| get(format!("/v1/adapters/hub?q={}", segment(args.get("q").and_then(Value::as_str).unwrap_or_default()))),
            },
            Tool {
                name: "lora_hf_files",
                description: "The adapter files in one Hugging Face repository.",
                schema: || id_only("repo", "owner/name"),
                call: |args| get(format!("/v1/adapters/hub/files?repo={}", segment(&text(args, "repo")?))),
            },
            Tool {
                name: "lora_install_hf",
                description: "Download adapter files from a Hugging Face repository into the studio.",
                schema: || object(json!({ "repo": { "type": "string" }, "paths": { "type": "array", "items": { "type": "string" } } }), &["repo", "paths"]),
                call: |args| post("/v1/adapters/hub/install".into(), args.clone()),
            },
            Tool {
                name: "lora_update",
                description: "Rename an installed LoRA, set its trigger word or its default strengths per slot.",
                schema: || object(json!({ "lora_id": { "type": "string" }, "name": { "type": "string" }, "trigger": { "type": "string" }, "scales": { "type": "object" } }), &["lora_id"]),
                call: |args| send(Method::PATCH, format!("/v1/adapters/{}", segment(&text(args, "lora_id")?)), body_without(args, &["lora_id"])),
            },
            Tool {
                name: "lora_delete",
                description: "Remove an installed LoRA.",
                schema: || id_only("lora_id", "installed LoRA id"),
                call: |args| send(Method::DELETE, format!("/v1/adapters/{}", segment(&text(args, "lora_id")?)), json!({})),
            },
            // ---------------------------------------------------------------- everything else the page does
            Tool {
                name: "models_cancel_download",
                description: "Stop the model download at work.",
                schema: nothing,
                call: |_| post("/setup/cancel".into(), json!({})),
            },
            Tool {
                name: "models_remove",
                description: "Delete downloaded model files by component id (see models_catalog).",
                schema: || object(json!({ "ids": { "type": "array", "items": { "type": "string" } } }), &["ids"]),
                call: |args| post("/setup/remove".into(), args.clone()),
            },
            Tool {
                name: "engine_logs",
                description: "The music engine's recent log, for when a song fails or the engine will not start.",
                schema: nothing,
                call: |_| get("/v1/engine/logs".into()),
            },
            Tool {
                name: "studio_capabilities",
                description: "What the studio can do with the installed models and providers, and which provider runs each capability.",
                schema: nothing,
                call: |_| get("/v1/capabilities".into()),
            },
            Tool {
                name: "settings_get",
                description: "The studio's settings: which provider (local or OpenRouter) runs music, lyrics timing, the assistant and covers, and their models.",
                schema: nothing,
                call: |_| get("/v1/configuration".into()),
            },
            Tool {
                name: "settings_set",
                description: "Save the studio's settings: the object as settings_get returns it, with your changes.",
                schema: || json!({ "type": "object" }),
                call: |args| send(Method::PUT, "/v1/configuration".into(), args.clone()),
            },
            Tool {
                name: "studio_open_data_folder",
                description: "Open the studio's data folder (models, songs, settings) in the file explorer for the user.",
                schema: nothing,
                call: |_| post("/v1/open-data-directory".into(), json!({})),
            },
            Tool {
                name: "assistant_set",
                description: "Choose the writing assistant: provider none, local (an OpenAI-compatible server at local_base_url with local_model), managed (a model the studio runs: managed_model, or a GGUF at managed_path) or openrouter (openrouter_model); reasoning_effort off, minimal, low, medium or high.",
                schema: || object(json!({ "provider": { "type": "string", "enum": ["none", "local", "managed", "openrouter"] }, "local_base_url": { "type": "string" }, "local_model": { "type": "string" }, "managed_model": { "type": "string" }, "managed_path": { "type": "string" }, "openrouter_model": { "type": "string" }, "reasoning_effort": { "type": "string" } }), &[]),
                call: |args| send(Method::PUT, "/v1/assistant/status".into(), args.clone()),
            },
            Tool {
                name: "assistant_local_models",
                description: "The models an OpenAI-compatible local server (LM Studio, Ollama...) at base offers.",
                schema: || id_only("base", "the server's base URL, e.g. http://127.0.0.1:1234/v1"),
                call: |args| get(format!("/v1/assistant/local-models?base={}", segment(&text(args, "base")?))),
            },
            Tool {
                name: "assistant_runtime",
                description: "The assistant models the studio can run itself: what is downloaded, what is running, download progress.",
                schema: nothing,
                call: |_| get("/v1/assistant/runtime".into()),
            },
            Tool {
                name: "assistant_model_install",
                description: "Download a managed assistant model or its runtime (asset_id and model_id from assistant_runtime).",
                schema: || object(json!({ "asset_id": { "type": "string" }, "model_id": { "type": "string" } }), &["asset_id"]),
                call: |args| post("/v1/assistant/runtime/install".into(), args.clone()),
            },
            Tool {
                name: "assistant_model_remove",
                description: "Delete a downloaded assistant model or runtime.",
                schema: || object(json!({ "asset_id": { "type": "string" }, "model_id": { "type": "string" } }), &["asset_id"]),
                call: |args| post("/v1/assistant/runtime/remove".into(), args.clone()),
            },
            Tool {
                name: "assistant_cancel_download",
                description: "Stop the assistant model download at work.",
                schema: nothing,
                call: |_| post("/v1/assistant/runtime/cancel".into(), json!({})),
            },
            Tool {
                name: "assistant_start",
                description: "Load a managed assistant model on the card now (it also starts by itself on first use).",
                schema: || object(json!({ "model_id": { "type": "string" }, "model_path": { "type": "string" } }), &["model_id"]),
                call: |args| post("/v1/assistant/runtime/start".into(), args.clone()),
            },
            Tool {
                name: "assistant_stop",
                description: "Unload the managed assistant model and free its video memory.",
                schema: nothing,
                call: |_| post("/v1/assistant/runtime/stop".into(), json!({})),
            },
            Tool {
                name: "karaoke_settings_get",
                description: "Lyrics timing and recognition: which recogniser (parakeet, whisper, openrouter), its model, on card or processor, what is downloaded.",
                schema: nothing,
                call: |_| get("/v1/karaoke/status".into()),
            },
            Tool {
                name: "karaoke_settings_set",
                description: "Choose the recogniser used for karaoke and for dataset lyrics: enabled, provider (parakeet, whisper, openrouter), whisper_model, openrouter_model, runtime (cpu, cuda).",
                schema: || object(json!({ "enabled": { "type": "boolean" }, "provider": { "type": "string" }, "whisper_model": { "type": "string" }, "openrouter_model": { "type": "string" }, "runtime": { "type": "string" } }), &[]),
                call: |args| send(Method::PUT, "/v1/karaoke/status".into(), args.clone()),
            },
            Tool {
                name: "recogniser_install",
                description: "Download a speech recogniser or its runtime (asset_id and model_id from karaoke_settings_get).",
                schema: || object(json!({ "asset_id": { "type": "string" }, "model_id": { "type": "string" } }), &["asset_id"]),
                call: |args| post("/v1/karaoke/install".into(), args.clone()),
            },
            Tool {
                name: "recogniser_remove",
                description: "Delete a downloaded speech recogniser or runtime.",
                schema: || object(json!({ "asset_id": { "type": "string" }, "model_id": { "type": "string" } }), &["asset_id"]),
                call: |args| post("/v1/karaoke/remove".into(), args.clone()),
            },
            Tool {
                name: "recogniser_cancel_download",
                description: "Stop the recogniser download at work.",
                schema: nothing,
                call: |_| post("/v1/karaoke/cancel".into(), json!({})),
            },
            Tool {
                name: "karaoke_delete",
                description: "Remove a library song's karaoke timings.",
                schema: || id_only("song_id", "library song id"),
                call: |args| send(Method::DELETE, format!("/v1/library/songs/{}/karaoke", segment(&text(args, "song_id")?)), json!({})),
            },
            Tool {
                name: "separator_status",
                description: "The stem separator: whether its model and runtime are installed, and download progress.",
                schema: nothing,
                call: |_| get("/v1/separation/status".into()),
            },
            Tool {
                name: "separator_runtime",
                description: "The ONNX runtimes the separator can use (card or processor) and what is downloaded.",
                schema: nothing,
                call: |_| get("/v1/separation/runtime".into()),
            },
            Tool {
                name: "separator_runtime_install",
                description: "Download a runtime for the separator (asset_id from separator_runtime).",
                schema: || id_only("asset_id", "runtime asset id"),
                call: |args| post("/v1/separation/runtime/install".into(), args.clone()),
            },
            Tool {
                name: "separator_cancel_download",
                description: "Stop the separator runtime download at work.",
                schema: nothing,
                call: |_| post("/v1/separation/runtime/cancel".into(), json!({})),
            },
            Tool {
                name: "separator_install",
                description: "Download the HT-Demucs stem model.",
                schema: nothing,
                call: |_| post("/v1/separation/install".into(), json!({})),
            },
            Tool {
                name: "separator_remove",
                description: "Delete the HT-Demucs stem model.",
                schema: nothing,
                call: |_| post("/v1/separation/remove".into(), json!({})),
            },
            Tool {
                name: "separator_settings_get",
                description: "Separator settings: runtime (card or processor) and overlap.",
                schema: nothing,
                call: |_| get("/v1/separation/settings".into()),
            },
            Tool {
                name: "separator_settings_set",
                description: "Save separator settings: the object as separator_settings_get returns it, with your changes.",
                schema: || json!({ "type": "object" }),
                call: |args| send(Method::PUT, "/v1/separation/settings".into(), args.clone()),
            },
            Tool {
                name: "library_song_files",
                description: "Where a library song's files are on this computer: its audio, its cover and each stem; read or open them directly.",
                schema: || id_only("song_id", "library song id"),
                call: |args| get(format!("/v1/library/songs/{}/files", segment(&text(args, "song_id")?))),
            },
            Tool {
                name: "cover_set_from_file",
                description: "Make an image file on this computer (PNG, JPEG, WebP) a library song's cover.",
                schema: || object(json!({ "song_id": { "type": "string" }, "path": { "type": "string" } }), &["song_id", "path"]),
                call: |args| {
                    use base64::Engine;
                    let path = PathBuf::from(text(args, "path")?);
                    let bytes = std::fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
                    let media_type = match path.extension().and_then(|value| value.to_str()).unwrap_or_default().to_lowercase().as_str() {
                        "jpg" | "jpeg" => "image/jpeg",
                        "webp" => "image/webp",
                        _ => "image/png",
                    };
                    send(Method::PUT, format!("/v1/library/songs/{}/cover", segment(&text(args, "song_id")?)), json!({ "image_base64": base64::engine::general_purpose::STANDARD.encode(bytes), "media_type": media_type }))
                },
            },
            Tool {
                name: "library_version_select",
                description: "Make one of a song's audio versions (the original, a processed one) the one it plays.",
                schema: || object(json!({ "song_id": { "type": "string" }, "version": { "type": "string" } }), &["song_id", "version"]),
                call: |args| send(Method::PUT, format!("/v1/library/songs/{}/version", segment(&text(args, "song_id")?)), json!({ "version": text(args, "version")? })),
            },
            Tool {
                name: "library_version_delete",
                description: "Remove one audio version of a song.",
                schema: || object(json!({ "song_id": { "type": "string" }, "version": { "type": "string" } }), &["song_id", "version"]),
                call: |args| send(Method::DELETE, format!("/v1/library/songs/{}/versions/{}", segment(&text(args, "song_id")?), segment(&text(args, "version")?)), json!({})),
            },
            Tool {
                name: "cover_prompt_render",
                description: "Fill a cover template for a song, to see the prompt a cover would be drawn from.",
                schema: || object(json!({ "template": { "type": "string" }, "song_id": { "type": "string" }, "title": { "type": "string" } }), &["template"]),
                call: |args| post("/v1/cover-templates/render".into(), args.clone()),
            },
            Tool {
                name: "openrouter_status",
                description: "Whether an OpenRouter key is set, and where it comes from.",
                schema: nothing,
                call: |_| get("/v1/openrouter/settings".into()),
            },
            Tool {
                name: "openrouter_set_key",
                description: "Store the user's OpenRouter API key (empty to remove it).",
                schema: || object(json!({ "api_key": { "type": "string" } }), &["api_key"]),
                call: |args| send(Method::PUT, "/v1/openrouter/settings".into(), args.clone()),
            },
            Tool {
                name: "openrouter_catalog",
                description: "OpenRouter's live model catalogue, by capability: music, transcription, text, images.",
                schema: nothing,
                call: |_| get("/v1/openrouter/catalog".into()),
            },
            Tool {
                name: "openrouter_catalog_refresh",
                description: "Fetch OpenRouter's model catalogue again.",
                schema: nothing,
                call: |_| post("/v1/openrouter/catalog/refresh".into(), json!({})),
            },
            Tool {
                name: "openrouter_log",
                description: "What was asked of OpenRouter and what came back, newest last.",
                schema: nothing,
                call: |_| get("/v1/openrouter/logs".into()),
            },
            Tool {
                name: "openrouter_complete",
                description: "Ask an OpenRouter text model a prompt.",
                schema: || object(json!({ "model_id": { "type": "string" }, "prompt": { "type": "string" } }), &["model_id", "prompt"]),
                call: |args| post("/v1/openrouter/completions".into(), args.clone()),
            },
            Tool {
                name: "openrouter_cover",
                description: "Draw an image with an OpenRouter image model from a prompt.",
                schema: || object(json!({ "model_id": { "type": "string" }, "prompt": { "type": "string" } }), &["model_id", "prompt"]),
                call: |args| post("/v1/openrouter/covers".into(), args.clone()),
            },
            Tool {
                name: "openrouter_transcribe",
                description: "Transcribe an audio file on this computer with an OpenRouter speech model.",
                schema: || object(json!({ "model_id": { "type": "string" }, "path": { "type": "string" }, "language": { "type": "string" } }), &["model_id", "path"]),
                call: |args| {
                    use base64::Engine;
                    let path = PathBuf::from(text(args, "path")?);
                    let bytes = std::fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
                    let format = path.extension().and_then(|value| value.to_str()).unwrap_or("wav").to_lowercase();
                    post("/v1/openrouter/transcriptions".into(), json!({ "model_id": text(args, "model_id")?, "audio_base64": base64::engine::general_purpose::STANDARD.encode(bytes), "audio_format": format, "language": args.get("language") }))
                },
            },
            Tool {
                name: "processing_set_reference",
                description: "Use an audio file on this computer as the reference processing_start masters to.",
                schema: || id_only("path", "reference audio file"),
                call: |args| {
                    let path = PathBuf::from(text(args, "path")?);
                    let name = file_name(&path);
                    Ok(Call { method: Method::POST, path: "/v1/processing/reference".into(), payload: Payload::Form { fields: Vec::new(), files: vec![("audio".into(), path, name)] } })
                },
            },
            Tool {
                name: "vst_list",
                description: "The VST3 plugins the studio found, for processing_start's vst chain.",
                schema: nothing,
                call: |_| get("/v1/processing/vst".into()),
            },
            Tool {
                name: "vst_scan",
                description: "Look for VST3 plugins again.",
                schema: nothing,
                call: |_| post("/v1/processing/vst/scan".into(), json!({})),
            },
            Tool {
                name: "vst_open_editor",
                description: "Open a VST3 plugin's own window for the user to set it; state_id keeps its settings for the chain.",
                schema: || object(json!({ "path": { "type": "string" }, "state_id": { "type": "string" } }), &["path"]),
                call: |args| post("/v1/processing/vst/editor".into(), args.clone()),
            },
            Tool {
                name: "lora_import_files",
                description: "Install LoRA files from this computer (.safetensors, with adapter_config.json or lora.json beside them when there are).",
                schema: || object(json!({ "name": { "type": "string" }, "paths": { "type": "array", "items": { "type": "string" } } }), &["name", "paths"]),
                call: |args| {
                    let paths: Vec<PathBuf> = args.get("paths").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(PathBuf::from).collect();
                    if paths.is_empty() {
                        return Err("'paths' is required".into());
                    }
                    let files = paths.into_iter().map(|path| { let name = file_name(&path); ("files".to_string(), path, name) }).collect();
                    Ok(Call { method: Method::POST, path: "/v1/adapters/import".into(), payload: Payload::Form { fields: vec![("name".into(), text(args, "name")?)], files } })
                },
            },
            Tool {
                name: "lora_cancel_download",
                description: "Stop the LoRA download at work.",
                schema: nothing,
                call: |_| post("/v1/adapters/cancel".into(), json!({})),
            },
            Tool {
                name: "dataset_import_folder",
                description: "Take a dataset folder another studio of the family wrote (its dataset.json and WAV files).",
                schema: || id_only("path", "the dataset folder"),
                call: |args| {
                    let folder = PathBuf::from(text(args, "path")?);
                    let mut files = Vec::new();
                    let mut stack = vec![folder.clone()];
                    while let Some(dir) = stack.pop() {
                        for entry in std::fs::read_dir(&dir).map_err(|error| format!("read {}: {error}", dir.display()))?.flatten() {
                            let path = entry.path();
                            if path.is_dir() {
                                stack.push(path);
                            } else if path.file_name().is_some_and(|name| name == "dataset.json") || path.extension().is_some_and(|value| value.eq_ignore_ascii_case("wav")) {
                                let relative = path.strip_prefix(folder.parent().unwrap_or(&folder)).unwrap_or(&path).to_string_lossy().replace('\\', "/");
                                files.push(("files".to_string(), path, relative));
                            }
                        }
                    }
                    Ok(Call { method: Method::POST, path: "/v1/training/datasets/import".into(), payload: Payload::Form { fields: Vec::new(), files } })
                },
            },
            Tool {
                name: "dataset_reveal",
                description: "Open a dataset's folder in the file explorer for the user.",
                schema: || id_only("dataset_id", "dataset id"),
                call: |args| post(format!("/v1/training/datasets/{}/reveal", segment(&text(args, "dataset_id")?)), json!({})),
            },
            Tool {
                name: "dataset_song_files",
                description: "Where a dataset song's audio and its separated vocals are on this computer.",
                schema: || object(json!({ "dataset_id": { "type": "string" }, "song_id": { "type": "string" } }), &["dataset_id", "song_id"]),
                call: |args| get(format!("/v1/training/datasets/{}/items/{}/files", segment(&text(args, "dataset_id")?), segment(&text(args, "song_id")?))),
            },
            Tool {
                name: "training_pack_cancel",
                description: "Stop the trainer download at work.",
                schema: nothing,
                call: |_| post("/v1/training/pack/cancel".into(), json!({})),
            },
            Tool {
                name: "dataset_prepare_train_after",
                description: "For the preparation at work: start this training run once every song is ready (train: {name, recipe}), or not (train: null).",
                schema: || object(json!({ "train": { "type": ["object", "null"] } }), &[]),
                call: |args| post("/v1/training/prepare/train-after".into(), args.clone()),
            },
            // ---------------------------------------------------------------- training
            Tool {
                name: "training_status",
                description: "Training at a glance: every dataset with its song count and how many songs are ready, every run with its status, stage, last step, loss, KL and checkpoints, the preparation at work, whether the trainer and the listening pack are installed, and the recipe defaults to start a run from. response_format detailed gives everything, recipe fields included.",
                schema: || object(json!({ "response_format": { "type": "string", "enum": ["concise", "detailed"] } }), &[]),
                call: |_| get("/v1/training".into()),
            },
            Tool {
                name: "dataset_get",
                description: "One dataset with every song: id, title, artist, lyrics, style, lyrics_state (wanted, found, done) and style_state (wanted, heard, done), lyrics_source, heard (what MOSS heard: genre, caption, bpm) and length. Give song_id for one song only.",
                schema: || object(json!({ "dataset_id": { "type": "string" }, "song_id": { "type": "string" } }), &["dataset_id"]),
                call: |_| get("/v1/training".into()),
            },
            Tool {
                name: "training_pack_install",
                description: "Download the trainer and its weights (needed once before training).",
                schema: nothing,
                call: |_| post("/v1/training/pack/install".into(), json!({})),
            },
            Tool {
                name: "training_listen_pack_install",
                description: "Download MOSS-Music and the tempo model, which describe songs by ear during preparation.",
                schema: nothing,
                call: |_| post("/v1/training/listen/install".into(), json!({})),
            },
            Tool {
                name: "dataset_create",
                description: "Make an empty dataset. The trigger word is made from the name when left out.",
                schema: || object(json!({ "name": { "type": "string" }, "trigger": { "type": "string" } }), &["name"]),
                call: |args| post("/v1/training/datasets".into(), args.clone()),
            },
            Tool {
                name: "dataset_add_folder",
                description: "Add every song under a folder on this computer to a dataset, as a dropped folder: audio, .txt/.lrc lyrics beside it, albums cut by their .cue. Titles and artists come from tags, file names and folders.",
                schema: || object(json!({ "dataset_id": { "type": "string" }, "path": { "type": "string", "description": "folder or single audio file" } }), &["dataset_id", "path"]),
                call: |args| {
                    let path = PathBuf::from(text(args, "path")?);
                    let files = if path.is_dir() { folder_files(&path)? } else { vec![("files".to_string(), path.clone(), file_name(&path))] };
                    Ok(Call { method: Method::POST, path: format!("/v1/training/datasets/{}/files", segment(&text(args, "dataset_id")?)), payload: Payload::Form { fields: Vec::new(), files } })
                },
            },
            Tool {
                name: "dataset_add_library_songs",
                description: "Add library songs to a dataset.",
                schema: || object(json!({ "dataset_id": { "type": "string" }, "song_ids": { "type": "array", "items": { "type": "string" } } }), &["dataset_id", "song_ids"]),
                call: |args| post(format!("/v1/training/datasets/{}/songs", segment(&text(args, "dataset_id")?)), body_without(args, &["dataset_id"])),
            },
            Tool {
                name: "dataset_update",
                description: "Rename a dataset or change its trigger word.",
                schema: || object(json!({ "dataset_id": { "type": "string" }, "name": { "type": "string" }, "trigger": { "type": "string" } }), &["dataset_id"]),
                call: |args| send(Method::PATCH, format!("/v1/training/datasets/{}", segment(&text(args, "dataset_id")?)), body_without(args, &["dataset_id"])),
            },
            Tool {
                name: "dataset_delete",
                description: "Remove a dataset and its audio.",
                schema: || id_only("dataset_id", "dataset id"),
                call: |args| send(Method::DELETE, format!("/v1/training/datasets/{}", segment(&text(args, "dataset_id")?)), json!({})),
            },
            Tool {
                name: "dataset_song_update",
                description: "Write a dataset song's title, artist, style, lyrics (sections tagged [Verse 1], [Chorus]...) or instrumental flag. What you write is final: preparation leaves it alone.",
                schema: || object(json!({ "dataset_id": { "type": "string" }, "song_id": { "type": "string" }, "title": { "type": "string" }, "artist": { "type": "string" }, "style": { "type": "string" }, "lyrics": { "type": "string" }, "instrumental": { "type": "boolean" } }), &["dataset_id", "song_id"]),
                call: |args| send(Method::PATCH, format!("/v1/training/datasets/{}/items/{}", segment(&text(args, "dataset_id")?), segment(&text(args, "song_id")?)), body_without(args, &["dataset_id", "song_id"])),
            },
            Tool {
                name: "dataset_song_delete",
                description: "Remove a song from a dataset.",
                schema: || object(json!({ "dataset_id": { "type": "string" }, "song_id": { "type": "string" } }), &["dataset_id", "song_id"]),
                call: |args| send(Method::DELETE, format!("/v1/training/datasets/{}/items/{}", segment(&text(args, "dataset_id")?), segment(&text(args, "song_id")?)), json!({})),
            },
            Tool {
                name: "lyrics_find",
                description: "Look a song's published lyrics up in LRCLIB, QQ Music and Kugou by artist, title and length; returns the plain and timed lyrics word for word, or that it is an instrumental, or nothing.",
                schema: || object(json!({ "artist": { "type": "string" }, "title": { "type": "string" }, "seconds": { "type": "number" } }), &["title"]),
                call: |args| post("/v1/lyrics/find".into(), args.clone()),
            },
            Tool {
                name: "dataset_prepare",
                description: "Fill a dataset in by itself: lyrics from the lyric databases, Whisper only for songs they miss, sections laid out by the assistant, styles written from what MOSS-Music hears. lyrics and style: missing (only what is not done yet), all (again, for the chosen songs) or none. items: song ids, all songs when left out. train: start this run once every song is ready. Watch training_status.",
                schema: || object(json!({
                    "dataset_id": { "type": "string" },
                    "items": { "type": "array", "items": { "type": "string" } },
                    "lyrics": { "type": "string", "enum": ["none", "missing", "all"] },
                    "style": { "type": "string", "enum": ["none", "missing", "all"] },
                    "language": { "type": "string", "description": "sung language code when known, e.g. ru" },
                    "train": { "type": "object", "properties": { "name": { "type": "string" }, "recipe": { "type": "object" } } },
                    "writer": { "type": "string", "enum": ["studio", "agent"], "description": "agent: the studio finds, recognises and listens, and leaves the lyric layout and the styles to you - songs stay lyrics_state 'found' (lyrics as found, lyrics_source says from where) and style_state 'heard' (heard: genre, caption, bpm); write them with dataset_song_update, following writing_guide" }
                }), &["dataset_id"]),
                call: |args| post(format!("/v1/training/datasets/{}/prepare", segment(&text(args, "dataset_id")?)), body_without(args, &["dataset_id"])),
            },
            Tool {
                name: "dataset_prepare_cancel",
                description: "Stop the preparation at work; every song keeps what it has.",
                schema: nothing,
                call: |_| post("/v1/training/prepare/cancel".into(), json!({})),
            },
            Tool {
                name: "training_start",
                description: "Train a LoRA on a dataset. recipe: the recipe from training_status (recipe_defaults) with your changes, e.g. stop 'kl' with target_kl 1.4, or stop 'epochs' with epochs. Watch training_status; one run at a time, and it holds the card.",
                schema: || object(json!({ "dataset_id": { "type": "string" }, "name": { "type": "string" }, "recipe": { "type": "object" } }), &["dataset_id", "recipe"]),
                call: |args| post("/v1/training/runs".into(), args.clone()),
            },
            Tool {
                name: "training_cancel",
                description: "Stop a training run; its saved checkpoints stay.",
                schema: || id_only("run_id", "training run id"),
                call: |args| post(format!("/v1/training/runs/{}/cancel", segment(&text(args, "run_id")?)), json!({})),
            },
            Tool {
                name: "training_checkpoint_install",
                description: "Put a run's checkpoint (its step) into the LoRA library, ready to use in song_create.",
                schema: || object(json!({ "run_id": { "type": "string" }, "step": { "type": "integer" }, "name": { "type": "string" } }), &["run_id", "step"]),
                call: |args| {
                    let step = args.get("step").and_then(Value::as_u64).ok_or("'step' is required")?;
                    post(format!("/v1/training/runs/{}/checkpoints/{step}/install", segment(&text(args, "run_id")?)), body_without(args, &["run_id", "step"]))
                },
            },
            Tool {
                name: "training_run_delete",
                description: "Remove a finished training run and its checkpoints.",
                schema: || id_only("run_id", "training run id"),
                call: |args| send(Method::DELETE, format!("/v1/training/runs/{}", segment(&text(args, "run_id")?)), json!({})),
            },
        ]
    })
}

/// Builds the route's request, calls it inside the process and returns what
/// it answered, cut to a size an agent reads.
async fn call_route(call: Call) -> Result<(StatusCode, String), String> {
    let api = API.get().ok_or("the studio is still starting")?.clone();
    let builder = Request::builder().method(call.method).uri(&call.path);
    let request = match call.payload {
        Payload::None | Payload::Window { .. } => builder.body(Body::empty()),
        Payload::Json(body) => builder.header(header::CONTENT_TYPE, "application/json").body(Body::from(body.to_string())),
        Payload::Form { fields, files } => {
            let boundary = format!("studio-mcp-{}", uuid::Uuid::now_v7().simple());
            let mut body: Vec<u8> = Vec::new();
            for (name, value) in fields {
                body.extend(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes());
            }
            for (name, path, file) in files {
                let bytes = tokio::fs::read(&path).await.map_err(|error| format!("read {}: {error}", path.display()))?;
                body.extend(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"; filename=\"{}\"\r\nContent-Type: application/octet-stream\r\n\r\n", file.replace('"', "'")).as_bytes());
                body.extend(bytes);
                body.extend(b"\r\n");
            }
            body.extend(format!("--{boundary}--\r\n").as_bytes());
            builder.header(header::CONTENT_TYPE, format!("multipart/form-data; boundary={boundary}")).body(Body::from(body))
        }
    }
    .map_err(|error| error.to_string())?;
    let response = api.oneshot(request).await.map_err(|error| error.to_string())?;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024 * 1024).await.map_err(|error| error.to_string())?;
    let mut text = match serde_json::from_slice::<Value>(&bytes) {
        Ok(value) => serde_json::to_string(&value).unwrap_or_default(),
        Err(_) => String::from_utf8_lossy(&bytes).into_owned(),
    };
    if text.trim().is_empty() {
        text = if status.is_success() { "Done.".into() } else { status.to_string() };
    }
    Ok((status, text))
}

/// Cuts an answer to what an agent reads in one go, and says how to get the rest.
fn cut(mut text: String) -> String {
    if text.len() > LIMIT {
        let mut end = LIMIT;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("\n... (cut: ask for one item, e.g. library_song_get or dataset_get with song_id)");
    }
    text
}

fn rpc(id: Value, result: Value) -> Response {
    Json(json!({ "jsonrpc": "2.0", "id": id, "result": result })).into_response()
}

fn rpc_error(id: Value, code: i64, message: String) -> Response {
    Json(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })).into_response()
}

pub async fn handle(body: axum::body::Bytes) -> Response {
    let Ok(message) = serde_json::from_slice::<Value>(&body) else {
        return rpc_error(Value::Null, -32700, "Parse error".into());
    };
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    let method = message.get("method").and_then(Value::as_str).unwrap_or_default();
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    match method {
        "initialize" => {
            let protocol = params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL);
            rpc(id, json!({
                "protocolVersion": protocol,
                "capabilities": { "tools": {}, "resources": {}, "prompts": {} },
                "serverInfo": { "name": env!("CARGO_PKG_NAME"), "version": env!("CARGO_PKG_VERSION") },
                "instructions": INSTRUCTIONS,
            }))
        }
        "notifications/initialized" | "notifications/cancelled" => StatusCode::ACCEPTED.into_response(),
        "ping" => rpc(id, json!({})),
        "resources/list" => {
            let mut list = vec![json!({ "uri": SKILL_URI, "name": "studio skill", "description": "How to drive the studio: every tool, what the model reads, step-by-step recipes.", "mimeType": "text/markdown" })];
            for (topic, about) in crate::assistant::GUIDE_TOPICS {
                list.push(json!({ "uri": format!("{GUIDE_URI}{topic}"), "name": format!("writing guide: {topic}"), "description": about, "mimeType": "text/plain" }));
            }
            rpc(id, json!({ "resources": list }))
        }
        "resources/read" => {
            let uri = params.get("uri").and_then(Value::as_str).unwrap_or_default();
            let text = if uri == SKILL_URI { Some(SKILL.to_string()) } else { uri.strip_prefix(GUIDE_URI).and_then(crate::assistant::writing_guide) };
            match text {
                Some(text) => rpc(id, json!({ "contents": [{ "uri": uri, "mimeType": if uri == SKILL_URI { "text/markdown" } else { "text/plain" }, "text": text }] })),
                None => rpc_error(id, -32002, format!("Resource not found: {uri}")),
            }
        }
        "prompts/list" => rpc(id, json!({ "prompts": [
            { "name": "studio", "description": "Load the studio's skill: the tools, what the model reads, and recipes." },
            { "name": "write_song", "description": "Write a song for the model and make it.", "arguments": [{ "name": "idea", "description": "what the song is about, its genre and mood", "required": true }] },
        ] })),
        "prompts/get" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
            let idea = params.get("arguments").and_then(|arguments| arguments.get("idea")).and_then(Value::as_str).unwrap_or_default();
            let text = match name {
                "studio" => Some(SKILL.to_string()),
                "write_song" => crate::assistant::writing_guide("song").map(|guide| format!("{guide}\n\nWrite a song about: {idea}\nFind the closest official examples with writing_examples, write the style and lyrics by the rules above, then song_create and studio_wait.")),
                _ => None,
            };
            match text {
                Some(text) => rpc(id, json!({ "messages": [{ "role": "user", "content": { "type": "text", "text": text } }] })),
                None => rpc_error(id, -32602, format!("Unknown prompt: {name}")),
            }
        }
        "tools/list" => {
            let list: Vec<Value> = tools().iter().map(|tool| json!({ "name": tool.name, "description": tool.description, "inputSchema": (tool.schema)(), "annotations": annotations(tool.name) })).collect();
            rpc(id, json!({ "tools": list }))
        }
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
            let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            let answer = |text: String, error: bool| rpc(id.clone(), json!({ "content": [{ "type": "text", "text": text }], "isError": error }));
            let Some(tool) = tools().iter().find(|tool| tool.name == name) else {
                return answer(format!("Unknown tool: {name}"), true);
            };
            match (tool.call)(&args) {
                Err(problem) => answer(problem, true),
                Ok(Call { payload: Payload::Window { command, args, seconds }, .. }) => match ask_window(command, args, seconds).await {
                    Ok(result) => {
                        // a screenshot comes back as an image the agent sees
                        let mut content = Vec::new();
                        if let Some(image) = result.get("image").and_then(Value::as_str) {
                            content.push(json!({ "type": "image", "data": image, "mimeType": "image/png" }));
                        }
                        let text = result.get("text").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| if result.is_null() || result.get("image").is_some() { "Done.".into() } else { serde_json::to_string_pretty(&result).unwrap_or_default() });
                        content.push(json!({ "type": "text", "text": text }));
                        rpc(id.clone(), json!({ "content": content, "isError": false }))
                    }
                    Err(problem) => answer(problem, true),
                },
                Ok(call) if call.path == "composite:status" => answer(serde_json::to_string_pretty(&status_summary().await).unwrap_or_default(), false),
                Ok(call) if call.path == "composite:wait" => answer(serde_json::to_string_pretty(&wait_for(&args).await).unwrap_or_default(), false),
                Ok(call) => match call_route(call).await {
                    Ok((status, text)) if status.is_success() => match serde_json::from_str::<Value>(&text) {
                        Ok(value) => answer(cut(serde_json::to_string_pretty(&shape(name, &args, value)).unwrap_or_default()), false),
                        Err(_) => answer(text, false),
                    },
                    Ok((_, text)) => answer(text, true),
                    Err(problem) => answer(problem, true),
                },
            }
        }
        _ => rpc_error(id, -32601, format!("Method not found: {method}")),
    }
}

pub async fn info() -> &'static str {
    "Studio MCP endpoint: POST JSON-RPC here (Streamable HTTP)."
}

/// What an agent is told when it connects.
const INSTRUCTIONS: &str = "You drive a music studio on this computer. Every tool is a button of the studio: what you do, the user sees in the studio's window. Long work (songs, stems, preparation, training) runs as jobs: start it, then poll studio_status or the job's own get tool until it finishes. The graphics card runs one heavy job at a time; while a LoRA trains, songs are not made. Look things up with the tools instead of guessing ids: library_songs_list, training_status, lora_list, models_status.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_has_a_unique_name_and_an_object_schema() {
        let mut names: Vec<&str> = tools().iter().map(|tool| tool.name).collect();
        let count = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), count, "two tools share a name");
        for tool in tools() {
            let schema = (tool.schema)();
            assert_eq!(schema["type"], "object", "{} has no object schema", tool.name);
            assert!(!tool.description.is_empty(), "{} has no description", tool.name);
        }
    }

    #[test]
    fn a_tool_turns_its_arguments_into_its_route() {
        let find = |name: &str| tools().iter().find(|tool| tool.name == name).expect("the tool");
        let call = (find("dataset_song_update").call)(&json!({ "dataset_id": "d 1", "song_id": "s", "lyrics": "[Verse 1]" })).unwrap();
        assert_eq!(call.method, Method::PATCH);
        assert_eq!(call.path, "/v1/training/datasets/d%201/items/s");
        match call.payload {
            Payload::Json(body) => assert_eq!(body, json!({ "lyrics": "[Verse 1]" })),
            _ => panic!("a JSON body"),
        }
        assert!((find("library_song_get").call)(&json!({})).is_err(), "a missing id is refused");
    }

    #[test]
    fn a_folder_becomes_its_songs_with_their_paths() {
        let root = tempfile::tempdir().unwrap();
        let album = root.path().join("Artist").join("2020 - Album");
        std::fs::create_dir_all(&album).unwrap();
        std::fs::write(album.join("01. Song.flac"), b"x").unwrap();
        std::fs::write(album.join("01. Song.lrc"), b"x").unwrap();
        std::fs::write(album.join("cover.jpg"), b"x").unwrap();
        let files = folder_files(&root.path().join("Artist")).unwrap();
        let names: Vec<&str> = files.iter().map(|(_, _, name)| name.as_str()).collect();
        assert_eq!(names, ["Artist/2020 - Album/01. Song.flac", "Artist/2020 - Album/01. Song.lrc"]);
    }

    #[tokio::test]
    async fn the_server_introduces_itself_and_lists_its_tools() {
        let reply = |body: Value| async move {
            let response = handle(axum::body::Bytes::from(body.to_string())).await;
            let bytes = axum::body::to_bytes(response.into_body(), 1 << 22).await.unwrap();
            serde_json::from_slice::<Value>(&bytes).unwrap()
        };
        let hello = reply(json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18" } })).await;
        assert_eq!(hello["result"]["capabilities"], json!({ "tools": {}, "resources": {}, "prompts": {} }));
        let resources = reply(json!({ "jsonrpc": "2.0", "id": 4, "method": "resources/read", "params": { "uri": "studio://guide/lyrics" } })).await;
        assert!(resources["result"]["contents"][0]["text"].as_str().unwrap().contains("[Verse 1]"));
        let skill = reply(json!({ "jsonrpc": "2.0", "id": 5, "method": "prompts/get", "params": { "name": "studio" } })).await;
        assert!(skill["result"]["messages"][0]["content"]["text"].as_str().unwrap().contains("MCP"));
        let list = reply(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" })).await;
        assert_eq!(list["result"]["tools"].as_array().unwrap().len(), tools().len());
        let unknown = reply(json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "nope" } })).await;
        assert_eq!(unknown["result"]["isError"], true);
    }
}
