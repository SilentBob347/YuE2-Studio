---
name: yue2-studio
description: Drive YuE2 Studio on this computer through its MCP server - write and make songs with YuE2 (style, lyrics, scores, covers of recordings), manage the library, draw covers, split stems, time karaoke, process audio, make video clips, play songs, install LoRA, and build a LoRA from a folder of songs end to end, writing the lyrics layout and styles yourself instead of the studio's small assistant. Sees and works the studio's window like a user. Use whenever the user asks for anything the studio does.
---

# YuE2 Studio through MCP

YuE2 Studio serves MCP at `http://127.0.0.1:8791/mcp` while it is open (Streamable HTTP,
stateless JSON-RPC). Every tool runs the same code as a button of the studio, and the user
sees what you do in the studio's window.

## If the studio is not running yet

1. It is a Windows desktop application. If it is not installed, download the installer or
   the portable archive from https://github.com/timoncool/YuE2-Studio/releases/latest (it needs an
   NVIDIA card; the first start offers to download the models).
2. Start it. The MCP server is up as soon as its window is: `http://127.0.0.1:8791/mcp`.
   Nothing else to install - no npx, no bridge.
3. Connect (below), then call `studio_status`. If the models are missing, `models_catalog`
   and `models_download` fetch them.

## Connect

```bash
claude mcp add --transport http yue2-studio http://127.0.0.1:8791/mcp
```

Other clients: `{ "mcpServers": { "yue2-studio": { "type": "streamable-http", "url": "http://127.0.0.1:8791/mcp" } } }`.

The server also serves this skill (resource `studio://skill`, prompt `studio`) and the
writing guides (resources `studio://guide/<topic>`). It speaks MCP `2026-07-28` (stateless:
every request carries its version in `_meta`, `server/discover` describes the server) and
the handshake revisions `2025-11-25`, `2025-06-18` and `2025-03-26` through `initialize`.
Only this computer's agents and the studio's own window may connect.

The user sees it in the studio too: Settings, **Agent (MCP)** shows whether an agent is
connected and the address to paste.

## Ground rules

- **Start with `studio_status`.** It tells what runs now and whether the window is open.
- **Long work is a job**: songs, scores, stems, karaoke, preparation, training. Start it,
  then `studio_wait` (a `job_id`, or `until: preparation | training | idle`) instead of
  polling. It returns after at most 240 s with how far the work got; call it again.
- **One heavy job holds the graphics card at a time.** While a LoRA trains no song is
  made; start training last.
- **Answers are short by default**: a song job is its status and the songs it made, a
  library song leaves out its audio codes, `lora_list` gives one line per LoRA. Pass
  `response_format: detailed` when you need every field.
- **Covers are drawn only with an image model set up** (`settings_get`, covers; an
  OpenRouter key). Without one a `cover_prompt` is kept but no cover appears;
  `cover_set_from_file` still works.
- **Look ids up, never guess them**: `library_songs_list`, `training_status`,
  `dataset_get`, `lora_list`, `models_status`.
- **Files on this computer are passed by path**: `dataset_add_folder`,
  `library_import_audio`, `score_transcribe`, `cover_set_from_file`, `video_set`.
  `library_song_files` and `dataset_song_files` give the paths of the studio's own files.
- **You write, not the studio's assistant.** The studio has a small local model (Gemma)
  for users without an agent. You write better: read `writing_guide` and
  `writing_examples` first and write the style, lyrics and scores yourself. Use
  `assistant_write` only when the user asks for the studio's assistant.

## What YuE2 reads - read `writing_guide` for the full rules

- **style**: one English sentence in this order - language, genre with its era, vocal
  (register, gender, delivery), instruments named concretely, mood in two to four words,
  production, and last `N BPM`. No artist, song title, key or time signature. An
  instrumental says `instrumental` where the language goes.
- **lyrics**: sections tagged `[Intro]`, `[Verse 1]`, `[Pre-Chorus]`, `[Chorus]`,
  `[Bridge]`, `[Outro]`, one tag per line, a blank line between sections, about 2-3 sung
  words per second. Russian `ё` stays `ё`.
- **abc**: the score the model sings from, in YuE2's dialect (`writing_guide` topic
  `score`). `score_compose` writes one to start from.
- `writing_examples` returns the official M-A-P requests closest to your idea - match
  their shape and density.

## Recipes

**A song from an idea**

1. `writing_guide` topic `song`, `writing_examples` with the genre and mood.
2. Write the style and lyrics yourself.
3. `song_create` (with `title` and `cover_prompt`), then `studio_wait` with its `job_id`.
4. `player_play` with the new song's id to let the user hear it; `ui_screenshot` shows it.

**A cover of a recording**

