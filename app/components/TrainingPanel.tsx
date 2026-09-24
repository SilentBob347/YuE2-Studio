import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { AlertTriangle, Check, CheckSquare, ChevronDown, Download, FolderOpen, Library, Loader2, Mic2, Plus, Search, Square, Trash2, X } from 'lucide-react';
import { useI18n } from '../context/I18nContext';
import { ConfirmDialog } from './ConfirmDialog';
import {
  Dataset,
  DatasetItem,
  FieldCondition,
  Recipe,
  RecipeField,
  TrainingRun,
  TrainingState,
  addFiles,
  autofillItem,
  addLibrarySongs,
  cancelRun,
  cancelTrainingPack,
  clock,
  createDataset,
  deleteDataset,
  deleteItem,
  deleteRun,
  fetchTraining,
  gigabytes,
  installCheckpoint,
  installTrainingPack,
  startRun,
  updateDataset,
  updateItem,
} from '../services/training';

/**
 * Training a LoRA on the user's own songs: the optional pack, a dataset of
 * songs with their style and lyrics, a recipe, and runs whose checkpoints go
 * straight to the LoRA library.
 */

const CARD = 'rounded-2xl border border-zinc-200 bg-zinc-50 p-4 dark:border-white/10 dark:bg-white/[0.03]';
const CONTROL =
  'w-full rounded-lg border border-zinc-200 bg-white px-3 py-2 text-sm text-zinc-900 outline-none focus:border-pink-500 disabled:opacity-50 dark:border-white/10 dark:bg-black/30 dark:text-white';
const OUTLINE =
  'inline-flex items-center gap-1.5 rounded-lg border border-zinc-200 px-3 py-1.5 text-xs font-semibold text-zinc-700 hover:border-pink-400 hover:text-pink-600 disabled:cursor-not-allowed disabled:opacity-50 dark:border-white/10 dark:text-zinc-200';
const PRIMARY =
  'inline-flex items-center justify-center gap-2 rounded-lg bg-gradient-to-r from-orange-500 to-pink-600 px-4 py-2 text-sm font-bold text-white disabled:cursor-not-allowed disabled:opacity-50';
const LABEL = 'text-[11px] font-bold uppercase tracking-wide text-zinc-500';

const HINT = 'mt-1 text-[11px] leading-4 text-zinc-500';

const NumberField: React.FC<{ label: string; value: number; onChange: (value: number) => void; min?: number; max?: number; step?: number; disabled?: boolean; hint?: string }> = ({ label, value, onChange, min, max, step, disabled, hint }) => (
  <label className="block">
    <span className={LABEL}>{label}</span>
    <input
      type="number"
      value={value}
      min={min}
      max={max}
      step={step}
      disabled={disabled}
      onChange={event => {
        const next = Number(event.target.value);
        if (Number.isFinite(next)) onChange(next);
      }}
      className={`${CONTROL} mt-1`}
    />
    {hint && <span className={`block ${HINT}`}>{hint}</span>}
  </label>
);

const Choice = <T extends string>({ label, value, options, onChange }: { label: string; value: T; options: { value: T; label: string }[]; onChange: (value: T) => void }) => (
  <label className="block">
    <span className={LABEL}>{label}</span>
    <select value={value} onChange={event => onChange(event.target.value as T)} className={`${CONTROL} mt-1`}>
      {options.map(option => <option key={option.value} value={option.value}>{option.label}</option>)}
    </select>
  </label>
);

