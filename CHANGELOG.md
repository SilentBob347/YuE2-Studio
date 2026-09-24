# Changelog

What changed, newest first. Dates are release dates; the studio is versioned by its
Windows build.

## 2026-09-24 — 1.1.1

### Fixed

- **Songs play after the studio's folder moves.** The library kept each song's full path,
  so a drive that came back under another letter after a restart, or a portable folder
  copied elsewhere, left every song saying it was no longer available while the files
  sat in the media folder. Songs are now found by name in the studio's own media folder.
- **Errors say why.** Stem separation, karaoke and cover art showed only the first line
  of a failure ("load the separation model ..."), without its cause; the whole reason is
  shown now. When the graphics card cannot load the separation model, the message says
  to choose the processor instead.

### Added

- **Select everything in the LoRA catalogue** that is not downloaded yet, in one click,
  and download it as one set.

## 2026-09-24 — 1.1.0

### Added

- **LoRA.** A LoRA page with the installed files, a catalogue of ready ones (styles,
  artists, sound, sliders, with their authors credited) and a search on Hugging Face that
  downloads what you pick. In the create form each LoRA gets its own strength for the
  composition and for the sound, and its trigger word goes into the style for you. The
  engine merges LoRA and LoKr at load (yue2.cpp fork `adapters`, 7647831), honours the
  rsLoRA scale, and refuses DoRA and LoHa files by name instead of playing them wrong.
- **Training your own LoRA.** An optional tab on the LoRA page. 5–20 songs of one artist
  or style become a LoRA on your card with HOT-Step's trainer and its tuned recipe: LoKr
  64/4, Prodigy, a stop when the composition half drifts past a KL of 1.4, a checkpoint
  every 50 steps, and lyric timing from the MMS forced aligner, which now reads Cyrillic
  too. Every setting of the recipe is editable under Advanced, with the defaults one
  click away. The vocals of each song are separated first, so the aligner hears the voice.
  The assistant fills in a song's lyrics by ear, and each checkpoint goes into the LoRA
  library in one click. The trainer and its weights (about 8 GB) download only when you
  open training; it needs an RTX 30-series card or newer with 11 GB of VRAM.
- **Datasets travel between studios.** A dataset is a folder with `dataset.json` and its
  audio; import one from MiniMax Music3 Studio or show the folder to take it there.
- **Audio processing.** Noise reduction, the Spectral Lifter, a vocal naturaliser, your own
  VST3 plugins in a chain, and mastering to a reference track, in that order. Plugins are
  found in the system VST3 folders, each is set up in its own window, and they run in a
  host process of their own, so a plugin that crashes does not take the studio with it.
  Compare before and after while it plays; keep the result as a version of the track,
  next to the untouched original, or throw it away.

### Changed

- **MP3 is made by the studio.** The engine renders 32-bit float and the studio encodes
  the MP3 with LAME, so nothing is lost before the encoder.
- **Section tags keep the order they are pressed in**, and the LoRA list in the create form
  is no longer cut off by its card.

## 2026-09-24 — 1.0.4

### Fixed

- **The audio editor opens with the track.** It opened blank: the waveform library it
  runs on was left out of every build since 1.0.0, so the editor stopped on start. The
  library is back, and a test now checks that every file the editor loads is built in.

### Changed

- **Newer runtimes for the add-ons.** The assistant downloads llama.cpp b11146 (CUDA 13.4)
  instead of b9966, and karaoke and stem separation download ONNX Runtime 1.30.0
  instead of 1.24.2. Add-ons already installed keep working on the versions they have.

## 2026-09-23 — 1.0.3

### Fixed

- **Karaoke follows the song through repeated choruses.** A line used to jump to a later
  repeat of itself when the recogniser heard that one more clearly, and every line sung
  in between was squeezed into a second at the end or left hanging for half a minute.
  Lines are now placed together, in order, so each chorus keeps its own lines, a line
  nobody heard is filled in between its neighbours, and a chorus the model sang twice in
  a row shows its words as they are first sung.

### Changed

- **Covers say what they need.** The cover card now says that the words must fit the
  transcribed melody: the original lyrics, or new ones with the same syllables in every
  line and the stresses on the same notes.

## 2026-09-23 — 1.0.2

### Fixed

