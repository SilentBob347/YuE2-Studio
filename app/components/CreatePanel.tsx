import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { karaokeReason } from '../services/karaoke';
import {
  AlertTriangle, AudioLines, ChevronDown, CircleAlert, Dices, Eye, EyeOff, FileMusic, FolderOpen, Loader2,
  Music2, RotateCcw, Save, Sparkles, Square, Upload, Wand2, Settings2, X,
} from 'lucide-react';
import type { Song, YueCot, YueOutputFormat, YueRequest, YueSampling } from '../types';
import { useI18n } from '../context/I18nContext';
import { EXAMPLES, randomExample } from '../services/examples';
import { ScoreView } from './ScoreView';
import { transcribe } from '../services/transcription';

/**
 * The YuE2 request form.
 *
 * Grouped the way the engine works, stage by stage: the prompt (style and
 * lyrics), the score the autoregressive half plans before it writes codes, the
 * semantic stage, and the acoustic side (flow matching and output). A field
 * left empty is the engine's own default, shown as the placeholder, so the
 * request stays sparse exactly like the engine's reference client sends it.
 */

interface CreatePanelProps {
  onGenerate: (request: YueRequest & { _tempId?: string }) => void;
  isGenerating: boolean;
  activeJobCount?: number;
  initialData?: { song: Song; timestamp: number } | null;
}

type EngineDefaults = Partial<Record<string, unknown>> & {
  abc_sampling?: YueSampling;
  semantic_sampling?: YueSampling;
};

type ProfileFiles = { backbone: string; vae: string; transcriber?: string | null };

type SetupStatus = {
  ready?: boolean;
  profile_files?: ProfileFiles | null;
  engine_ready?: boolean;
  selected_profile_id?: string | null;
  selected_component_ids?: string[] | null;
  effective_max_batch?: number;
  hardware?: { reason?: string };
};

type EngineCatalog = {
  defaults?: EngineDefaults;
  transcriber?: string | null;
  max_batch?: number;
  version?: string;
};

type SamplingText = Record<keyof YueSampling, string>;

/** 9000 semantic frames at 25 per second, the stage's own budget. */
const MAX_DURATION_SECONDS = 360;
const SAMPLING_KEYS: (keyof YueSampling)[] = ['temperature', 'top_p', 'top_k', 'repetition_penalty', 'penalty_window', 'min_tokens', 'max_tokens'];
/** A quoted chord symbol in ABC: "Am", "F/C", "G7". */
const CHORD_SYMBOL = /"[^"\n]+"/;
const CHORD_SYMBOLS = /"[^"\n]+"/g;
const SECTION_TAGS = ['[Intro]', '[Verse 1]', '[Pre-Chorus]', '[Chorus]', '[Verse 2]', '[Bridge]', '[Instrumental Break]', '[Outro]'];

const PROFILE_LABEL: Record<string, string> = {
  native: 'Full Native · BF16',
  'quality-q8': 'Quality · Q8_0',
  balanced: 'Balanced · Q6_K',
  light: 'Light · Q5_K_M',
};

const CONTROL =
  'w-full rounded-lg border border-zinc-200 bg-zinc-50 px-3 py-2 text-sm text-zinc-900 outline-none transition-colors focus:border-pink-500 disabled:opacity-50 dark:border-white/10 dark:bg-black/25 dark:text-white';
const LABEL = 'mb-1.5 block text-[11px] font-semibold uppercase tracking-wide text-zinc-500 dark:text-zinc-400';
const ICON =
  'rounded-md p-1.5 text-zinc-400 transition-colors hover:bg-zinc-200 hover:text-black dark:hover:bg-white/10 dark:hover:text-white disabled:opacity-40';
const CHIP =
  'rounded-md border border-zinc-200 px-2 py-0.5 font-mono text-[10px] text-zinc-500 transition-colors hover:border-pink-400 hover:text-pink-600 dark:border-white/10 dark:text-zinc-400';

const emptySampling = (): SamplingText => ({ temperature: '', top_p: '', top_k: '', repetition_penalty: '', penalty_window: '', min_tokens: '', max_tokens: '' });

const numberOrUndefined = (value: string): number | undefined => {
  const trimmed = value.trim();
  if (trimmed === '') return undefined;
  const parsed = Number(trimmed);
  return Number.isFinite(parsed) ? parsed : undefined;
};

const asText = (value: unknown) => (typeof value === 'number' || typeof value === 'string' ? String(value) : '');

const samplingFrom = (text: SamplingText): YueSampling | undefined => {
  const preset: YueSampling = {};
  for (const key of SAMPLING_KEYS) {
    const value = numberOrUndefined(text[key]);
    if (value !== undefined) preset[key] = value;
  }
  return Object.keys(preset).length ? preset : undefined;
};

const samplingText = (value: unknown): SamplingText => {
  const text = emptySampling();
  if (value && typeof value === 'object') {
    for (const key of SAMPLING_KEYS) text[key] = asText((value as Record<string, unknown>)[key]);
  }
  return text;
};

const Field: React.FC<{ label: string; hint?: string; children: React.ReactNode }> = ({ label, hint, children }) => (
  <label className="block">
    <span className={LABEL}>{label}</span>
    {children}
    {hint && <span className="mt-1 block text-[11px] leading-4 text-zinc-500">{hint}</span>}
  </label>
);

const Switch: React.FC<{ checked: boolean; onChange: (value: boolean) => void; label: string; hint?: string }> = ({ checked, onChange, label, hint }) => (
  <div className="flex items-center justify-between gap-3">
    <div className="min-w-0">
      <span className="text-[11px] font-bold uppercase tracking-wide text-zinc-500 dark:text-zinc-400">{label}</span>
      {hint && <p className="mt-0.5 text-[11px] leading-4 text-zinc-500">{hint}</p>}
    </div>
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      onClick={() => onChange(!checked)}
      className={`relative h-5 w-10 shrink-0 rounded-full transition-colors ${checked ? 'bg-pink-500' : 'bg-zinc-300 dark:bg-zinc-600'}`}
    >
      <span className={`absolute top-[2px] h-4 w-4 rounded-full bg-white shadow-sm transition-all ${checked ? 'left-[22px]' : 'left-[2px]'}`} />
    </button>
  </div>
);

/** A number you drag. Empty means "engine default", shown until touched. */
const SliderRow: React.FC<{
  label: string;
  value: string;
  fallback: number;
  min: number;
  max: number;
  step: number;
  suffix?: string;
  onChange: (value: string) => void;
  disabled?: boolean;
  format?: (value: number) => string;
}> = ({ label, value, fallback, min, max, step, suffix, onChange, disabled, format }) => {
  const current = value.trim() === '' ? fallback : Number(value);
  const shown = Number.isFinite(current) ? current : fallback;
  const decimals = step < 1 ? String(step).split('.')[1]?.length ?? 1 : 0;
  return (
    <div className={disabled ? 'opacity-50' : undefined}>
      <div className="flex items-baseline justify-between gap-2">
        <span className="text-[11px] font-semibold uppercase tracking-wide text-zinc-500 dark:text-zinc-400">{label}</span>
        <span className="text-[11px] tabular-nums text-zinc-600 dark:text-zinc-300">
          {format ? format(shown) : `${shown.toFixed(decimals)}${suffix ?? ''}`}
          {value.trim() === '' && <span className="ml-1 text-zinc-400" title="engine default">·</span>}
        </span>
      </div>
      <input
        type="range"
        min={min}
        max={max}
        step={step}
        value={shown}
        disabled={disabled}
        aria-label={label}
        onChange={event => onChange(event.target.value)}
        className="mt-1.5 h-1 w-full cursor-pointer accent-pink-500"
      />
    </div>
  );
};