/** Every setting of a run, as the engine lists them, starting from its recipe. */
const RecipeForm: React.FC<{ recipe: Recipe; defaults: Recipe; fields: RecipeField[]; onChange: (recipe: Recipe) => void }> = ({ recipe, defaults, fields, onChange }) => {
  const { tt } = useStrings();
  const holds = (condition?: FieldCondition) => condition !== undefined && condition.values.includes(String(recipe[condition.field]));
  const groups: string[] = [];
  for (const field of fields) if (!groups.includes(field.group)) groups.push(field.group);
  const changed = JSON.stringify(recipe) !== JSON.stringify(defaults);
  const control = (field: RecipeField) => {
    const label = tt(`trainingField_${field.key}`);
    const off = holds(field.off_when);
    const hint = off ? tt(`trainingHint_${field.key}_off`) : undefined;
    const value = recipe[field.key];
    if (field.kind === 'toggle') {
      return (
        <label key={field.key} className="flex items-center gap-2 pb-2 text-sm text-zinc-700 dark:text-zinc-200">
          <input type="checkbox" checked={value === true} onChange={event => onChange({ ...recipe, [field.key]: event.target.checked })} className="accent-pink-500" />
          {label}
        </label>
      );
    }
    if (field.kind === 'choice') {
      return (
        <React.Fragment key={field.key}>
          <Choice
            label={label}
            value={String(value)}
            options={(field.choices ?? []).map(choice => ({ value: choice, label: tt(`trainingChoice_${choice}`) }))}
            onChange={next => onChange({ ...recipe, [field.key]: next })}
          />
        </React.Fragment>
      );
    }
    return (
      <NumberField
        key={field.key}
        label={label}
        value={Number(value)}
        min={field.min}
        max={field.max}
        step={field.step}
        disabled={off}
        hint={hint}
        onChange={next => {
          const bounded = Math.min(field.max ?? next, Math.max(field.min ?? next, next));
          onChange({ ...recipe, [field.key]: field.kind === 'integer' ? Math.round(bounded) : bounded });
        }}
      />
    );
  };
  return (
    <div className="mt-3 space-y-4">
      {groups.map(group => {
        const shown = fields.filter(field => field.group === group && (field.shown_when === undefined || holds(field.shown_when)));
        const hints = shown.map(field => ({ key: field.key, text: tt(`trainingHint_${field.key}`) })).filter(entry => entry.text !== `trainingHint_${entry.key}`);
        return (
          <div key={group}>
            <p className={LABEL}>{tt(`trainingGroup_${group}`)}</p>
            <div className="mt-1.5 grid items-end gap-2 sm:grid-cols-4">{shown.map(control)}</div>
            {hints.map(entry => <p key={entry.key} className={HINT}>{entry.text}</p>)}
          </div>
        );
      })}
      {changed && (
        <button type="button" onClick={() => onChange(defaults)} className={OUTLINE}>{tt('trainingResetRecipe')}</button>
      )}
    </div>
  );
};

function useStrings() {
  const { t } = useI18n();
  return { t, tt: t as unknown as (key: string) => string };
}

const errorText = (problem: unknown) => (problem instanceof Error ? problem.message : String(problem));

/** The pack: what it holds, the requirements, and its download. */
const PackCard: React.FC<{ state: TrainingState; onError: (message: string) => void; onChanged: () => void }> = ({ state, onError, onChanged }) => {
  const { t } = useStrings();
  const total = state.pack.reduce((sum, file) => sum + file.bytes, 0);
  const missing = state.pack.filter(file => !file.installed).reduce((sum, file) => sum + file.bytes, 0);
  const download = state.download && !state.download.done ? state.download : null;
  const percent = download ? Math.min(100, (100 * download.downloaded_bytes) / Math.max(1, download.total_bytes)) : 0;
  return (
    <section className={CARD}>
      <p className="flex items-center gap-2 text-sm font-semibold text-zinc-900 dark:text-white"><Download size={16} className="text-pink-500" />{t('trainingSetupTitle')}</p>
      <p className="mt-2 text-sm text-zinc-600 dark:text-zinc-300">{t('trainingSetupNeeds').replace('{size}', gigabytes(total)).replace('{vram}', String(state.min_vram_gb))}</p>
      <ul className="mt-3 space-y-1.5">
        {state.pack.map(file => (
          <li key={file.id} className="flex items-center justify-between gap-3 text-xs text-zinc-700 dark:text-zinc-200">
            <span className="flex items-center gap-2">{file.installed ? <Check size={13} className="text-emerald-500" /> : <Square size={13} className="text-zinc-400" />}{file.label}</span>
            <span className="tabular-nums text-zinc-500">{gigabytes(file.bytes)}</span>
          </li>
        ))}
      </ul>
      {download ? (
        <div className="mt-3">
          <div className="flex items-center justify-between text-xs text-zinc-500">
            <span className="inline-flex items-center gap-1.5"><Loader2 size={13} className="animate-spin text-pink-500" />{percent.toFixed(1)}%</span>
            <span className="tabular-nums">{gigabytes(download.downloaded_bytes)} / {gigabytes(download.total_bytes)}</span>
          </div>
          <div className="mt-2 h-1.5 overflow-hidden rounded-full bg-zinc-200 dark:bg-white/10">
            <div className="h-full bg-gradient-to-r from-orange-500 to-pink-600 transition-[width]" style={{ width: `${percent}%` }} />
          </div>
          <button type="button" onClick={() => void cancelTrainingPack().then(onChanged)} className={`${OUTLINE} mt-3`}><X size={13} />{t('adaptersCancel')}</button>
        </div>
      ) : (
        !state.pack_ready && (
          <button type="button" onClick={() => void installTrainingPack().then(onChanged).catch(problem => onError(errorText(problem)))} className={`${PRIMARY} mt-3`}>
            <Download size={15} />{t('trainingDownload')} · {gigabytes(missing)}
          </button>
        )
      )}
      {state.download?.done && state.download.error && state.download.error !== 'cancelled' && (
        <p role="alert" className="mt-3 text-xs text-rose-600 dark:text-rose-300">{state.download.error}</p>
      )}
    </section>
  );
};

