/**
 * Adapter training: the optional pack, engine-neutral datasets and the runs
 * that turn them into LoRA.
 */

export interface PackFile {
  id: string;
  label: string;
  bytes: number;
  installed: boolean;
}

export interface DatasetItem {
  id: string;
  title: string;
  style: string;
  lyrics: string;
  instrumental: boolean;
  file: string;
  seconds: number;
  source: string;
}

export interface Dataset {
  id: string;
  name: string;
  trigger: string;
  created_at: string;
  items: DatasetItem[];
}

export interface Recipe {
  steps: number;
  save_every: number;
  seed?: number;
  rank?: number | null;
  learning_rate?: number | null;
}

export type RunStatus = 'running' | 'done' | 'failed' | 'cancelled' | 'interrupted';

export interface TrainingRun {
  id: string;
  dataset_id: string;
  dataset_name: string;
  name: string;
  trigger: string;
  recipe: Recipe;
  status: RunStatus;
  stage: string | null;
  stages: string[];
  steps: { step: number; loss: number; step_ms?: number | null }[];
  error?: string | null;
  created_at: string;
  finished_at?: string | null;
  installed: number[];
  checkpoints: number[];
  log?: string[];
}

export interface TrainingState {
  pack: PackFile[];
  pack_ready: boolean;
  download: { downloaded_bytes: number; total_bytes: number; done: boolean; error?: string | null } | null;
  trainer_installed: boolean;
  datasets: Dataset[];
  runs: TrainingRun[];
  active: string | null;
}

async function call<T>(url: string, init?: RequestInit): Promise<T> {
  const response = await fetch(url, init);
  if (response.status === 204) return undefined as T;
  const body = await response.json().catch(() => null);
  if (!response.ok) throw new Error(body?.error || `Training: HTTP ${response.status}`);
  return body as T;
}

const json = (method: string, body: unknown): RequestInit => ({
  method,
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify(body),
});

export const fetchTraining = () => call<TrainingState>('/v1/training');
export const installTrainingPack = () => call<void>('/v1/training/pack/install', { method: 'POST' });
export const cancelTrainingPack = () => call<void>('/v1/training/pack/cancel', { method: 'POST' });

export const createDataset = (name: string, trigger: string) => call<Dataset>('/v1/training/datasets', json('POST', { name, trigger }));
export const updateDataset = (id: string, patch: { name?: string; trigger?: string }) => call<Dataset>(`/v1/training/datasets/${id}`, json('PATCH', patch));
export const deleteDataset = (id: string) => call<void>(`/v1/training/datasets/${id}`, { method: 'DELETE' });
export const addLibrarySongs = (id: string, songIds: string[]) => call<Dataset>(`/v1/training/datasets/${id}/songs`, json('POST', { song_ids: songIds }));

/** Audio files, with any same-named .txt or .lrc taken as their lyrics. */
export function addFiles(id: string, files: File[]): Promise<Dataset> {
  const form = new FormData();
  for (const file of files) form.append('files', file, file.name);
  return call<Dataset>(`/v1/training/datasets/${id}/files`, { method: 'POST', body: form });
}

export const updateItem = (id: string, item: string, patch: Partial<Pick<DatasetItem, 'title' | 'style' | 'lyrics' | 'instrumental'>>) =>
  call<Dataset>(`/v1/training/datasets/${id}/items/${item}`, json('PATCH', patch));
export const deleteItem = (id: string, item: string) => call<Dataset>(`/v1/training/datasets/${id}/items/${item}`, { method: 'DELETE' });

export const startRun = (datasetId: string, name: string, recipe: Recipe) => call<TrainingRun>('/v1/training/runs', json('POST', { dataset_id: datasetId, name, recipe }));
export const cancelRun = (id: string) => call<void>(`/v1/training/runs/${id}/cancel`, { method: 'POST' });
export const deleteRun = (id: string) => call<void>(`/v1/training/runs/${id}`, { method: 'DELETE' });
export const installCheckpoint = (id: string, step: number, name?: string) =>
  call<{ id: string }>(`/v1/training/runs/${id}/checkpoints/${step}/install`, json('POST', { name }));

export const gigabytes = (bytes: number) => `${(bytes / 1024 ** 3).toFixed(1)} GB`;
export const clock = (seconds: number) => `${Math.floor(seconds / 60)}:${String(Math.floor(seconds % 60)).padStart(2, '0')}`;

/** Recognises a song's lyrics from its recording and lays them out in sections. */
export const autofillItem = (id: string, item: string, language?: string) =>
  call<Dataset>(`/v1/training/datasets/${id}/items/${item}/autofill`, json('POST', { language }));