1. `score_transcribe` with `song_id` or `path`; `studio_wait` with its `job_id`.
2. `song_create` with the new style, the original lyrics and that score as `abc`.

**A LoRA from a folder of songs, written by you**

1. `training_status`. If the trainer or the listening pack is missing:
   `training_pack_install`, `training_listen_pack_install`.
2. `dataset_create` with the artist's name (the trigger word is made from it), then
   `dataset_add_folder` with the folder.
3. `dataset_prepare` with `lyrics: missing, style: missing, writer: agent`, then
   `studio_wait until: preparation`. The studio finds the lyrics in LRCLIB, QQ Music and
   Kugou, recognises only what they miss with Whisper, and listens to every song with
   MOSS-Music, measuring the tempo. It leaves the writing to you.
4. `dataset_get`. For each song:
   - `lyrics_state: found` - the words are there (from a database when `lyrics_source`
     names one: keep every word; `recognised`: fix the recogniser's mishearings). Lay them
     out in sections (`writing_guide` topics `sections` or `transcript`).
   - `style_state: heard` - `heard` holds what MOSS heard (genre, caption, bpm). Write the
     style sentence from it (`writing_guide` topic `style`), ending with that BPM.
   - Save with `dataset_song_update`; what you write is final and marks the song done.
   - `lyrics_state: wanted` after preparation: nothing found it. `lyrics_find` with other
     spellings, or ask the user, or write it instrumental.
5. `training_start` with `recipe_defaults` from `training_status` (stop `kl` at 1.4, or
   stop `epochs`). `studio_wait until: training`; `training_status` shows step, loss, KL.
6. `training_checkpoint_install` for the chosen step, then `song_create` with that LoRA
   in `adapters` and its trigger word in the style.

**The create page, where the user can see it**

`song_create` makes a song directly. When the user wants to watch and adjust it first:
`ui_navigate` create, `create_form_set` with the fields (the user sees them fill in),
`create_form_get` to check, and `create_form_submit` to press Create.

**When you are the studio's writing assistant**

The user can pick **Agent (MCP)** as the assistant engine. Then the studio's write buttons,
and the lyric layout and the styles of a dataset preparation, ask you instead of its local model:
`assistant_requests_wait` returns each request with the instructions and the answer schema
the local model would get; write the answer by them and send it with
`assistant_request_answer`. Keep calling `assistant_requests_wait` while the user works -
`studio_status` shows `assistant_requests_waiting`. A request waits 15 minutes.

**Talking to the user**

`ui_notify` shows the user a short message in the window. `ui_console` shows the errors the
window logged, when a button did nothing.

**A video clip**

1. `video_open` with a song id, `video_get` to see the presets and settings.
2. `video_set`: preset, aspect ratio, colours, effects, text layers, karaoke lyrics,
   a background picture or video from a path. `video_seek` and `ui_screenshot` to look.
3. `video_render`, then `video_get` until `export.saved` names the MP4 (or `export.error` says why not).

**Anything the tools do not cover**

`ui_read_page` lists every control of the window with a ref; `ui_click`, `ui_type`,
`ui_select`, `ui_press_key` work it like the user; `ui_navigate` and `ui_open_settings`
move around. Check the result with `ui_screenshot`.

## Tools by area

- **studio**: status, wait, system, capabilities, open data folder; **settings** get/set.
- **models**: status, catalog, download, adopt (files already on disk), select, cancel,
  remove; **engine**: options,
  restart, logs.
- **song**: create, defaults (what a field left out becomes), job get/list/cancel, replay; **score**: compose, transcribe, job
  get/cancel.
- **writing**: guide, examples; **assistant**: write, status, set, runtime, models;
  requests wait and answer (when you are the assistant).
- **library**: songs list, song get/update/delete/files, import audio, versions;
  **playlist**: list/create/update/delete.
- **cover**: draw, set from file, templates, prompt render; **karaoke**: make, delete,
  settings; **recogniser**: install/remove; **stems**: split, get; **separator**: status,
  install, settings; **processing**: start, get, keep, discard, reference; **vst**.
- **lora**: list, install from the catalogue or Hugging Face, import files, update,
  delete.
- **dataset**: create, add folder or library songs, import, get, update, delete, song
  update/delete/files, prepare (+ cancel, train after), reveal; **lyrics**: find;
  **training**: status, start, cancel, checkpoint install, run delete, packs.
- **ui**: screenshot, read page, click, type, select, press key, scroll, navigate, open
  settings, notify, console; **create_form**: get, set, submit; **player**: state, play, pause, seek, next, previous, set; **video**: open,
  get, set, render, play, pause, seek, close.
- **openrouter**: status, key, catalog, log, complete, cover, transcribe.