/** Library songs with audio, searchable, several picked at once. */
const LibraryPicker: React.FC<{ exclude: string[]; onAdd: (ids: string[]) => void; onClose: () => void }> = ({ exclude, onAdd, onClose }) => {
  const { t } = useStrings();
  const [songs, setSongs] = useState<{ id: string; title: string; caption: string }[]>([]);
  const [query, setQuery] = useState('');
  const [picked, setPicked] = useState<string[]>([]);
  useEffect(() => {
    void fetch('/v1/library/songs')
      .then(response => response.json())
      .then((body: { id: string; title: string; caption: string; audio_path?: string | null }[] | { songs: never[] }) => {
        const list = Array.isArray(body) ? body : body.songs ?? [];
        setSongs(list.filter(song => song.audio_path && !exclude.includes(song.id)));
      })
      .catch(() => undefined);
  }, [exclude]);
  const needle = query.trim().toLowerCase();
  const visible = needle ? songs.filter(song => song.title.toLowerCase().includes(needle) || song.caption.toLowerCase().includes(needle)) : songs;
  return (
    <div className="mt-3 rounded-xl border border-zinc-200 p-3 dark:border-white/10">
      <div className="relative">
        <Search size={14} className="pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-zinc-400" />
        <input value={query} onChange={event => setQuery(event.target.value)} placeholder={t('trainingSearchLibrary')} aria-label={t('trainingSearchLibrary')} className={`${CONTROL} pl-9`} />
      </div>
      <div className="mt-2 max-h-56 overflow-y-auto rounded-lg border border-zinc-200 dark:border-white/10">
        {visible.map(song => {
          const on = picked.includes(song.id);
          return (
            <button
              key={song.id}
              type="button"
              role="checkbox"
              aria-checked={on}
              onClick={() => setPicked(current => (on ? current.filter(id => id !== song.id) : [...current, song.id]))}
              className={`flex w-full items-center gap-2 border-b border-zinc-100 px-3 py-1.5 text-left text-sm last:border-b-0 dark:border-white/5 ${on ? 'bg-pink-500/10' : 'hover:bg-zinc-100 dark:hover:bg-white/5'}`}
            >
              {on ? <CheckSquare size={14} className="shrink-0 text-pink-500" /> : <Square size={14} className="shrink-0 text-zinc-400" />}
              <span className="min-w-0 flex-1 truncate text-zinc-800 dark:text-zinc-200">{song.title}</span>
            </button>
          );
        })}
      </div>
      <div className="mt-2 flex justify-end gap-2">
        <button type="button" onClick={onClose} className={OUTLINE}>{t('adaptersCancel')}</button>
        <button type="button" onClick={() => onAdd(picked)} disabled={picked.length === 0} className={OUTLINE}><Plus size={13} />{t('trainingAddSelected')}{picked.length ? ` · ${picked.length}` : ''}</button>
      </div>
    </div>
  );
};

