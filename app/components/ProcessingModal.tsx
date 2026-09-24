import React, { useEffect, useMemo, useRef, useState } from 'react';
import { AlertTriangle, Check, FolderOpen, Loader2, Pause, Play, Search, Wand2, X } from 'lucide-react';
import { Song } from '../types';
import { useI18n } from '../context/I18nContext';
import { apiUrl } from '../services/apiBase';
import { activeVersionLabel } from '../services/songDownload';

/**
 * Processing a finished track: noise reduction, the Spectral Lifter, vocal
 * naturalising and mastering to a reference. The result is a preview heard
 * against the original, kept as a version of the track or thrown away; the
 * original never goes.
 */

interface ProcessingModalProps {
  song: Song;
  onClose: () => void;
  onKept: () => void;
}

interface Run {
  song_id: string;
  stages: string[];
  stage: string | null;
  done: boolean;
  error: string | null;
  preview_ready: boolean;
}

interface LibraryEntry {
  id: string;
  title: string;
  audio_path?: string | null;
}

const CARD = 'rounded-xl border border-zinc-200 p-3 dark:border-white/10';
const CONTROL =
  'w-full rounded-lg border border-zinc-200 bg-white px-3 py-2 text-sm text-zinc-900 outline-none focus:border-pink-500 dark:border-white/10 dark:bg-black/20 dark:text-white';

const Toggle: React.FC<{ checked: boolean; onChange: (value: boolean) => void; label: string; hint: string }> = ({ checked, onChange, label, hint }) => (
  <div className="flex items-start justify-between gap-3">
    <div className="min-w-0">
      <p className="text-sm font-semibold text-zinc-900 dark:text-white">{label}</p>
      <p className="mt-0.5 text-[11px] leading-4 text-zinc-500">{hint}</p>
    </div>
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      onClick={() => onChange(!checked)}
      className={`relative mt-0.5 h-5 w-10 shrink-0 rounded-full transition-colors ${checked ? 'bg-pink-500' : 'bg-zinc-300 dark:bg-zinc-600'}`}
    >
      <span className={`absolute top-[2px] h-4 w-4 rounded-full bg-white shadow-sm transition-all ${checked ? 'left-[22px]' : 'left-[2px]'}`} />
    </button>
  </div>
);

const Slider: React.FC<{ label: string; value: number; min: number; max: number; step: number; onChange: (value: number) => void; format?: (value: number) => string }> = ({ label, value, min, max, step, onChange, format }) => (
  <div className="mt-2">
    <div className="flex items-baseline justify-between text-[11px] text-zinc-600 dark:text-zinc-300">
      <span>{label}</span>
      <span className="tabular-nums">{format ? format(value) : value.toFixed(2)}</span>
    </div>
    <input type="range" min={min} max={max} step={step} value={value} aria-label={label} onChange={event => onChange(Number(event.target.value))} className="mt-1 h-1 w-full cursor-pointer accent-pink-500" />
  </div>
);