const Card: React.FC<{ title: string; icon?: React.ReactNode; actions?: React.ReactNode; children: React.ReactNode }> = ({ title, icon, actions, children }) => (
  <div className="overflow-hidden rounded-xl border border-zinc-200 bg-white dark:border-white/5 dark:bg-suno-card">
    <div className="flex items-center justify-between gap-2 border-b border-zinc-100 bg-zinc-50 px-3 py-2 dark:border-white/5 dark:bg-white/5">
      <span className="flex items-center gap-1.5 text-[11px] font-bold uppercase tracking-wide text-zinc-500 dark:text-zinc-400">{icon}{title}</span>
      {actions && <div className="flex items-center gap-1">{actions}</div>}
    </div>
    <div className="p-3">{children}</div>
  </div>
);

const Stage: React.FC<{ title: string; hint: string; children: React.ReactNode }> = ({ title, hint, children }) => (
  <section>
    <h4 className="text-[11px] font-bold uppercase tracking-wide text-zinc-500 dark:text-zinc-400">{title}</h4>
    <p className="mb-3 mt-0.5 text-[11px] leading-4 text-zinc-500">{hint}</p>
    {children}
  </section>
);

const AutoTextarea: React.FC<React.TextareaHTMLAttributes<HTMLTextAreaElement> & { minRows?: number }> = ({ minRows = 3, value, ...rest }) => {
  const node = useRef<HTMLTextAreaElement | null>(null);
  useEffect(() => {
    const element = node.current;
    if (!element) return;
    element.style.height = 'auto';
    element.style.height = `${Math.max(element.scrollHeight, minRows * 20)}px`;
  }, [value, minRows]);
  return <textarea ref={node} value={value} rows={minRows} {...rest} />;
};

/** Seven knobs of one autoregressive stage, each defaulting to the checkpoint. */
const SamplingGrid: React.FC<{ value: SamplingText; defaults?: YueSampling; onChange: (value: SamplingText) => void; t: (key: never) => string }> = ({ value, defaults, onChange, t }) => {
  const label: Record<keyof YueSampling, string> = {
    temperature: t('samplingTemperature' as never),
    top_p: 'Top P',
    top_k: 'Top K',
    repetition_penalty: t('samplingRepetitionPenalty' as never),
    penalty_window: t('samplingPenaltyWindow' as never),
    min_tokens: t('samplingMinTokens' as never),
    max_tokens: t('samplingMaxTokens' as never),
  };
  return (
    <div className="grid grid-cols-2 gap-2">
      {SAMPLING_KEYS.map(key => (
        <Field key={key} label={label[key]}>
          <input
            value={value[key]}
            onChange={event => onChange({ ...value, [key]: event.target.value })}
            placeholder={asText(defaults?.[key])}
            inputMode="decimal"
            className={CONTROL}
          />
        </Field>
      ))}
    </div>
  );
};

