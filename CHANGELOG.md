# Changelog

What changed, newest first. Dates are release dates; the studio is versioned by its
Windows build.

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