export const ProcessingModal: React.FC<ProcessingModalProps> = ({ song, onClose, onKept }) => {
  const { t } = useI18n();
  const tt = t as unknown as (key: string) => string;

  const [denoiseOn, setDenoiseOn] = useState(false);
  const [denoiseStrength, setDenoiseStrength] = useState(0.4);
  const [lifterOn, setLifterOn] = useState(false);
  const [lifterGate, setLifterGate] = useState(0.3);
  const [shimmer, setShimmer] = useState(6);
  const [highBand, setHighBand] = useState(0);
  const [punch, setPunch] = useState(0);
  const [naturalizeOn, setNaturalizeOn] = useState(false);
  const [naturalizeAmount, setNaturalizeAmount] = useState(0.5);
  const [masterOn, setMasterOn] = useState(false);
  const [referenceMode, setReferenceMode] = useState<'library' | 'file'>('library');
  const [referenceSong, setReferenceSong] = useState<LibraryEntry | null>(null);
  const [upload, setUpload] = useState<{ upload_id: string; name: string } | null>(null);
  const [uploading, setUploading] = useState(false);
  const [library, setLibrary] = useState<LibraryEntry[]>([]);
  const [query, setQuery] = useState('');

  const [run, setRun] = useState<Run | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [keeping, setKeeping] = useState(false);
  const kept = useRef(false);
  const filePicker = useRef<HTMLInputElement | null>(null);

  useEffect(() => {
    void fetch('/v1/library/songs')
      .then(response => response.json())
      .then((body: LibraryEntry[] | { songs: LibraryEntry[] }) => {
        const list = Array.isArray(body) ? body : body.songs ?? [];
        setLibrary(list.filter(entry => entry.audio_path && entry.id !== song.id));
      })
      .catch(() => undefined);
  }, [song.id]);

  // polling while a run works
  const running = Boolean(run && !run.done);
  useEffect(() => {
    if (!running) return;
    const timer = window.setInterval(() => {
      void fetch('/v1/processing')
        .then(response => response.json())
        .then((body: { run: Run | null }) => setRun(body.run))
        .catch(() => undefined);
    }, 500);
    return () => window.clearInterval(timer);
  }, [running]);

  // a preview nobody kept goes when the window closes
  useEffect(() => () => {
    if (!kept.current) void fetch('/v1/processing/discard', { method: 'POST' }).catch(() => undefined);
  }, []);

  const visible = useMemo(() => {
    const needle = query.trim().toLowerCase();
    return (needle ? library.filter(entry => entry.title.toLowerCase().includes(needle)) : library).slice(0, 60);
  }, [library, query]);

  const reference = referenceMode === 'library'
    ? referenceSong && { type: 'song', song_id: referenceSong.id }
    : upload && { type: 'upload', upload_id: upload.upload_id };
  const referenceTitle = referenceMode === 'library' ? referenceSong?.title : upload?.name;
  const nothing = !denoiseOn && !lifterOn && !naturalizeOn && !masterOn;
  const ready = !nothing && (!masterOn || Boolean(reference));

  const uploadReference = async (file: File | undefined) => {
    if (!file) return;
    setUploading(true);
    setError(null);
    try {
      const form = new FormData();
      form.append('audio', file, file.name);
      const response = await fetch('/v1/processing/reference', { method: 'POST', body: form });
      const body = await response.json().catch(() => ({}));
      if (!response.ok) throw new Error(body.error || `HTTP ${response.status}`);
      setUpload(body);
    } catch (problem) {
      setError(problem instanceof Error ? problem.message : String(problem));
    } finally {
      setUploading(false);
      if (filePicker.current) filePicker.current.value = '';
    }
  };

  const start = async () => {
    setError(null);
    const request: Record<string, unknown> = {};
    if (denoiseOn) request.denoise = { strength: denoiseStrength };
    if (lifterOn) request.lifter = { denoise_strength: lifterGate, shimmer_reduction_db: shimmer, hf_mix: highBand, transient_boost: punch };
    if (naturalizeOn) request.naturalize = { amount: naturalizeAmount };
    if (masterOn && reference) request.master = reference;
    const response = await fetch(`/v1/library/songs/${encodeURIComponent(song.id)}/process`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(request),
    });
    const body = await response.json().catch(() => ({}));
    if (!response.ok) {
      setError(body.error || `HTTP ${response.status}`);
      return;
    }
    setRun({ song_id: song.id, stages: [], stage: null, done: false, error: null, preview_ready: false });
  };

  // names the version, its downloaded file and the line above the comparison
  const label = () => {
    const parts: string[] = [];
    if (denoiseOn) parts.push(`${t('processDenoise')} ${denoiseStrength.toFixed(2)}`);
    if (lifterOn) {
      const values = [lifterGate.toFixed(2), `${shimmer.toFixed(1)} dB`];
      if (highBand > 0) values.push(`${t('processHighBand')} ${highBand.toFixed(2)}`);
      if (punch > 0) values.push(`${t('processPunch')} ${punch.toFixed(2)}`);
      parts.push(`${t('processLifter')} (${values.join(', ')})`);
    }
    if (naturalizeOn) parts.push(`${t('processNaturalize')} ${naturalizeAmount.toFixed(2)}`);
    if (masterOn) parts.push(referenceTitle ? `${t('processStage_master')} · ${referenceTitle}` : t('processStage_master'));
    // a processed version processed again carries its whole chain
    const earlier = activeVersionLabel(song);
    return [earlier, parts.join(' + ')].filter(Boolean).join(' → ');
  };

  const keep = async () => {
    setKeeping(true);
    try {
      const response = await fetch('/v1/processing/keep', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ label: label() }),
      });
      const body = await response.json().catch(() => ({}));
      if (!response.ok) throw new Error(body.error || `HTTP ${response.status}`);
      kept.current = true;
      window.dispatchEvent(new CustomEvent('yue:toast', { detail: { message: t('processKept'), type: 'success' } }));
      onKept();
      onClose();
    } catch (problem) {
      setError(problem instanceof Error ? problem.message : String(problem));
    } finally {
      setKeeping(false);
    }
  };

  const preview = Boolean(run?.done && run.preview_ready);
  const failed = run?.done && run.error;
  // one address per preview, fixed when it arrives, so re-renders do not reload it
  const previewUrl = useMemo(() => (preview ? apiUrl(`/v1/processing/preview?t=${Date.now()}`) : ''), [preview]);

  return (
    <div className="fixed inset-0 z-[70] flex items-center justify-center bg-black/60 p-4" onClick={onClose}>
      <div className="flex max-h-[92vh] w-full max-w-lg flex-col overflow-hidden rounded-2xl bg-white shadow-2xl dark:bg-zinc-900" onClick={event => event.stopPropagation()}>
        <div className="flex items-center justify-between border-b border-zinc-200 px-5 py-4 dark:border-white/10">
          <h3 className="flex items-center gap-2 text-base font-bold text-zinc-900 dark:text-white">
            <Wand2 size={17} className="text-pink-500" /> {t('processTitle')}
          </h3>
          <button type="button" onClick={onClose} className="text-zinc-400 hover:text-zinc-600 dark:hover:text-zinc-200" aria-label={t('cancel')}>
            <X size={18} />
          </button>
        </div>

        <div className="space-y-3 overflow-y-auto p-5">
          <div className="min-w-0">
            <p className="truncate text-sm font-semibold text-zinc-900 dark:text-white">{song.title}</p>
            {activeVersionLabel(song) && <p className="truncate text-[11px] text-zinc-500" title={activeVersionLabel(song) ?? ''}>{activeVersionLabel(song)}</p>}
          </div>
          <p className="text-xs leading-5 text-zinc-500 dark:text-zinc-400">{t('processHint')}</p>

          {!preview && (
            <fieldset disabled={running} className="space-y-3 disabled:opacity-60">
              <section className={CARD}>
                <Toggle checked={denoiseOn} onChange={setDenoiseOn} label={t('processDenoise')} hint={t('processDenoiseHint')} />
                {denoiseOn && <Slider label={t('processStrength')} value={denoiseStrength} min={0.05} max={1} step={0.05} onChange={setDenoiseStrength} />}
              </section>

              <section className={CARD}>
                <Toggle checked={lifterOn} onChange={setLifterOn} label={t('processLifter')} hint={t('processLifterHint')} />
                {lifterOn && (
                  <>
                    <Slider label={t('processLifterGate')} value={lifterGate} min={0} max={1} step={0.05} onChange={setLifterGate} />
                    <Slider label={t('processShimmer')} value={shimmer} min={0} max={12} step={0.5} onChange={setShimmer} format={value => `${value.toFixed(1)} dB`} />
                    <Slider label={t('processHighBand')} value={highBand} min={0} max={0.5} step={0.05} onChange={setHighBand} />
                    <Slider label={t('processPunch')} value={punch} min={0} max={1} step={0.05} onChange={setPunch} />
                  </>
                )}
              </section>

              <section className={CARD}>
                <Toggle checked={naturalizeOn} onChange={setNaturalizeOn} label={t('processNaturalize')} hint={t('processNaturalizeHint')} />
                {naturalizeOn && <Slider label={t('processAmount')} value={naturalizeAmount} min={0.05} max={1} step={0.05} onChange={setNaturalizeAmount} />}
              </section>

              <section className={CARD}>
                <Toggle checked={masterOn} onChange={setMasterOn} label={t('processMaster')} hint={t('processMasterHint')} />
                {masterOn && (
                  <div className="mt-3 space-y-2">
                    <div role="tablist" className="flex rounded-lg bg-zinc-100 p-1 dark:bg-white/5">
                      {(['library', 'file'] as const).map(mode => (
                        <button
                          key={mode}
                          type="button"
                          role="tab"
                          aria-selected={referenceMode === mode}
                          onClick={() => setReferenceMode(mode)}
                          className={`flex-1 rounded-md py-1 text-xs font-semibold transition-all ${referenceMode === mode ? 'bg-white text-black shadow-sm dark:bg-zinc-800 dark:text-white' : 'text-zinc-500 hover:text-zinc-900 dark:hover:text-zinc-300'}`}
                        >
                          {mode === 'library' ? t('processReferenceLibrary') : t('processReferenceFile')}
                        </button>
                      ))}
                    </div>
                    {referenceMode === 'library' ? (
                      <div>
                        <div className="relative">
                          <Search size={14} className="pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-zinc-400" />
                          <input value={query} onChange={event => setQuery(event.target.value)} placeholder={t('processReferenceSearch')} className={`${CONTROL} pl-9`} />
                        </div>
                        <div className="mt-2 max-h-40 overflow-y-auto rounded-lg border border-zinc-200 dark:border-white/10">
                          {visible.map(entry => (
                            <button
                              key={entry.id}
                              type="button"
                              onClick={() => setReferenceSong(entry)}
                              className={`flex w-full items-center gap-2 border-b border-zinc-100 px-3 py-1.5 text-left text-sm last:border-b-0 dark:border-white/5 ${referenceSong?.id === entry.id ? 'bg-pink-500/10' : 'hover:bg-zinc-100 dark:hover:bg-white/5'}`}
                            >
                              <span className="min-w-0 flex-1 truncate text-zinc-800 dark:text-zinc-200">{entry.title}</span>
                              {referenceSong?.id === entry.id && <Check size={14} className="shrink-0 text-pink-500" />}
                            </button>
                          ))}
                        </div>
                      </div>
                    ) : (
                      <div className="flex items-center gap-2">
                        <button type="button" onClick={() => filePicker.current?.click()} disabled={uploading} className="inline-flex shrink-0 items-center gap-1.5 rounded-lg border border-zinc-200 px-3 py-1.5 text-xs font-semibold text-zinc-700 hover:border-pink-400 hover:text-pink-600 dark:border-white/10 dark:text-zinc-200">
                          {uploading ? <Loader2 size={13} className="animate-spin" /> : <FolderOpen size={13} />}
                          {t('processReferenceChoose')}
                        </button>
                        <span className="min-w-0 truncate text-xs text-zinc-600 dark:text-zinc-300">{upload?.name}</span>
                        <input ref={filePicker} type="file" accept=".mp3,.wav,.flac,.ogg,.m4a,audio/*" className="hidden" onChange={event => void uploadReference(event.target.files?.[0])} />
                      </div>
                    )}
                    {!reference && <p className="text-[11px] text-amber-600 dark:text-amber-300">{t('processReferenceNone')}</p>}
                  </div>
                )}
              </section>
            </fieldset>
          )}

          {running && (
            <p className="flex items-center gap-2 text-sm text-zinc-700 dark:text-zinc-200">
              <Loader2 size={15} className="animate-spin text-pink-500" />
              {run?.stage ? tt(`processStage_${run.stage}`) : t('processTitle')}
            </p>
          )}

          {preview && (
            <>
              <p className="text-xs leading-5 text-zinc-700 dark:text-zinc-200">
                <span className="font-semibold">{t('processApplied')}:</span> {label()}
              </p>
              <Compare original={song.audioUrl ?? ''} processed={previewUrl} />
            </>
          )}

          {(error || failed) && (
            <p role="alert" className="flex items-center gap-2 rounded-lg bg-rose-500/10 px-3 py-2 text-xs text-rose-700 dark:text-rose-300">
              <AlertTriangle size={14} /> {error || run?.error}
            </p>
          )}
        </div>

        <div className="flex justify-end gap-2 border-t border-zinc-200 px-5 py-4 dark:border-white/10">
          {preview ? (
            <>
              <button type="button" onClick={onClose} className="rounded-lg px-4 py-2 text-sm font-medium text-zinc-500 hover:text-zinc-800 dark:hover:text-zinc-200">
                {t('processDiscard')}
              </button>
              <button type="button" onClick={() => void keep()} disabled={keeping} className="inline-flex items-center gap-2 rounded-lg bg-gradient-to-r from-orange-500 to-pink-600 px-4 py-2 text-sm font-bold text-white disabled:opacity-50">
                {keeping ? <Loader2 size={14} className="animate-spin" /> : <Check size={14} />} {t('processKeep')}
              </button>
            </>
          ) : (
            <>
              {nothing && <span className="mr-auto self-center text-[11px] text-zinc-500">{t('processNothing')}</span>}
              <button type="button" onClick={onClose} className="rounded-lg px-4 py-2 text-sm font-medium text-zinc-500 hover:text-zinc-800 dark:hover:text-zinc-200">
                {t('cancel')}
              </button>
              <button type="button" onClick={() => void start()} disabled={!ready || running} className="inline-flex items-center gap-2 rounded-lg bg-gradient-to-r from-orange-500 to-pink-600 px-4 py-2 text-sm font-bold text-white disabled:opacity-50">
                {running ? <Loader2 size={14} className="animate-spin" /> : <Wand2 size={14} />} {t('processStart')}
              </button>
            </>
          )}
        </div>
      </div>
    </div>
  );
};