export const CreatePanel: React.FC<CreatePanelProps> = ({ onGenerate, isGenerating, activeJobCount = 0, initialData }) => {
  const { t } = useI18n();
  const tt = t as unknown as (key: string) => string;

  const [name, setName] = useState('');
  const [style, setStyle] = useState('');
  const [lyrics, setLyrics] = useState('');
  const [instrumental, setInstrumental] = useState(false);
  const [abc, setAbc] = useState('');
  const [cot, setCot] = useState<YueCot | ''>('');
  const [showNotation, setShowNotation] = useState(true);

  // Strings, so an empty field can mean "engine default".
  const [duration, setDuration] = useState('');
  const [lmBatch, setLmBatch] = useState('');
  const [synthBatch, setSynthBatch] = useState('');
  const [steps, setSteps] = useState('');
  const [cfgScale, setCfgScale] = useState('');
  const [randomizeSeed, setRandomizeSeed] = useState(true);
  const [lmSeed, setLmSeed] = useState('');
  const [seed, setSeed] = useState('');
  const [semanticTokens, setSemanticTokens] = useState('');
  const [abcSampling, setAbcSampling] = useState<SamplingText>(emptySampling);
  const [semanticSampling, setSemanticSampling] = useState<SamplingText>(emptySampling);
  const [peakClip, setPeakClip] = useState('');
  // The engine default of 128 kbps throws away what the VAE produced.
  const [mp3Bitrate, setMp3Bitrate] = useState('320');
  const [format, setFormat] = useState<YueOutputFormat>('mp3');

  const [setup, setSetup] = useState<SetupStatus | null>(null);
  const [serviceDown, setServiceDown] = useState(false);
  const [catalog, setCatalog] = useState<EngineCatalog | null>(null);
  const [assistantReady, setAssistantReady] = useState(false);
  const [assisting, setAssisting] = useState<'all' | 'lyrics' | 'style' | 'score' | null>(null);
  const [assistStage, setAssistStage] = useState<string | null>(null);
  const [assistModel, setAssistModel] = useState<string | null>(null);
  const [assistDraft, setAssistDraft] = useState('');
  const [assistSeconds, setAssistSeconds] = useState(0);
  const [coverPrompt, setCoverPrompt] = useState('');
  const [activity, setActivity] = useState<Array<{ song_id: string; title: string; kind: string; state: string; detail?: string }>>([]);
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [mode, setMode] = useState<'studio' | 'simple' | 'cover'>('studio');
  const [assistInstruction, setAssistInstruction] = useState('');
  const [scoreInstruction, setScoreInstruction] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [examplesOpen, setExamplesOpen] = useState(false);
  const [transcribing, setTranscribing] = useState<string | null>(null);
  const [coverSource, setCoverSource] = useState<string>('');
  const [coverMelodyOnly, setCoverMelodyOnly] = useState(true);
  const promptFile = useRef<HTMLInputElement | null>(null);
  const scoreFile = useRef<HTMLInputElement | null>(null);
  const audioFile = useRef<HTMLInputElement | null>(null);
  const lyricsBox = useRef<HTMLTextAreaElement | null>(null);

  const ready = setup?.ready === true && setup?.engine_ready === true;
  const defaults: EngineDefaults = catalog?.defaults ?? {};
  const placeholder = (key: string) => asText(defaults[key]);
  const maxBatch = Math.max(1, catalog?.max_batch ?? setup?.effective_max_batch ?? 1);
  const transcriberReady = Boolean(catalog?.transcriber);
  const effectiveCot: YueCot = cot || (defaults.cot as YueCot) || 'full';

  const profileLabel = useMemo(() => {
    if (setup?.selected_component_ids?.length) return t('customSet');
    const id = setup?.selected_profile_id;
    return id ? PROFILE_LABEL[id] ?? id : '—';
  }, [setup, t]);

  useEffect(() => {
    let finished = '';
    const read = () => void fetch('/v1/activity')
      .then(response => response.json())
      .then((body: { activity?: typeof activity }) => {
        const entries = body.activity ?? [];
        const done = entries.filter(entry => entry.state === 'done').map(entry => `${entry.song_id}:${entry.kind}`).join(',');
        if (done !== finished) {
          finished = done;
          window.dispatchEvent(new CustomEvent('yue:library-changed'));
        }
        setActivity(entries);
      })
      .catch(() => undefined);
    read();
    const timer = window.setInterval(read, 2000);
    return () => window.clearInterval(timer);
  }, []);

  useEffect(() => {
    if (!assisting) return;
    setAssistSeconds(0);
    const started = Date.now();
    const timer = window.setInterval(() => setAssistSeconds(Math.round((Date.now() - started) / 1000)), 1000);
    return () => window.clearInterval(timer);
  }, [assisting]);

  const refreshSetup = useCallback(async () => {
    const response = await fetch('/setup/status');
    if (!response.ok) throw new Error(String(response.status));
    setSetup(await response.json());
    setServiceDown(false);
  }, []);

  useEffect(() => {
    const poll = () => void refreshSetup().catch(() => { setSetup(null); setServiceDown(true); });
    poll();
    const timer = window.setInterval(poll, 5000);
    return () => window.clearInterval(timer);
  }, [refreshSetup]);

  useEffect(() => {
    void fetch('/v1/local-models/music')
      .then(response => (response.ok ? response.json() : Promise.reject(new Error())))
      .then((body: { catalog?: EngineCatalog }) => setCatalog(body.catalog ?? null))
      .catch(() => setCatalog(null));
  }, [setup?.engine_ready, setup?.selected_profile_id]);

  useEffect(() => {
    const read = () => void fetch('/v1/assistant/status')
      .then(response => (response.ok ? response.json() : Promise.reject(new Error())))
      .then((body: { available?: boolean }) => setAssistantReady(body.available === true))
      .catch(() => setAssistantReady(false));
    read();
    const timer = window.setInterval(read, 5000);
    window.addEventListener('yue:settings-changed', read);
    return () => {
      window.clearInterval(timer);
      window.removeEventListener('yue:settings-changed', read);
    };
  }, []);

  /** Fills the form from a stored request: a reused track, a file, an example. */
  const applyRequest = useCallback((request: Record<string, unknown>, title?: string) => {
    if (title !== undefined) setName(title);
    if (typeof request.style === 'string') setStyle(request.style);
    if (typeof request.lyrics === 'string') setLyrics(request.lyrics);
    setInstrumental(false);
    setAbc(typeof request.abc === 'string' ? request.abc.trimEnd() : '');
    setCot(request.cot === 'full' || request.cot === 'melody' || request.cot === 'off' ? request.cot : '');
    setDuration(asText(request.duration ?? request.duration_seconds));
    setLmBatch('');
    setSynthBatch('');
    setSteps(asText(request.steps));
    setCfgScale(typeof request.cfg_scale === 'number' && request.cfg_scale >= 0 ? String(request.cfg_scale) : '');
    const storedLmSeed = asText(request.lm_seed);
    const storedSeed = asText(request.seed);
    setLmSeed(storedLmSeed === '-1' ? '' : storedLmSeed);
    setSeed(storedSeed === '-1' ? '' : storedSeed);
    setRandomizeSeed(!(storedLmSeed && storedLmSeed !== '-1'));
    setSemanticTokens(typeof request.semantic_tokens === 'string' ? request.semantic_tokens : '');
    setAbcSampling(samplingText(request.abc_sampling));
    setSemanticSampling(samplingText(request.semantic_sampling));
    setPeakClip(asText(request.peak_clip));
    if (request.mp3_bitrate !== undefined) setMp3Bitrate(asText(request.mp3_bitrate));
    if (typeof request.output_format === 'string') setFormat(request.output_format as YueOutputFormat);
    setError(null);
  }, []);

  useEffect(() => {
    if (!initialData?.song) return;
    const song = initialData.song;
    const settings = (song.generationParams ?? {}) as Record<string, unknown>;
    applyRequest({ style: song.style, lyrics: song.lyrics, ...settings }, song.title || '');
    setMode('studio');
  }, [initialData, applyRequest]);

  // A library track to cover: its recording becomes the score, and its own
  // words start the lyric sheet when there is nothing there yet.
  useEffect(() => {
    const onTranscribe = (event: Event) => {
      const detail = (event as CustomEvent<{ song: Song; melodyOnly: boolean }>).detail;
      if (!detail?.song) return;
      setCoverSource(detail.song.title);
      if (detail.song.lyrics?.trim()) setLyrics(current => (current.trim() ? current : detail.song.lyrics));
      setMode('cover');
      void runTranscription({ songId: detail.song.id }, detail.melodyOnly);
    };
    window.addEventListener('yue:transcribe-song', onTranscribe);
    return () => window.removeEventListener('yue:transcribe-song', onTranscribe);
  });

  // A score arriving from elsewhere: a transcribed library track, an edited plan.
  useEffect(() => {
    const onScore = (event: Event) => {
      const detail = (event as CustomEvent<{ abc: string; cot?: YueCot; lyrics?: string; title?: string }>).detail;
      if (!detail?.abc) return;
      setAbc(detail.abc.trimEnd());
      if (detail.cot) setCot(detail.cot);
      if (detail.lyrics && !lyrics.trim()) setLyrics(detail.lyrics);
      if (detail.title && !name.trim()) setName(detail.title);
      setSemanticTokens('');
      setMode('studio');
    };
    window.addEventListener('yue:use-score', onScore);
    return () => window.removeEventListener('yue:use-score', onScore);
  }, [lyrics, name]);

  const reset = () => {
    setName(''); setStyle(''); setLyrics(''); setInstrumental(false); setAbc(''); setCot('');
    resetParameters();
    setCoverPrompt('');
    setError(null);
  };

  const resetParameters = () => {
    setDuration(''); setLmBatch(''); setSynthBatch(''); setSteps(''); setCfgScale('');
    setRandomizeSeed(true); setLmSeed(''); setSeed(''); setSemanticTokens('');
    setAbcSampling(emptySampling()); setSemanticSampling(emptySampling());
    setPeakClip(''); setMp3Bitrate('320'); setFormat('mp3');
  };

  const loadExample = (id?: string) => {
    const example = (id && EXAMPLES.find(entry => entry.id === id)) || randomExample();
    applyRequest({ style: example.style, lyrics: example.lyrics, cot: example.cot, abc: example.abc }, example.title);
    setExamplesOpen(false);
  };

  const buildRequest = (): YueRequest => {
    const request: YueRequest = {
      style: style.trim(),
      // An instrumental has no words, whatever is still sitting in the box.
      lyrics: instrumental ? '' : lyrics.replace(/\r\n?/g, '\n').trim(),
      output_format: format,
    };
    if (abc.trim() && effectiveCot !== 'off') request.abc = abc.trim();
    if (cot) request.cot = cot;
    const durationValue = numberOrUndefined(duration);
    if (durationValue !== undefined) request.duration_seconds = Math.min(Math.max(durationValue, 1), MAX_DURATION_SECONDS);
    const lmBatchValue = numberOrUndefined(lmBatch);
    if (lmBatchValue !== undefined) request.lm_batch_size = lmBatchValue;
    const synthBatchValue = numberOrUndefined(synthBatch);
    if (synthBatchValue !== undefined) request.synth_batch_size = synthBatchValue;
    const stepsValue = numberOrUndefined(steps);
    if (stepsValue !== undefined) request.steps = stepsValue;
    const cfgValue = numberOrUndefined(cfgScale);
    if (cfgValue !== undefined) request.cfg_scale = cfgValue;
    if (!randomizeSeed) {
      const lmSeedValue = numberOrUndefined(lmSeed);
      const seedValue = numberOrUndefined(seed);
      if (lmSeedValue !== undefined) request.lm_seed = lmSeedValue;
      if (seedValue !== undefined) request.seed = seedValue;
    }
    if (semanticTokens.trim()) request.semantic_tokens = semanticTokens.trim();
    const abcPreset = samplingFrom(abcSampling);
    if (abcPreset) request.abc_sampling = abcPreset;
    const semanticPreset = samplingFrom(semanticSampling);
    if (semanticPreset) request.semantic_sampling = semanticPreset;
    const peakValue = numberOrUndefined(peakClip);
    if (peakValue !== undefined) request.peak_clip = peakValue;
    const bitrate = numberOrUndefined(mp3Bitrate);
    if (bitrate !== undefined && format === 'mp3') request.mp3_bitrate = bitrate;
    if (name.trim()) request.title = name.trim();
    if (coverPrompt.trim()) request.cover_prompt = coverPrompt.trim();
    return request;
  };

  const download = (filename: string, text: string, type: string) => {
    const blob = new Blob([text], { type });
    const link = document.createElement('a');
    link.href = URL.createObjectURL(blob);
    link.download = filename;
    link.click();
    URL.revokeObjectURL(link.href);
  };

  const safeName = () => (name.trim() || 'request').replace(/[\\/:*?"<>|]/g, '');

  const savePrompt = () => download(`${safeName()}.json`, JSON.stringify(buildRequest(), null, 2), 'application/json');

  const openPrompt = async (file: File) => {
    try {
      const parsed = JSON.parse(await file.text()) as Record<string, unknown>;
      applyRequest(parsed, typeof parsed.title === 'string' ? parsed.title : file.name.replace(/\.json$/i, ''));
    } catch {
      setError(t('promptFileInvalid'));
    }
  };

  const openScore = async (file: File) => {
    const text = await file.text();
    if (!/K:/.test(text)) { setError(tt('scoreFileInvalid')); return; }
    setAbc(text.trimEnd());
    setError(null);
  };

  const insertTag = (tag: string) => {
    const box = lyricsBox.current;
    const at = box?.selectionStart ?? lyrics.length;
    const before = lyrics.slice(0, at);
    const after = lyrics.slice(at);
    const prefix = before && !before.endsWith('\n\n') ? (before.endsWith('\n') ? '\n' : '\n\n') : '';
    setLyrics(`${before}${prefix}${tag}\n${after}`);
    window.requestAnimationFrame(() => box?.focus());
  };

  /** Reads a recording into a score with SheetSage2, for a cover. */
  const runTranscription = async (source: { file?: File; songId?: string }, melodyOnly: boolean) => {
    setError(null);
    setTranscribing(source.file?.name ?? source.songId ?? '');
    try {
      const score = await transcribe(source, melodyOnly);
      setAbc(score.trimEnd());
      // The model card: covers take a melody-only score in melody mode.
      setCot(melodyOnly ? 'melody' : 'full');
      setSemanticTokens('');
      setShowNotation(true);
      setMode('studio');
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setTranscribing(null);
    }
  };

  const assistRun = useRef<AbortController | null>(null);
  const stopAssistant = () => {
    assistRun.current?.abort();
    assistRun.current = null;
    setAssisting(null);
    setAssistStage(null);
    setAssistDraft('');
  };

  const askAssistant = async (target: 'all' | 'lyrics' | 'style' | 'score') => {
    if (!assistantReady || assisting) return;
    const run = new AbortController();
    assistRun.current = run;
    setAssisting(target);
    setError(null);
    try {
      const payload = JSON.stringify({
        target,
        description: name.trim(),
        instruction: (target === 'score' ? scoreInstruction : assistInstruction).trim(),
        lyrics: lyrics.trim(),
        style: style.trim(),
        abc: abc.trim(),
        duration_seconds: numberOrUndefined(duration) ?? 120,
        instrumental,
      });
      setAssistStage('preparing');
      setAssistDraft('');
      let streamed = '';
      const live = await fetch('/v1/assistant/write/stream', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: payload,
        signal: run.signal,
      });
      if (live.ok && live.body) {
        const reader = live.body.getReader();
        const decoder = new TextDecoder();
        let carry = '';
        for (;;) {
          const { done, value } = await reader.read();
          if (done) break;
          carry += decoder.decode(value, { stream: true });
          let split = carry.indexOf('\n\n');
          while (split !== -1) {
            const frame = carry.slice(0, split).trim();
            carry = carry.slice(split + 2);
            split = carry.indexOf('\n\n');
            if (!frame.startsWith('data:')) continue;
            let event: { stage?: string; delta?: string; error?: string; model?: string };
            try {
              event = JSON.parse(frame.slice(5).trim());
            } catch {
              continue;
            }
            if (event.error) throw new Error(event.error);
            if (event.stage) setAssistStage(event.stage);
            if (event.model) setAssistModel(event.model);
            if (event.delta) {
              streamed += event.delta;
              setAssistDraft(streamed);
            }
          }
        }
      }
      const response = await fetch('/v1/assistant/write', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: payload,
      });
      const body = await response.json().catch(() => null);
      if (!response.ok) throw new Error(body?.error || String(response.status));
      if (typeof body?.lyrics === 'string') setLyrics(body.lyrics);
      if (typeof body?.style === 'string') setStyle(body.style);
      if (typeof body?.abc === 'string') setAbc(body.abc.trimEnd());
      if (typeof body?.title === 'string' && body.title.trim() && target !== 'score') setName(body.title.trim());
      if (typeof body?.cover_prompt === 'string' && body.cover_prompt.trim()) setCoverPrompt(body.cover_prompt.trim());
      if (typeof body?.duration_seconds === 'number' && body.duration_seconds >= 10 && target === 'all') {
        setDuration(String(Math.min(MAX_DURATION_SECONDS, Math.round(body.duration_seconds))));
      }
    } catch (reason) {
      const cancelled = reason instanceof DOMException && reason.name === 'AbortError';
      if (!cancelled) setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      assistRun.current = null;
      setAssisting(null);
      setAssistStage(null);
      setAssistDraft('');
    }
  };

  const submit = () => {
    if (!ready) { setError(t('downloadProfileFirst')); return; }
    if (!style.trim() && !lyrics.trim() && !semanticTokens.trim()) { setError(tt('styleOrLyricsRequired')); return; }
    setError(null);
    onGenerate(buildRequest());
  };

  const songs = numberOrUndefined(lmBatch) ?? 1;
  const variations = numberOrUndefined(synthBatch) ?? 1;
  const totalTracks = songs * variations;
  const durationFallback = Number(defaults.duration ?? MAX_DURATION_SECONDS);
  const formatDuration = (seconds: number) => `${Math.floor(seconds / 60)}:${String(Math.round(seconds % 60)).padStart(2, '0')}`;
  const scoreDisabled = effectiveCot === 'off';
  // The model card: melody mode does not strip chord symbols by itself.
  const melodyWithChords = effectiveCot === 'melody' && CHORD_SYMBOL.test(abc);

  return (
    <section className="flex h-full min-h-0 w-full flex-col overflow-hidden bg-zinc-50 text-zinc-900 dark:bg-suno-panel dark:text-white">
      <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain custom-scrollbar">
        <div className="space-y-3 p-4 pb-6">
          <div className="flex items-center justify-between gap-3">
            <div className="min-w-0">
              <h1 className="truncate text-base font-bold">{t('createMusic')}</h1>
              <p className="mt-0.5 truncate text-[11px] text-zinc-500 dark:text-zinc-400">{tt('localInferenceYue')}</p>
            </div>
            <span className={`shrink-0 rounded-full px-2.5 py-1 text-[10px] font-semibold ${ready ? 'bg-emerald-500/10 text-emerald-600 dark:text-emerald-300' : 'bg-amber-500/10 text-amber-700 dark:text-amber-300'}`}>
              <span className={`mr-1 inline-block h-1.5 w-1.5 rounded-full ${ready ? 'bg-emerald-500' : 'bg-amber-500'}`} />
              {serviceDown ? t('serviceUnavailable') : ready ? t('engineReady') : t('profileRequired')}
            </span>
          </div>

          {serviceDown ? (
            <div className="flex gap-2 rounded-xl border border-rose-500/30 bg-rose-500/10 p-3 text-xs leading-5 text-rose-700 dark:text-rose-200">
              <CircleAlert className="mt-0.5 shrink-0" size={15} />
              <div><b>{t('serviceUnavailable')}</b><br />{t('serviceUnavailableHint')}</div>
            </div>
          ) : !ready && (
            <div className="flex gap-2 rounded-xl border border-amber-500/25 bg-amber-500/10 p-3 text-xs leading-5 text-amber-800 dark:text-amber-200">
              <CircleAlert className="mt-0.5 shrink-0" size={15} />
              <div><b>{t('localGenerationUnavailable')}</b><br />{t('downloadProfileFirst')}</div>
            </div>
          )}

          <div className="flex items-center rounded-lg border border-zinc-300 bg-zinc-200 p-1 dark:border-white/5 dark:bg-black/40" role="tablist">
            {(['studio', 'cover', 'simple'] as const).map(value => (
              <button
                key={value}
                type="button"
                role="tab"
                aria-selected={mode === value}
                onClick={() => setMode(value)}
                className={`flex-1 rounded-md py-1.5 text-xs font-semibold transition-all ${mode === value ? 'bg-white text-black shadow-sm dark:bg-zinc-800 dark:text-white' : 'text-zinc-500 hover:text-zinc-900 dark:hover:text-zinc-300'}`}
              >
                {value === 'studio' ? t('studioMode') : value === 'cover' ? tt('coverMode') : t('simpleMode')}
              </button>
            ))}
          </div>

          {mode === 'simple' && !assistantReady && (
            <Card title={t('songIdea')}>
              <p className="text-xs leading-5 text-zinc-500 dark:text-zinc-400">{t('assistantNeedsModel')}</p>
              <p className="mt-2 text-[11px] leading-4 text-zinc-500">{t('assistantHint')}</p>
              <button
                type="button"
                onClick={() => window.dispatchEvent(new CustomEvent('yue:open-settings', { detail: 'models' }))}
                className="mt-3 inline-flex items-center gap-1 rounded-lg border border-zinc-300 px-3 py-1.5 text-xs font-medium text-zinc-600 hover:border-pink-400 hover:text-pink-600 dark:border-white/15 dark:text-zinc-300"
              >
                <Settings2 size={13} />
                {t('setUpAssistant')}
              </button>
            </Card>
          )}

          {mode === 'simple' && assistantReady && (
            <Card title={t('songIdea')}>
              <AutoTextarea
                value={assistInstruction}
                minRows={3}
                onChange={event => setAssistInstruction(event.target.value)}
                placeholder={t('songIdeaPlaceholder')}
                className={`${CONTROL} resize-none`}
              />
              <p className="mt-2 text-[11px] leading-4 text-zinc-500">{t('songIdeaHint')}</p>
              <button
                type="button"
                onClick={() => void askAssistant('all')}
                disabled={assisting !== null || !assistInstruction.trim()}
                className="mt-3 inline-flex w-full items-center justify-center gap-2 rounded-lg bg-gradient-to-r from-orange-500 to-pink-600 py-2.5 text-xs font-bold text-white transition hover:brightness-110 disabled:opacity-50"
              >
                {assisting === 'all' ? <Loader2 size={14} className="animate-spin" /> : <Wand2 size={14} />}
                {assisting === 'all' ? `${t('assistantWriting')} · ${assistSeconds} ${t('secondsShort')}` : t('writeEverything')}
              </button>
              {assisting === 'all' && (
                <button
                  type="button"
                  onClick={stopAssistant}
                  className="mt-2 inline-flex w-full items-center justify-center gap-2 rounded-lg border border-zinc-300 py-2 text-xs font-semibold text-zinc-600 transition-colors hover:border-rose-400 hover:text-rose-600 dark:border-white/15 dark:text-zinc-300"
                >
                  <Square size={13} />
                  {t('cancelDownload')}
                </button>
              )}
            </Card>
          )}

          {mode === 'cover' && (
            <Card title={tt('coverTitle')} icon={<AudioLines size={13} />}>
              <p className="text-xs leading-5 text-zinc-600 dark:text-zinc-300">{tt('coverIntro')}</p>
              <ol className="mt-3 space-y-1.5 text-[11px] leading-4 text-zinc-500">
                <li><b className="text-zinc-700 dark:text-zinc-200">1.</b> {tt('coverStep1')}</li>
                <li><b className="text-zinc-700 dark:text-zinc-200">2.</b> {tt('coverStep2')}</li>
                <li><b className="text-zinc-700 dark:text-zinc-200">3.</b> {tt('coverStep3')}</li>
              </ol>
              <div className="mt-3 border-t border-zinc-100 pt-3 dark:border-white/5">
                <Switch checked={coverMelodyOnly} onChange={setCoverMelodyOnly} label={tt('coverMelodyOnly')} hint={tt('coverMelodyOnlyHint')} />
              </div>
              {!transcriberReady && (
                <p className="mt-3 flex gap-1.5 rounded-lg bg-amber-500/10 p-2 text-[11px] leading-4 text-amber-700 dark:text-amber-300">
                  <AlertTriangle size={13} className="mt-0.5 shrink-0" />
                  {tt('transcriberMissing')}
                </p>
              )}
              <input
                ref={audioFile}
                type="file"
                accept="audio/*,.mp3,.wav,.flac,.ogg,.m4a"
                className="hidden"
                onChange={event => {
                  const file = event.target.files?.[0];
                  event.target.value = '';
                  if (file) { setCoverSource(file.name); void runTranscription({ file }, coverMelodyOnly); }
                }}
              />
              <button
                type="button"
                onClick={() => audioFile.current?.click()}
                disabled={!transcriberReady || transcribing !== null}
                className="mt-3 inline-flex w-full items-center justify-center gap-2 rounded-lg bg-gradient-to-r from-orange-500 to-pink-600 py-2.5 text-xs font-bold text-white transition hover:brightness-110 disabled:opacity-50"
              >
                {transcribing !== null ? <Loader2 size={14} className="animate-spin" /> : <Upload size={14} />}
                {transcribing !== null ? `${tt('transcribing')} · ${transcribing}` : tt('coverPickRecording')}
              </button>
              <p className="mt-2 text-[11px] leading-4 text-zinc-500">{tt('coverFromLibraryHint')}</p>
              {coverSource && !transcribing && abc && (
                <p className="mt-2 text-[11px] text-emerald-600 dark:text-emerald-300">{tt('coverScoreReady')} · {coverSource}</p>
              )}
            </Card>
          )}

          {activity.filter(entry => entry.state !== 'done').slice(-3).map(entry => (
            <div key={`${entry.song_id}-${entry.kind}`} className="rounded-xl border border-zinc-200 bg-white px-3 py-2 text-[11px] dark:border-white/10 dark:bg-suno-card">
              <div className="flex items-center gap-2">
                {entry.state === 'running'
                  ? <Loader2 size={12} className="animate-spin text-pink-500" />
                  : <AlertTriangle size={12} className="text-amber-500" />}
                <span className="font-semibold text-zinc-700 dark:text-zinc-200">
                  {entry.kind === 'cover' ? t('activityCover') : t('activityKaraoke')}
                </span>
                <span className="min-w-0 flex-1 truncate text-zinc-500">{entry.title}</span>
              </div>
              {entry.detail && <p className="mt-1 break-words text-[11px] leading-4 text-amber-600 dark:text-amber-300">{karaokeReason(t, entry.detail)}</p>}
            </div>
          ))}

          {assisting !== null && (
            <div className="rounded-xl border border-zinc-200 bg-white p-3 dark:border-white/10 dark:bg-suno-card">
              <div className="flex items-center justify-between gap-2 text-[11px] font-semibold uppercase tracking-wide">
                <span className="flex items-center gap-1.5 text-pink-600 dark:text-pink-300">
                  <Loader2 size={12} className="animate-spin" />
                  {assistStage === 'sent' ? t('assistStageSent') : assistStage === 'writing' ? t('assistStageWriting') : assistStage === 'done' ? t('assistStageDone') : t('assistStagePreparing')}
                </span>
                <span className="flex items-center gap-2">
                  <span className="tabular-nums text-zinc-400">{assistSeconds} {t('secondsShort')}</span>
                  <button type="button" onClick={stopAssistant} className={ICON} title={t('cancelDownload')}><X size={13} /></button>
                </span>
              </div>
              {assistModel && <p className="mt-1 truncate text-[11px] text-zinc-500">{assistModel}</p>}
              {assistDraft && (
                <pre className="mt-2 max-h-40 overflow-y-auto whitespace-pre-wrap break-words rounded-lg bg-zinc-50 p-2 font-mono text-[11px] leading-4 text-zinc-600 dark:bg-black/30 dark:text-zinc-300">
                  {assistDraft.slice(-1200)}
                </pre>
              )}
            </div>
          )}

          <Card
            title={tt('styleCardTitle')}
            icon={<Music2 size={13} />}
            actions={
              <>
                {assistantReady && (
                  <button type="button" onClick={() => void askAssistant('style')} disabled={assisting !== null} className={ICON} title={tt('writeStyle')}>
                    {assisting === 'style' ? <Loader2 size={14} className="animate-spin" /> : <Wand2 size={14} className="text-pink-500" />}
                  </button>
                )}
                <button type="button" onClick={() => setExamplesOpen(open => !open)} className={ICON} title={t('examplePrompt')} aria-expanded={examplesOpen}><Dices size={14} /></button>
                <button type="button" onClick={() => promptFile.current?.click()} className={ICON} title={t('openPrompt')}><FolderOpen size={14} /></button>
                <button type="button" onClick={savePrompt} className={ICON} title={t('savePrompt')}><Save size={14} /></button>
                <button type="button" onClick={reset} className={ICON} title={t('resetPrompt')}><RotateCcw size={14} /></button>
                <input
                  ref={promptFile}
                  type="file"
                  accept="application/json,.json"
                  className="hidden"
                  onChange={event => { const file = event.target.files?.[0]; if (file) void openPrompt(file); event.target.value = ''; }}
                />
              </>
            }
          >
            {examplesOpen && (
              <div className="mb-3 rounded-lg border border-zinc-200 bg-zinc-50 p-2 dark:border-white/10 dark:bg-black/25">
                <div className="mb-2 flex items-center justify-between">
                  <span className="text-[10px] font-semibold uppercase tracking-wide text-zinc-500">{tt('officialExamples')} · {EXAMPLES.length}</span>
                  <button type="button" onClick={() => loadExample()} className="inline-flex items-center gap-1 text-[11px] font-semibold text-pink-600 hover:text-pink-500 dark:text-pink-300"><Dices size={12} />{tt('randomExample')}</button>
                </div>
                <div className="grid max-h-52 grid-cols-2 gap-1 overflow-y-auto custom-scrollbar">
                  {EXAMPLES.map(example => (
                    <button
                      key={example.id}
                      type="button"
                      onClick={() => loadExample(example.id)}
                      className="truncate rounded-md px-2 py-1 text-left text-[11px] text-zinc-600 transition-colors hover:bg-white hover:text-black dark:text-zinc-300 dark:hover:bg-white/10 dark:hover:text-white"
                      title={example.style}
                    >
                      {example.cover && <span className="mr-1 rounded bg-pink-500/15 px-1 text-[9px] font-bold uppercase text-pink-600 dark:text-pink-300">{tt('coverBadge')}</span>}
                      {example.title}
                    </button>
                  ))}
                </div>
              </div>
            )}
            <input
              value={name}
              onChange={event => setName(event.target.value)}
              placeholder={t('untitled')}
              aria-label={t('untitled')}
              className="w-full border-0 bg-transparent p-0 text-lg font-bold text-zinc-900 outline-none placeholder:text-zinc-300 dark:text-white dark:placeholder:text-zinc-600"
            />
            <AutoTextarea
              value={style}
              minRows={3}
              onChange={event => setStyle(event.target.value)}
              placeholder={tt('stylePlaceholder')}
              aria-label={tt('styleCardTitle')}
              className={`${CONTROL} mt-3 resize-none leading-5`}
            />
            <p className="mt-2 text-[11px] leading-4 text-zinc-500">{tt('styleHint')}</p>
          </Card>

          <Card
            title={t('lyrics')}
            actions={
              <>
                {assistantReady && (
                  <button type="button" onClick={() => void askAssistant('lyrics')} disabled={assisting !== null} className={ICON} title={t('writeLyrics')}>
                    {assisting === 'lyrics' ? <Loader2 size={14} className="animate-spin" /> : <Wand2 size={14} className="text-pink-500" />}
                  </button>
                )}
                <button type="button" onClick={() => setLyrics('')} className={ICON} title={t('resetPrompt')}><RotateCcw size={14} /></button>
              </>
            }
          >
            <div className="mb-3 border-b border-zinc-100 pb-3 dark:border-white/5">
              <Switch checked={instrumental} onChange={setInstrumental} label={t('instrumental')} hint={tt('instrumentalHintYue')} />
            </div>
            <div className="mb-2 flex flex-wrap gap-1">
              {SECTION_TAGS.map(tag => (
                <button key={tag} type="button" onClick={() => insertTag(tag)} disabled={instrumental} className={CHIP}>{tag}</button>
              ))}
            </div>
            <AutoTextarea
              value={lyrics}
              minRows={10}
              onChange={event => setLyrics(event.target.value)}
              onFocus={event => { lyricsBox.current = event.currentTarget; }}
              disabled={instrumental}
              placeholder={'[Verse 1]\n…\n\n[Chorus]\n…'}
              aria-label={t('lyrics')}
              className={`${CONTROL} resize-none font-mono text-xs leading-5`}
            />
            <p className="mt-2 text-[11px] leading-4 text-zinc-500">{tt('lyricsHintYue')}</p>
          </Card>

          <Card
            title={tt('scoreCardTitle')}
            icon={<FileMusic size={13} />}
            actions={
              <>
                <button type="button" onClick={() => setShowNotation(value => !value)} className={ICON} title={showNotation ? tt('hideNotation') : tt('showNotation')}>
                  {showNotation ? <EyeOff size={14} /> : <Eye size={14} />}
                </button>
                <button type="button" onClick={() => scoreFile.current?.click()} className={ICON} title={tt('openScore')}><FolderOpen size={14} /></button>
                <button type="button" onClick={() => abc.trim() && download(`${safeName()}.abc`, `${abc.trim()}\n`, 'text/vnd.abc')} disabled={!abc.trim()} className={ICON} title={tt('saveScore')}><Save size={14} /></button>
                <button type="button" onClick={() => setAbc('')} disabled={!abc} className={ICON} title={tt('clearScore')}><X size={14} /></button>
                <input
                  ref={scoreFile}
                  type="file"
                  accept=".abc,text/plain"
                  className="hidden"
                  onChange={event => { const file = event.target.files?.[0]; if (file) void openScore(file); event.target.value = ''; }}
                />
              </>
            }
          >
            <div className="grid grid-cols-3 gap-1 rounded-lg bg-zinc-100 p-1 dark:bg-black/30" role="radiogroup" aria-label={tt('cotMode')}>
              {(['full', 'melody', 'off'] as const).map(value => (
                <button
                  key={value}
                  type="button"
                  role="radio"
                  aria-checked={effectiveCot === value}
                  onClick={() => setCot(value)}
                  className={`rounded-md px-2 py-1.5 text-[11px] font-semibold transition-all ${effectiveCot === value ? 'bg-white text-black shadow-sm dark:bg-zinc-700 dark:text-white' : 'text-zinc-500 hover:text-zinc-800 dark:hover:text-zinc-200'}`}
                >
                  {tt(value === 'full' ? 'cotFull' : value === 'melody' ? 'cotMelody' : 'cotOff')}
                </button>
              ))}
            </div>
            <p className="mt-2 text-[11px] leading-4 text-zinc-500">
              {tt(effectiveCot === 'full' ? 'cotFullHint' : effectiveCot === 'melody' ? 'cotMelodyHint' : 'cotOffHint')}
            </p>
            {!scoreDisabled && (
              <>
                {showNotation && abc.trim() && (
                  <div className="mt-3 max-h-80 overflow-auto rounded-lg border border-zinc-200 bg-white p-2 dark:border-white/10 custom-scrollbar">
                    <ScoreView abc={abc} />
                  </div>
                )}
                <AutoTextarea
                  value={abc}
                  minRows={5}
                  onChange={event => setAbc(event.target.value)}
                  placeholder={tt('scorePlaceholder')}
                  aria-label={tt('scoreCardTitle')}
                  spellCheck={false}
                  className={`${CONTROL} mt-3 resize-none font-mono text-[11px] leading-4`}
                />
                <p className="mt-2 text-[11px] leading-4 text-zinc-500">{tt('scoreHint')}</p>
                {melodyWithChords && (
                  <div className="mt-2 flex items-start gap-2 rounded-lg bg-amber-500/10 p-2 text-[11px] leading-4 text-amber-700 dark:text-amber-300">
                    <AlertTriangle size={13} className="mt-0.5 shrink-0" />
                    <span className="flex-1">{tt('melodyScoreHasChords')}</span>
                    <button type="button" onClick={() => setAbc(current => current.replace(CHORD_SYMBOLS, ''))} className="shrink-0 font-semibold underline decoration-dotted underline-offset-2">{tt('stripChords')}</button>
                  </div>
                )}
                {assistantReady && abc.trim() && (
                  <div className="mt-3 rounded-lg border border-pink-500/20 bg-pink-500/5 p-2">
                    <span className={LABEL}>{tt('scoreEditTitle')}</span>
                    <div className="flex gap-2">
                      <input
                        value={scoreInstruction}
                        onChange={event => setScoreInstruction(event.target.value)}
                        onKeyDown={event => { if (event.key === 'Enter' && scoreInstruction.trim()) void askAssistant('score'); }}
                        placeholder={tt('scoreEditPlaceholder')}
                        className={CONTROL}
                      />
                      <button
                        type="button"
                        onClick={() => void askAssistant('score')}
                        disabled={assisting !== null || !scoreInstruction.trim()}
                        className="inline-flex shrink-0 items-center gap-1.5 rounded-lg bg-pink-600 px-3 text-xs font-bold text-white transition hover:brightness-110 disabled:opacity-50"
                      >
                        {assisting === 'score' ? <Loader2 size={13} className="animate-spin" /> : <Wand2 size={13} />}
                        {tt('scoreEditApply')}
                      </button>
                    </div>
                  </div>
                )}
              </>
            )}
          </Card>

          <Card
            title={t('quality')}
            actions={
              <button type="button" onClick={resetParameters} className="rounded-md px-2 py-1 text-[10px] font-semibold text-zinc-500 transition-colors hover:bg-zinc-200 hover:text-black dark:hover:bg-white/10 dark:hover:text-white">
                {t('resetToDefaults')}
              </button>
            }
          >
            <div className="space-y-3">
              <SliderRow
                label={t('maxDuration')}
                value={duration}
                fallback={durationFallback}
                min={10}
                max={MAX_DURATION_SECONDS}
                step={5}
                onChange={setDuration}
                format={formatDuration}
              />
              <p className="text-[11px] leading-4 text-zinc-500">{tt('maxDurationHintYue')}</p>
              <SliderRow
                label={tt('flowSteps')}
                value={steps}
                fallback={Number(defaults.steps ?? 32)}
                min={4}
                max={100}
                step={1}
                onChange={setSteps}
              />
            </div>
            <div className="mt-4 space-y-3 border-t border-zinc-100 pt-4 dark:border-white/5">
              <SliderRow
                label={tt('songsPerRequest')}
                value={lmBatch}
                fallback={1}
                min={1}
                max={Math.max(maxBatch, 1)}
                step={1}
                onChange={setLmBatch}
                disabled={maxBatch <= 1}
              />
              {maxBatch <= 1 && <p className="text-[11px] leading-4 text-zinc-500">{tt('songsPerRequestHint')}</p>}
              <SliderRow
                label={t('variationsBatch')}
                value={synthBatch}
                fallback={1}
                min={1}
                max={9}
                step={1}
                onChange={setSynthBatch}
              />
              <Switch checked={randomizeSeed} onChange={setRandomizeSeed} label={t('randomizeSeed')} />
              {!randomizeSeed && (
                <div className="grid grid-cols-2 gap-2">
                  <Field label={tt('lmSeedYue')}>
                    <input value={lmSeed} onChange={event => setLmSeed(event.target.value)} placeholder="-1" inputMode="numeric" className={CONTROL} />
                  </Field>
                  <Field label={tt('noiseSeed')}>
                    <input value={seed} onChange={event => setSeed(event.target.value)} placeholder="-1" inputMode="numeric" className={CONTROL} />
                  </Field>
                </div>
              )}
              {totalTracks > 1 && (
                <p className="text-[11px] text-zinc-500">{t('renderCountPrefix')} <b className="text-zinc-700 dark:text-zinc-200">{totalTracks}</b></p>
              )}
            </div>
          </Card>

          <div className="overflow-hidden rounded-xl border border-zinc-200 bg-white dark:border-white/5 dark:bg-suno-card">
            <button
              type="button"
              onClick={() => setShowAdvanced(current => !current)}
              className="flex w-full items-center justify-between gap-2 px-3 py-2 text-[11px] font-bold uppercase tracking-wide text-zinc-500 transition-colors hover:text-black dark:text-zinc-400 dark:hover:text-white"
              aria-expanded={showAdvanced}
            >
              {t('advanced')}
              <ChevronDown size={15} className={showAdvanced ? 'rotate-180 transition-transform' : 'transition-transform'} />
            </button>
            {showAdvanced && (
              <div className="space-y-4 border-t border-zinc-100 p-3 dark:border-white/5">
                <Stage title={tt('stageScoreSampling')} hint={tt('stageScoreSamplingHint')}>
                  <SamplingGrid value={abcSampling} defaults={defaults.abc_sampling} onChange={setAbcSampling} t={t as never} />
                </Stage>

                <div className="border-t border-zinc-100 pt-4 dark:border-white/5">
                  <Stage title={tt('stageSemantic')} hint={tt('stageSemanticHint')}>
                    <Field label={tt('guidanceScale')} hint={tt('guidanceScaleHint')}>
                      <input value={cfgScale} onChange={event => setCfgScale(event.target.value)} placeholder={tt('guidanceAuto')} inputMode="decimal" className={CONTROL} />
                    </Field>
                    <div className="mt-3">
                      <SamplingGrid value={semanticSampling} defaults={defaults.semantic_sampling} onChange={setSemanticSampling} t={t as never} />
                    </div>
                    <div className="mt-3">
                      <Field label={tt('semanticTokens')} hint={tt('semanticTokensHint')}>
                        <AutoTextarea
                          value={semanticTokens}
                          minRows={2}
                          onChange={event => setSemanticTokens(event.target.value)}
                          placeholder="12046,8433,22418,…"
                          spellCheck={false}
                          className={`${CONTROL} resize-none font-mono text-[11px]`}
                        />
                      </Field>
                    </div>
                  </Stage>
                </div>

                <div className="border-t border-zinc-100 pt-4 dark:border-white/5">
                  <Stage title={t('stageOutput')} hint={t('stageOutputHint')}>
                    <SliderRow
                      label={t('peakClipLabel')}
                      value={peakClip}
                      fallback={Number(defaults.peak_clip ?? 10)}
                      min={0}
                      max={30}
                      step={1}
                      onChange={setPeakClip}
                    />
                    <div className="mt-3 grid grid-cols-2 gap-2">
                      <Field label={t('mp3Bitrate')}>
                        <select value={mp3Bitrate || String(defaults.mp3_bitrate ?? 128)} onChange={event => setMp3Bitrate(event.target.value)} disabled={format !== 'mp3'} className={CONTROL}>
                          {['128', '192', '256', '320'].map(rate => <option key={rate} value={rate}>{rate} kbps</option>)}
                        </select>
                      </Field>
                      <Field label={t('outputFormat')}>
                        <select value={format} onChange={event => setFormat(event.target.value as YueOutputFormat)} className={CONTROL}>
                          <option value="mp3">MP3</option>
                          <option value="wav16">WAV 16-bit</option>
                          <option value="wav24">WAV 24-bit</option>
                          <option value="wav32">WAV 32-bit float</option>
                        </select>
                      </Field>
                    </div>
                    <p className="mt-2 text-[11px] leading-4 text-zinc-500">{t('peakClipHint')}</p>
                  </Stage>
                </div>
              </div>
            )}
          </div>

          <div className="flex items-center justify-between px-1 text-[11px] text-zinc-500 dark:text-zinc-400">
            <button
              type="button"
              onClick={() => window.dispatchEvent(new CustomEvent('yue:open-settings', { detail: 'models' }))}
              className="text-left hover:text-pink-500"
              title={t('changeProfileHint')}
            >
              {t('profile')}: <b className="text-zinc-700 underline decoration-dotted underline-offset-2 dark:text-zinc-200">{profileLabel}</b>
            </button>
            {catalog?.version && <span className="font-mono text-[10px] text-zinc-400">yue2.cpp {catalog.version.split(' ')[0]}</span>}
          </div>
          {setup?.hardware?.reason && <p className="px-1 text-[10px] text-zinc-400">{setup.hardware.reason}</p>}
          {error && <div role="alert" className="rounded-xl border border-red-500/30 bg-red-500/10 p-3 text-xs leading-5 text-red-700 dark:text-red-200">{error}</div>}
        </div>
      </div>

      <footer className="shrink-0 border-t border-zinc-200 bg-zinc-50/95 p-4 backdrop-blur dark:border-white/5 dark:bg-suno-panel/95">
        <button
          type="button"
          onClick={submit}
          disabled={activeJobCount >= 10}
          className="flex h-12 w-full items-center justify-center gap-2 rounded-xl bg-gradient-to-r from-orange-500 to-pink-600 text-base font-bold text-white shadow-lg transition hover:brightness-110 disabled:cursor-not-allowed disabled:opacity-50"
        >
          {isGenerating ? <Square size={18} /> : <Sparkles size={18} />}
          {t('create')}
          {activeJobCount > 0 && <span className="rounded-full bg-white/20 px-2 py-0.5 text-xs">{activeJobCount}/10</span>}
        </button>
      </footer>
    </section>
  );
};
