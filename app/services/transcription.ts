/**
 * Reads a recording into an ABC score with SheetSage2, through the studio
 * service: the audio is either a file the user picked or a library track.
 */

interface TranscriptionJob {
  id: string;
  status: 'running' | 'done' | 'failed' | 'cancelled';
  abc?: string;
  error?: string;
}

const POLL_MS = 1000;

export async function transcribe(source: { file?: File; songId?: string }, melodyOnly: boolean, signal?: AbortSignal): Promise<string> {
  const form = new FormData();
  if (source.file) form.append('audio', source.file, source.file.name);
  else if (source.songId) form.append('song_id', source.songId);
  else throw new Error('no audio to transcribe');
  if (melodyOnly) form.append('melody_only', '1');

  const submitted = await fetch('/v1/transcriptions', { method: 'POST', body: form, signal });
  const job = (await submitted.json().catch(() => ({}))) as Partial<TranscriptionJob> & { error?: string };
  if (!submitted.ok || !job.id) throw new Error(job.error || `transcription was refused (${submitted.status})`);

  for (;;) {
    await new Promise(resolve => window.setTimeout(resolve, POLL_MS));
    if (signal?.aborted) {
      await fetch(`/v1/transcriptions/${encodeURIComponent(job.id)}`, { method: 'POST' }).catch(() => undefined);
      throw new DOMException('cancelled', 'AbortError');
    }
    const response = await fetch(`/v1/transcriptions/${encodeURIComponent(job.id)}`, { signal });
    const state = (await response.json().catch(() => ({}))) as Partial<TranscriptionJob> & { error?: string };
    if (!response.ok) throw new Error(state.error || `transcription status failed (${response.status})`);
    if (state.status === 'done' && state.abc) return state.abc;
    if (state.status === 'failed') throw new Error(state.error || 'the engine could not transcribe this recording');
    if (state.status === 'cancelled') throw new DOMException('cancelled', 'AbortError');
  }
}
