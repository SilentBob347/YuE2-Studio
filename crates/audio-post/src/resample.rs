//! Band-limited sample rate conversion for whole tracks.

use anyhow::{Context, Result};
use audioadapter::Adapter;
use audioadapter_buffers::direct::SequentialSliceOfVecs;
use rubato::{Fft, FixedSync, Resampler};

pub fn stereo(left: &[f32], right: &[f32], from: u32, to: u32) -> Result<(Vec<f32>, Vec<f32>)> {
    let frames = left.len().min(right.len());
    let input = vec![left[..frames].iter().map(|&s| s as f64).collect::<Vec<_>>(), right[..frames].iter().map(|&s| s as f64).collect()];
    let adapter = SequentialSliceOfVecs::new(&input, 2, frames).context("wrap the audio for resampling")?;
    let mut resampler = Fft::<f64>::new(from as usize, to as usize, 1024, 2, FixedSync::Both).context("build the resampler")?;
    let output = resampler.process_all(&adapter, frames, None).context("resample the audio")?;
    let produced = output.frames();
    let mut left = Vec::with_capacity(produced);
    let mut right = Vec::with_capacity(produced);
    for frame in 0..produced {
        left.push(output.read_sample(0, frame).unwrap_or(0.0) as f32);
        right.push(output.read_sample(1, frame).unwrap_or(0.0) as f32);
    }
    Ok((left, right))
}