/** One song of a dataset: title and length, opening into its style and lyrics. */
const ItemRow: React.FC<{ datasetId: string; item: DatasetItem; recognising: boolean; onRecognise: () => void; onChanged: (dataset: Dataset) => void; onError: (message: string) => void }> = ({ datasetId, item, recognising, onRecognise, onChanged, onError }) => {
  const { t } = useStrings();
  const [open, setOpen] = useState(false);
  const [style, setStyle] = useState(item.style);
  const [lyrics, setLyrics] = useState(item.lyrics);
  useEffect(() => { setStyle(item.style); setLyrics(item.lyrics); }, [item.style, item.lyrics]);
  const save = (patch: Parameters<typeof updateItem>[2]) => void updateItem(datasetId, item.id, patch).then(onChanged).catch(problem => onError(errorText(problem)));
  const empty = !item.style.trim() || (!item.instrumental && !item.lyrics.trim());
  return (
    <div className="border-b border-zinc-200 py-2 last:border-b-0 dark:border-white/10">
      <div className="flex items-center gap-2">
        <button type="button" onClick={() => setOpen(value => !value)} className="flex min-w-0 flex-1 items-center gap-2 text-left" aria-expanded={open}>
          <ChevronDown size={14} className={`shrink-0 text-zinc-400 transition-transform ${open ? 'rotate-180' : ''}`} />
          <span className="min-w-0 truncate text-sm text-zinc-800 dark:text-zinc-200">{item.title}</span>
          {empty && <AlertTriangle size={12} className="shrink-0 text-amber-500" />}
        </button>
        <button type="button" onClick={onRecognise} disabled={recognising} className="shrink-0 text-zinc-400 hover:text-pink-500 disabled:opacity-60" title={t('trainingAutofill')}>
          {recognising ? <Loader2 size={13} className="animate-spin text-pink-500" /> : <Mic2 size={13} />}
        </button>
        <span className="shrink-0 text-[11px] tabular-nums text-zinc-500">{clock(item.seconds)}</span>
        <button type="button" onClick={() => void deleteItem(datasetId, item.id).then(onChanged).catch(problem => onError(errorText(problem)))} className="shrink-0 text-zinc-400 hover:text-rose-500" title={t('trainingDelete')}><Trash2 size={13} /></button>
      </div>
      {open && (
        <div className="mt-2 space-y-2 pl-6">
          <label className="block">
            <span className={LABEL}>{t('trainingStyle')}</span>
            <input value={style} onChange={event => setStyle(event.target.value)} onBlur={() => style !== item.style && save({ style })} className={`${CONTROL} mt-1`} />
          </label>
          <label className="flex items-center gap-2 text-xs text-zinc-700 dark:text-zinc-200">
            <input type="checkbox" checked={item.instrumental} onChange={event => save({ instrumental: event.target.checked })} className="accent-pink-500" />
            {t('trainingInstrumental')}
          </label>
          {!item.instrumental && (
            <label className="block">
              <span className={LABEL}>{t('trainingLyrics')}</span>
              <textarea value={lyrics} onChange={event => setLyrics(event.target.value)} onBlur={() => lyrics !== item.lyrics && save({ lyrics })} rows={8} className={`${CONTROL} mt-1 font-mono text-xs`} />
            </label>
          )}
        </div>
      )}
    </div>
  );
};

/** The loss over the steps so far, as a line. */
const LossLine: React.FC<{ steps: TrainingRun['steps'] }> = ({ steps }) => {
  if (steps.length < 2) return null;
  const losses = steps.map(step => step.loss);
  const low = Math.min(...losses);
  const high = Math.max(...losses);
  const span = Math.max(1e-9, high - low);
  const last = steps[steps.length - 1].step;
  const points = steps.map(step => `${(100 * step.step) / last},${36 - (34 * (step.loss - low)) / span}`).join(' ');
  return (
    <svg viewBox="0 0 100 38" preserveAspectRatio="none" className="mt-2 h-16 w-full" aria-hidden>
      <polyline points={points} fill="none" stroke="currentColor" strokeWidth="1.2" vectorEffect="non-scaling-stroke" className="text-pink-500" />
    </svg>
  );
};