- **Updates install from inside the studio.** Pressing Install closed the studio and
  nothing changed: the installer was started inside the studio's own process group,
  which Windows ends together with the studio, so it died a moment after starting.
  The installer is now let go before the studio exits, and it is told the folder the
  studio lives in: started from the studio it used to miss the previous folder and put
  a second copy into the default one. Versions 1.0.0 and 1.0.1 still
  carry the old behaviour, so from them the update is downloaded once by hand; from
  1.0.2 on the studio updates itself.

## 2026-09-23 — 1.0.1

### Added

- **Listen to the recording you cover.** The cover card shows the chosen track with a
  waveform player: play, pause, click the waveform to seek. After transcription the
  studio stays in Cover mode, with the score right below.

### Changed

- **No Instrumental switch.** YuE2 is trained on songs with vocals and sings whatever it
  is given: with empty lyrics it makes words up. The switch promised something the model
  does not do, so it is gone.

### Fixed

- **Deleting a song frees its disk space.** Its audio, its six stems and its cover are
  removed with it; before, they stayed in the media folder.

## 2026-09-23 — 1.0.0

The first release of YuE2 Studio: the studio of MiniMax Music3 Studio, rebuilt around
YuE2 and [yue2.cpp](https://github.com/ServeurpersoCom/yue2.cpp) at commit `ea07706`.

### Added

- **Full songs from a style and lyrics** on YuE2-3B, up to six minutes, rendered by
  `yue-server` on the GPU. Progress follows the engine's own stages: score, audio codes,
  acoustic rendering, decoding.
- **The score as a first-class part of every song.** The ABC score comes back with each
  track, is engraved as sheet music, and can be edited and sung again. Three modes —
  melody with chords, melody only, no score — with a warning and a one-click fix when a
  melody-mode score still carries chord symbols.
- **Compose the score alone** — the planning stage without singing, in seconds.
- **Covers** — SheetSage2 transcribes an uploaded recording or a library track, melody
  only or with chords, straight into the score.
- **Exact replay** — every track keeps its request and audio codes: re-render it bit for
  bit, or with other steps, a new sound seed, several variations, another format.
- **Every engine setting** — both sampling presets in full, guidance, both seeds, songs
  per request and variations, peak normalisation, MP3 bitrate or 16/24/32-bit WAV, and the
  server's own launch options (song limit, KV cache size, VAE tiling, flash attention,
  FP16 clamping, keep-loaded).
- **Prompt files in the engine's format** — JSON or YAML, compatible with the yue2.cpp
  WebUI and `yue-synth --request`; a prompt that carries audio codes asks whether to render
  the same take or sing a new one.
- **The 110 examples** that ship with yue2.cpp, one click to load.
- **Model sets** from Serveurperso/YuE2-GGUF pinned to `64b030e`: Full native BF16,
  Quality Q8_0, Balanced Q6_K, Light Q5_K_M, each with the matching SheetSage2, or a
  custom mix role by role. The studio names the set that fits your card; every file is
  checked by SHA-256 and downloads resume.
- **A writing assistant** tuned to YuE2's style tags and lyric sections, local (Gemma via
  llama.cpp) or through OpenRouter; it can also edit the score on request.
- **CUDA, Vulkan and CPU backends** in one engine, loaded at run time. NVIDIA runs on CUDA
  (cuBLAS is fetched once, on NVIDIA only). AMD and Intel run on Vulkan, experimentally:
  the Vulkan path is verified on NVIDIA, but AMD Radeon integrated graphics gave
  unintelligible vocals and discrete AMD and Intel cards are untested. The FP16 clamp is on
  for Vulkan, which stops the engine crashing on the silence it otherwise rendered.
  Settings → Local engine chooses the device.
- **Interface in five languages** — English, Russian, Chinese, Japanese, Korean.
- **Windows installer with auto-update** and a portable archive.

### Kept from MiniMax Music3 Studio

Library, playlists, word-level karaoke (Parakeet or Whisper), six-stem separation with
HT-Demucs, cover art from prompt templates, MP3 tagging, the resource monitor.

### Removed

- Cloud music generation through OpenRouter: music is always YuE2 on your machine. Cover
  art, transcription and the assistant can still use OpenRouter.
- Everything specific to MiniMax Music3: its engine, model sets, RVQ encoder and prompting
  skill.
