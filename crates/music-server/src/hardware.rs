//! The machine the studio runs on: its GPU, how much memory it has, and the
//! model set that fits in it.

use std::{process::Command, sync::OnceLock};

use serde::Serialize;
use sysinfo::System;

/// The set a clean install selects when no card fits any set: the lightest,
/// so the model manager still names a concrete local target.
const FALLBACK_PROFILE: &str = "light";

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Hardware {
    pub gpu_name: Option<String>,
    pub total_vram_gb: f64,
    pub total_ram_gb: f64,
    /// The model set id that fits this card, none when no set does.
    pub recommended: Option<&'static str>,
}

/// `nvidia-smi` costs tens of milliseconds and the setup screen polls status
/// once per second while a download runs. The machine's GPU does not change
/// inside one process lifetime, so probe it once.
fn probe() -> &'static Hardware {
    static HARDWARE: OnceLock<Hardware> = OnceLock::new();
    HARDWARE.get_or_init(|| {
        let mut system = System::new();
        system.refresh_memory();
        let total_ram_gb = system.total_memory() as f64 / 1_000_000_000.0;
        let (gpu_name, total_vram_gb) = match nvidia_smi() {
            Some((name, vram)) => (Some(name), vram),
            None => (None, 0.0),
        };
        Hardware { gpu_name, total_vram_gb, total_ram_gb, recommended: profile_for_vram(total_vram_gb) }
    })
}

pub fn hardware() -> Hardware {
    probe().clone()
}

/// VRAM tiers follow the strict-eviction peak of each set: the larger backbone
/// half plus a full-context KV cache (2.7 GB per set, two under guidance) and
/// the compute buffers. Upstream measured 5.8 GB for a 65 s song in Q8_0.
fn profile_for_vram(total_vram_gb: f64) -> Option<&'static str> {
    if total_vram_gb >= 12.0 {
        Some("native")
    } else if total_vram_gb >= 8.0 {
        Some("quality-q8")
    } else if total_vram_gb >= 7.0 {
        Some("balanced")
    } else if total_vram_gb >= 5.5 {
        Some("light")
    } else {
        None
    }
}

/// Chooses the complete local set on a clean install. This only records a
/// selection; downloading any component remains a separate user action.
pub fn recommended_local_profile() -> &'static str {
    probe().recommended.unwrap_or(FALLBACK_PROFILE)
}

fn nvidia_smi() -> Option<(String, f64)> {
    let output = Command::new("nvidia-smi").args(["--query-gpu=name,memory.total", "--format=csv,noheader,nounits"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&output.stdout).lines().next()?.trim().to_owned();
    let (name, memory) = line.rsplit_once(',')?;
    Some((name.trim().into(), memory.trim().parse::<f64>().ok()? / 1024.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recommendation_follows_yue2_vram_tiers() {
        assert_eq!(profile_for_vram(24.0), Some("native"));
        assert_eq!(profile_for_vram(11.9), Some("quality-q8"));
        assert_eq!(profile_for_vram(8.0), Some("quality-q8"));
        assert_eq!(profile_for_vram(7.6), Some("balanced"));
        assert_eq!(profile_for_vram(6.0), Some("light"));
        assert_eq!(profile_for_vram(4.0), None);
        assert_eq!(profile_for_vram(0.0), None);
    }

    #[test]
    fn every_recommendation_is_a_declared_profile() {
        for vram in [6.0, 7.5, 10.0, 16.0, 24.0] {
            assert!(crate::model_manager::profile_exists(profile_for_vram(vram).unwrap()));
        }
        assert!(crate::model_manager::profile_exists(FALLBACK_PROFILE));
    }
}