const RunCard: React.FC<{ run: TrainingRun; onChanged: () => void; onError: (message: string) => void; onDelete: () => void }> = ({ run, onChanged, onError, onDelete }) => {
  const { t, tt } = useStrings();
  const running = run.status === 'running';
  const last = run.steps[run.steps.length - 1];
  const cap = Number(run.recipe.steps) || 1;
  // an engine that stops on drift reports it per step; the stop is on the mean of the last 20
  const target = Number(run.recipe.target_kl ?? 0);
  const window = run.steps.slice(-20).map(step => step.ar_kl).filter((kl): kl is number => typeof kl === 'number');
  const kl = window.length ? window.reduce((a, b) => a + b, 0) / window.length : null;
  const recent = run.steps.slice(-10).map(step => step.step_ms).filter((ms): ms is number => typeof ms === 'number');
  const perStep = recent.length ? recent.reduce((a, b) => a + b, 0) / recent.length / 1000 : 0;
  const left = last && perStep && target <= 0 ? (cap - last.step) * perStep : 0;
  const percent = last ? Math.min(100, 100 * Math.max(last.step / cap, target > 0 && kl !== null ? kl / target : 0)) : 0;
  const stageIndex = run.stage ? run.stages.indexOf(run.stage) : run.status === 'done' ? run.stages.length : -1;
  const tone = { running: 'text-pink-600 dark:text-pink-300', done: 'text-emerald-600 dark:text-emerald-400', failed: 'text-rose-600 dark:text-rose-300', cancelled: 'text-zinc-500', interrupted: 'text-amber-600 dark:text-amber-300' }[run.status];
  return (
    <section className={CARD}>
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <p className="truncate text-sm font-semibold text-zinc-900 dark:text-white">{run.name}</p>
          <p className="mt-0.5 text-[11px] text-zinc-500">{run.dataset_name}{run.trigger ? ` · ${run.trigger}` : ''} · {target > 0 ? `KL ${target} · ` : ''}{target > 0 ? '≤ ' : ''}{cap} {t('trainingSteps')}</p>
        </div>
        <div className="flex shrink-0 items-center gap-2">
          <span className={`inline-flex items-center gap-1 text-xs font-semibold ${tone}`}>{running && <Loader2 size={12} className="animate-spin" />}{tt(`trainingStatus_${run.status}`)}</span>
          {running ? (
            <button type="button" onClick={() => void cancelRun(run.id).then(onChanged).catch(problem => onError(errorText(problem)))} className={OUTLINE}><X size={13} />{t('trainingStop')}</button>
          ) : (
            <button type="button" onClick={onDelete} className={`${OUTLINE} hover:border-rose-400 hover:text-rose-600`} title={t('trainingDelete')}><Trash2 size={13} /></button>
          )}
        </div>
      </div>

      <div className="mt-3 flex flex-wrap gap-1">
        {run.stages.map((stage, index) => (
          <span
            key={stage}
            className={`rounded-md px-1.5 py-0.5 text-[10px] font-semibold ${
              index < stageIndex ? 'bg-emerald-500/10 text-emerald-600 dark:text-emerald-400' : index === stageIndex ? 'bg-pink-500/15 text-pink-600 dark:text-pink-300' : 'bg-zinc-200/60 text-zinc-500 dark:bg-white/5'
            }`}
          >
            {tt(`trainingStage_${stage}`)}
          </span>
        ))}
      </div>

      {last && (
        <>
          <div className="mt-3 flex items-baseline justify-between text-[11px] tabular-nums text-zinc-600 dark:text-zinc-300">
            <span>{t('trainingStep')} {last.step} · {t('trainingLoss')} {last.loss.toFixed(3)}{kl !== null ? ` · KL ${kl.toFixed(2)}${target > 0 ? ` / ${target}` : ''}` : ''}</span>
            {running && left > 0 && <span>{clock(left)} {t('trainingLeft')}</span>}
          </div>
          <div className="mt-1.5 h-1.5 overflow-hidden rounded-full bg-zinc-200 dark:bg-white/10">
            <div className="h-full bg-gradient-to-r from-orange-500 to-pink-600 transition-[width]" style={{ width: `${percent}%` }} />
          </div>
          <LossLine steps={run.steps} />
        </>
      )}

      {run.checkpoints.length > 0 && (
        <div className="mt-3">
          <p className={LABEL}>{t('trainingCheckpoints')}</p>
          <div className="mt-1.5 flex flex-wrap gap-2">
            {[...run.checkpoints].sort((a, b) => a - b).map(step => {
              const installed = run.installed.includes(step);
              return (
                <button
                  key={step}
                  type="button"
                  disabled={installed}
                  onClick={() =>
                    void installCheckpoint(run.id, step)
                      .then(() => {
                        window.dispatchEvent(new CustomEvent('yue:adapters-changed'));
                        window.dispatchEvent(new CustomEvent('yue:toast', { detail: { message: t('trainingAdded'), type: 'success' } }));
                        onChanged();
                      })
                      .catch(problem => onError(errorText(problem)))
                  }
                  className={`${OUTLINE} ${installed ? 'text-emerald-600 dark:text-emerald-400' : ''}`}
                >
                  {installed ? <Check size={13} /> : <Plus size={13} />}
                  {t('trainingStep')} {step} · {installed ? t('trainingInLora') : t('trainingToLora')}
                </button>
              );
            })}
          </div>
        </div>
      )}

      {run.error && <p role="alert" className="mt-3 text-xs text-rose-600 dark:text-rose-300">{run.error}</p>}
      {run.log && run.log.length > 0 && (
        <details className="mt-2">
          <summary className="cursor-pointer text-[11px] text-zinc-500">{t('trainingLog')}</summary>
          <pre className="mt-1 max-h-40 overflow-auto whitespace-pre-wrap rounded-lg bg-zinc-100 p-2 text-[10px] leading-4 text-zinc-600 dark:bg-black/30 dark:text-zinc-400">{run.log.join('\n')}</pre>
        </details>
      )}
    </section>
  );
};