/**
 * Before and after, playing together with one of them heard: switching keeps
 * the position, so the difference is the only thing that changes.
 */
const Compare: React.FC<{ original: string; processed: string }> = ({ original, processed }) => {
  const { t } = useI18n();
  const before = useRef<HTMLAudioElement | null>(null);
  const after = useRef<HTMLAudioElement | null>(null);
  const [side, setSide] = useState<'before' | 'after'>('after');
  const [playing, setPlaying] = useState(false);
  const [time, setTime] = useState(0);
  const [duration, setDuration] = useState(0);

  useEffect(() => {
    if (before.current) before.current.muted = side !== 'before';
    if (after.current) after.current.muted = side !== 'after';
  }, [side]);

  // keep the silent one aligned with the heard one
  useEffect(() => {
    if (!playing) return;
    const timer = window.setInterval(() => {
      const a = before.current;
      const b = after.current;
      if (!a || !b) return;
      const [lead, follow] = side === 'after' ? [b, a] : [a, b];
      setTime(lead.currentTime);
      if (Math.abs(follow.currentTime - lead.currentTime) > 0.05) follow.currentTime = lead.currentTime;
    }, 200);
    return () => window.clearInterval(timer);
  }, [playing, side]);

  const both = () => [before.current, after.current].filter((element): element is HTMLAudioElement => Boolean(element));
  const toggle = async () => {
    if (playing) {
      both().forEach(element => element.pause());
      setPlaying(false);
    } else {
      await Promise.all(both().map(element => element.play().catch(() => undefined)));
      setPlaying(true);
    }
  };
  const seek = (value: number) => {
    both().forEach(element => { element.currentTime = value; });
    setTime(value);
  };
  const clock = (value: number) => `${Math.floor(value / 60)}:${String(Math.floor(value % 60)).padStart(2, '0')}`;

  return (
    <section className="rounded-xl border border-pink-400/40 bg-pink-500/5 p-3">
      <audio ref={before} src={original} preload="auto" muted onEnded={() => setPlaying(false)} />
      <audio ref={after} src={processed} preload="auto" onLoadedMetadata={event => setDuration(event.currentTarget.duration)} onEnded={() => setPlaying(false)} />
      <div className="flex items-center gap-3">
        <button type="button" onClick={() => void toggle()} className="flex h-10 w-10 shrink-0 items-center justify-center rounded-full bg-gradient-to-r from-orange-500 to-pink-600 text-white" aria-label={playing ? 'pause' : 'play'}>
          {playing ? <Pause size={16} /> : <Play size={16} className="ml-0.5" />}
        </button>
        <div role="tablist" className="flex flex-1 rounded-lg bg-zinc-100 p-1 dark:bg-white/5">
          {(['before', 'after'] as const).map(value => (
            <button
              key={value}
              type="button"
              role="tab"
              aria-selected={side === value}
              onClick={() => setSide(value)}
              className={`flex-1 rounded-md py-1.5 text-xs font-semibold transition-all ${side === value ? 'bg-white text-black shadow-sm dark:bg-zinc-800 dark:text-white' : 'text-zinc-500 hover:text-zinc-900 dark:hover:text-zinc-300'}`}
            >
              {value === 'before' ? t('processBefore') : t('processAfter')}
            </button>
          ))}
        </div>
      </div>
      <div className="mt-3 flex items-center gap-2 text-[11px] tabular-nums text-zinc-500">
        <span>{clock(time)}</span>
        <input type="range" min={0} max={duration || 0} step={0.1} value={time} onChange={event => seek(Number(event.target.value))} aria-label="position" className="h-1 flex-1 cursor-pointer accent-pink-500" />
        <span>{clock(duration)}</span>
      </div>
      <p className="mt-2 text-[11px] text-zinc-500">{t('processCompareHint')}</p>
    </section>
  );
};
