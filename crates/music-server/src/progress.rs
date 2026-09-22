//! Generation progress read from the engine log.
//!
//! yue-server reports a job as `running` and nothing finer; its log counts
//! every stage: the score tokens, the semantic frames against their budget,
//! the flow-matching steps and the VAE tracks. The bands follow the measured
//! cost of each stage on a full song (upstream: 44 s total, of which the
//! autoregressive stages are about two thirds).

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// The AR half writing the ABC score.
    Score,
    /// The AR half writing the semantic codes.
    Semantic,
    /// The NAR half solving the acoustic latents.
    Acoustic,
    /// The VAE turning latents into audio.
    Decode,
    /// SheetSage2 reading a recording into a score.
    Transcribe,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Progress {
    pub stage: Stage,
    /// Overall fraction of the job, 0 to 1.
    pub fraction: f64,
    /// The stage's own counter, e.g. `412/750`.
    pub detail: String,
}

const SCORE: (f64, f64) = (0.0, 0.2);
const SEMANTIC: (f64, f64) = (0.2, 0.7);
const ACOUSTIC: (f64, f64) = (0.7, 0.95);
const DECODE: (f64, f64) = (0.95, 1.0);
/// The score's length is unknown until its end token; a typical full score
/// runs one to two and a half thousand tokens.
const TYPICAL_SCORE_TOKENS: f64 = 2000.0;

fn counter(line: &str, prefix: &str) -> Option<(f64, f64)> {
    let rest = line.strip_prefix(prefix)?.trim_start();
    let token = rest.split(|c: char| c == ',' || c == ':' || c.is_whitespace()).next()?;
    let (done, total) = token.split_once('/')?;
    let done: f64 = done.parse().ok()?;
    let total: f64 = total.parse().ok()?;
    (total > 0.0).then_some((done, total))
}

fn band((start, end): (f64, f64), fraction: f64) -> f64 {
    start + (end - start) * fraction.clamp(0.0, 1.0)
}

/// The progress of the job the engine is running now, or `None` when the last
/// thing the log says is that it finished, failed or was cancelled.
pub fn from_log(lines: &[String]) -> Option<Progress> {
    let mut current: Option<Progress> = None;
    for line in lines {
        let line = line.trim();
        if line.starts_with("[Pipeline] Done")
            || line.starts_with("[SheetSage] Transcribed")
            || line.contains("Cancelled at")
            || line.contains("FATAL")
        {
            current = None;
        } else if let Some((done, total)) = counter(line, "[AR] Score") {
            let fraction = (done / TYPICAL_SCORE_TOKENS.min(total)).min(0.95);
            current = Some(Progress { stage: Stage::Score, fraction: band(SCORE, fraction), detail: format!("{done}") });
        } else if line.starts_with("[AR] Score:") {
            current = Some(Progress { stage: Stage::Score, fraction: SCORE.1, detail: String::new() });
        } else if let Some((done, total)) = counter(line, "[AR] Semantic") {
            current = Some(Progress { stage: Stage::Semantic, fraction: band(SEMANTIC, done / total), detail: format!("{done}/{total}") });
        } else if line.starts_with("[AR] Semantic:") || line.starts_with("[Pipeline] Replay") {
            current = Some(Progress { stage: Stage::Semantic, fraction: SEMANTIC.1, detail: String::new() });
        } else if let Some((done, total)) = counter(line, "[NAR] Step") {
            current = Some(Progress { stage: Stage::Acoustic, fraction: band(ACOUSTIC, done / total), detail: format!("{done}/{total}") });
        } else if let Some((done, total)) = counter(line, "[VAE] Track") {
            current = Some(Progress { stage: Stage::Decode, fraction: band(DECODE, (done - 1.0) / total), detail: format!("{done}/{total}") });
        } else if let Some((done, total)) = counter(line, "[SheetSage] Decoding") {
            current = Some(Progress { stage: Stage::Transcribe, fraction: (done / total).min(0.99), detail: format!("{done}") });
        } else if line.starts_with("[SheetSage] Window") {
            current = Some(Progress { stage: Stage::Transcribe, fraction: 0.05, detail: String::new() });
        }
    }
    current
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_owned).collect()
    }

    #[test]
    fn follows_a_song_through_every_stage() {
        let log = lines(
            "[AR] Score prefill: 44 ms, CFG=1.00, top_k=30, budget=4096, songs=1, batch=1\n[AR] Score 500/4096",
        );
        let score = from_log(&log).unwrap();
        assert_eq!(score.stage, Stage::Score);
        assert!(score.fraction > 0.0 && score.fraction < 0.2);

        let semantic = from_log(&lines("[AR] Score: 505 tokens over 1 songs\n[AR] Semantic 300/750")).unwrap();
        assert_eq!(semantic.stage, Stage::Semantic);
        assert!((semantic.fraction - 0.4).abs() < 1e-9);

        let acoustic = from_log(&lines("[NAR] Step 16/32, 43 ms")).unwrap();
        assert_eq!(acoustic.stage, Stage::Acoustic);
        assert!((acoustic.fraction - 0.825).abs() < 1e-9);

        let decode = from_log(&lines("[VAE] Track 1/1: song 0 variation 0")).unwrap();
        assert_eq!(decode.stage, Stage::Decode);
    }

    #[test]
    fn a_finished_or_cancelled_job_is_not_progress() {
        assert!(from_log(&lines("[NAR] Step 32/32, 43 ms\n[Pipeline] Done: 1 tracks, 30.0 s of audio in 8.5 s")).is_none());
        assert!(from_log(&lines("[AR] Semantic 100/750\n[AR] Cancelled at step 120")).is_none());
        assert!(from_log(&[]).is_none());
    }

    #[test]
    fn the_next_job_after_a_finished_one_is_read_on_its_own() {
        let log = lines("[Pipeline] Done: 1 tracks\n[AR] Semantic 700/750");
        assert_eq!(from_log(&log).unwrap().stage, Stage::Semantic);
    }

    #[test]
    fn a_transcription_reports_its_own_stage() {
        let progress = from_log(&lines("[SheetSage] Window 0: 0.0 s to 30.0 s\n[SheetSage] Decoding 200/5120")).unwrap();
        assert_eq!(progress.stage, Stage::Transcribe);
    }
}