export const TrainingPanel: React.FC = () => {
  const { t, tt } = useStrings();
  const [state, setState] = useState<TrainingState | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [newName, setNewName] = useState('');
  const [newTrigger, setNewTrigger] = useState('');
  const [picking, setPicking] = useState(false);
  const [adding, setAdding] = useState(false);
  const [advanced, setAdvanced] = useState(false);
  const [recipe, setRecipe] = useState<Recipe | null>(null);
  const [starting, setStarting] = useState(false);
  const [deleting, setDeleting] = useState<{ kind: 'dataset' | 'run'; id: string } | null>(null);
  const [recognising, setRecognising] = useState<string[]>([]);
  const filePicker = useRef<HTMLInputElement | null>(null);

  const refresh = useCallback(async () => {
    try {
      setState(await fetchTraining());
    } catch (problem) {
      setError(errorText(problem));
    }
  }, []);

  const busy = Boolean(state?.active || (state?.download && !state.download.done));
  useEffect(() => {
    void refresh();
    const timer = window.setInterval(() => void refresh(), busy ? 1500 : 5000);
    return () => window.clearInterval(timer);
  }, [refresh, busy]);

  const datasets = state?.datasets ?? [];
  const dataset = datasets.find(entry => entry.id === selected) ?? datasets[0] ?? null;
  const replace = (next: Dataset) => setState(current => (current ? { ...current, datasets: current.datasets.map(entry => (entry.id === next.id ? next : entry)) } : current));
  const total = dataset?.items.reduce((sum, item) => sum + item.seconds, 0) ?? 0;
  const exclude = useMemo(() => (dataset?.items ?? []).map(item => item.source.replace(/^song:/, '')), [dataset?.items]);

  const create = async () => {
    try {
      const made = await createDataset(newName, newTrigger);
      setCreating(false);
      setNewName('');
      setNewTrigger('');
      setSelected(made.id);
      await refresh();
    } catch (problem) {
      setError(errorText(problem));
    }
  };

  const addSongs = async (ids: string[]) => {
    if (!dataset) return;
    setPicking(false);
    setAdding(true);
    try {
      replace(await addLibrarySongs(dataset.id, ids));
    } catch (problem) {
      setError(errorText(problem));
    } finally {
      setAdding(false);
    }
  };

  const addDiskFiles = async (files: File[]) => {
    if (!dataset || files.length === 0) return;
    setAdding(true);
    try {
      replace(await addFiles(dataset.id, files));
    } catch (problem) {
      setError(errorText(problem));
    } finally {
      setAdding(false);
      if (filePicker.current) filePicker.current.value = '';
    }
  };

  // one at a time: each wants the card for its vocals and its recogniser
  const recognise = async (ids: string[]) => {
    if (!dataset) return;
    setRecognising(ids);
    setError(null);
    for (const id of ids) {
      try {
        replace(await autofillItem(dataset.id, id));
      } catch (problem) {
        setError(errorText(problem));
        break;
      } finally {
        setRecognising(current => current.filter(value => value !== id));
      }
    }
    setRecognising([]);
  };

  const start = async () => {
    if (!dataset) return;
    setStarting(true);
    setError(null);
    try {
      await startRun(dataset.id, dataset.name, recipe ?? state!.recipe_defaults);
      await refresh();
    } catch (problem) {
      setError(errorText(problem));
    } finally {
      setStarting(false);
    }
  };

  const confirmDelete = async () => {
    const target = deleting;
    setDeleting(null);
    if (!target) return;
    try {
      if (target.kind === 'dataset') await deleteDataset(target.id);
      else await deleteRun(target.id);
      await refresh();
    } catch (problem) {
      setError(errorText(problem));
    }
  };

  if (!state) return <p className="flex items-center gap-2 text-sm text-zinc-500"><Loader2 size={14} className="animate-spin" /></p>;
  const ready = state.pack_ready;
  const blocker = !ready ? t('trainingNeedPack') : !dataset?.items.length ? t('trainingNeedSongs') : null;

  return (
    <div className="space-y-4">
      <p className="text-sm text-zinc-600 dark:text-zinc-400">{t('trainingIntro')}</p>
      {!ready && <PackCard state={state} onError={setError} onChanged={() => void refresh()} />}

      <section className={CARD}>
        <div className="flex flex-wrap items-center gap-2">
          <span className={LABEL}>{t('trainingDatasets')}</span>
          {datasets.map(entry => (
            <button
              key={entry.id}
              type="button"
              onClick={() => setSelected(entry.id)}
              className={`rounded-full px-3 py-1 text-xs font-semibold transition-colors ${entry.id === dataset?.id ? 'bg-pink-500/15 text-pink-600 dark:text-pink-300' : 'bg-zinc-200/60 text-zinc-600 hover:text-zinc-900 dark:bg-white/5 dark:text-zinc-300'}`}
            >
              {entry.name} · {entry.items.length}
            </button>
          ))}
          <button type="button" onClick={() => setCreating(value => !value)} className={OUTLINE}><Plus size={13} />{t('trainingNewDataset')}</button>
        </div>

        {creating && (
          <div className="mt-3 grid gap-2 sm:grid-cols-[1fr_1fr_auto]">
            <input value={newName} onChange={event => setNewName(event.target.value)} placeholder={t('trainingDatasetName')} aria-label={t('trainingDatasetName')} className={CONTROL} />
            <input value={newTrigger} onChange={event => setNewTrigger(event.target.value)} placeholder={t('trainingTrigger')} aria-label={t('trainingTrigger')} className={CONTROL} />
            <button type="button" onClick={() => void create()} disabled={!newName.trim()} className={PRIMARY}>{t('trainingCreate')}</button>
          </div>
        )}

        {!dataset && !creating && <p className="mt-3 text-sm text-zinc-500">{t('trainingEmptyDatasets')}</p>}

        {dataset && (
          <div className="mt-4 space-y-3">
            <div className="grid gap-2 sm:grid-cols-[1fr_1fr_auto]">
              <label>
                <span className={LABEL}>{t('trainingDatasetName')}</span>
                <input key={`${dataset.id}-name`} defaultValue={dataset.name} onBlur={event => event.target.value !== dataset.name && void updateDataset(dataset.id, { name: event.target.value }).then(replace).catch(problem => setError(errorText(problem)))} className={`${CONTROL} mt-1`} />
              </label>
              <label>
                <span className={LABEL}>{t('trainingTrigger')}</span>
                <input key={`${dataset.id}-trigger`} defaultValue={dataset.trigger} onBlur={event => event.target.value !== dataset.trigger && void updateDataset(dataset.id, { trigger: event.target.value }).then(replace).catch(problem => setError(errorText(problem)))} className={`${CONTROL} mt-1`} />
              </label>
              <button type="button" onClick={() => setDeleting({ kind: 'dataset', id: dataset.id })} className={`${OUTLINE} self-end hover:border-rose-400 hover:text-rose-600`} title={t('trainingDelete')}><Trash2 size={13} /></button>
            </div>
            <p className="text-[11px] leading-4 text-zinc-500">{t('trainingTriggerHint')}</p>

            <div className="flex flex-wrap items-center justify-between gap-2">
              <span className={LABEL}>{t('trainingSongs')} · {dataset.items.length} · {clock(total)}</span>
              <div className="flex gap-2">
                <button type="button" onClick={() => setPicking(value => !value)} disabled={adding} className={OUTLINE}><Library size={13} />{t('trainingAddLibrary')}</button>
                <button type="button" onClick={() => filePicker.current?.click()} disabled={adding} className={OUTLINE}>{adding ? <Loader2 size={13} className="animate-spin" /> : <FolderOpen size={13} />}{adding ? t('trainingAdding') : t('trainingAddFiles')}</button>
                <input ref={filePicker} type="file" multiple accept="audio/*,.wav,.mp3,.flac,.ogg,.m4a,.txt,.lrc" className="hidden" onChange={event => void addDiskFiles(Array.from(event.target.files ?? []))} />
              </div>
            </div>
            <p className="text-[11px] leading-4 text-zinc-500">{t('trainingSongsHint')} {t('trainingAddFilesHint')}</p>
            {dataset.items.length > 0 && (
              <div className="flex flex-wrap items-center gap-2">
                <button type="button" onClick={() => void recognise(dataset.items.filter(item => !item.instrumental).map(item => item.id))} disabled={recognising.length > 0 || Boolean(state.active)} className={OUTLINE}>
                  {recognising.length > 0 ? <Loader2 size={13} className="animate-spin" /> : <Mic2 size={13} />}
                  {recognising.length > 0 ? `${t('trainingAutofilling')} ${dataset.items.length - recognising.length + 1}/${dataset.items.length}` : t('trainingAutofillAll')}
                </button>
                <span className="text-[11px] leading-4 text-zinc-500">{t('trainingAutofillHint')}</span>
              </div>
            )}
            {picking && <LibraryPicker exclude={exclude} onAdd={ids => void addSongs(ids)} onClose={() => setPicking(false)} />}
            {dataset.items.length === 0 ? (
              <p className="text-sm text-zinc-500">{t('trainingNoSongs')}</p>
            ) : (
              <div>{dataset.items.map(item => <ItemRow key={item.id} datasetId={dataset.id} item={item} recognising={recognising.includes(item.id)} onRecognise={() => void recognise([item.id])} onChanged={replace} onError={setError} />)}</div>
            )}
          </div>
        )}
      </section>

      {dataset && (
        <section className={CARD}>
          <p className={LABEL}>{t('trainingTrain')}</p>
          <button type="button" onClick={() => setAdvanced(value => !value)} className="mt-3 inline-flex items-center gap-1 text-xs font-semibold text-zinc-500 hover:text-zinc-800 dark:hover:text-zinc-200" aria-expanded={advanced}>
            <ChevronDown size={13} className={`transition-transform ${advanced ? 'rotate-180' : ''}`} />{t('trainingAdvanced')}
          </button>
          {advanced && <RecipeForm recipe={recipe ?? state.recipe_defaults} defaults={state.recipe_defaults} fields={state.recipe_fields} onChange={setRecipe} />}
          <div className="mt-4 flex flex-wrap items-center gap-3">
            <button type="button" onClick={() => void start()} disabled={Boolean(blocker) || starting || Boolean(state.active)} className={PRIMARY}>
              {starting ? <Loader2 size={15} className="animate-spin" /> : null}{t('trainingStart')}
            </button>
            {blocker && <span className="text-xs text-zinc-500">{blocker}</span>}
            {state.active && <span className="text-xs text-zinc-500">{t('trainingBusy')}</span>}
          </div>
        </section>
      )}

      <div className="space-y-3">
        <p className={LABEL}>{t('trainingRuns')}</p>
        {state.runs.length === 0 && <p className="text-sm text-zinc-500">{t('trainingNoRuns')}</p>}
        {state.runs.map(run => (
          <RunCard key={run.id} run={run} onChanged={() => void refresh()} onError={setError} onDelete={() => setDeleting({ kind: 'run', id: run.id })} />
        ))}
      </div>

      {error && <p role="alert" className="rounded-lg bg-rose-500/10 px-3 py-2 text-sm text-rose-700 dark:text-rose-300">{error}</p>}

      <ConfirmDialog
        isOpen={deleting !== null}
        title={t('trainingDelete')}
        message={deleting?.kind === 'dataset' ? t('trainingDeleteDatasetMessage') : t('trainingDeleteRunMessage')}
        confirmLabel={t('trainingDelete')}
        onConfirm={() => void confirmDelete()}
        onCancel={() => setDeleting(null)}
      />
    </div>
  );
};
