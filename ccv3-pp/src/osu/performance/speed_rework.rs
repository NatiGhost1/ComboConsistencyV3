// Speed PP rework module — Combo Consistency V3
//
// This file holds the SpeedReworkParams tunables shared between the
// precompute pipeline (difficulty/speed_precal.rs) and any live-data
// fallback called from performance/mod.rs.
//
// In V1.1 the real computation lives in difficulty/speed_precal.rs and
// runs on owned SpeedObjectData. The live-data fallback functions below
// are kept for API compatibility with code that might still call them
// with a real object slice; in normal operation they're called with
// an empty slice and return 1.0 (no-op).

use crate::osu::difficulty::object::OsuDifficultyObject;

// ─── Tunables ────────────────────────────────────────────────────

pub struct SpeedReworkParams {
    /// Lower BPM where nerf starts (1/4 interpretation).
    pub nerf_lo_bpm: f64,
    /// Upper BPM of nerf zone (where curve flattens to dead zone).
    pub nerf_hi_bpm: f64,
    /// BPM where buff ramp starts.
    pub buff_lo_bpm: f64,
    /// BPM where buff ramp caps.
    pub buff_hi_bpm: f64,
    /// Multiplier at the bottom of the nerf zone.
    pub nerf_mult: f64,
    /// Multiplier cap for vanilla speed buff.
    pub buff_cap_vanilla: f64,
    /// Multiplier cap for autopilot speed buff.
    pub buff_cap_autopilot: f64,
    /// Min objects in sustained 1/4 chain to qualify for stamina override.
    pub sustained_chain_min: usize,
    /// Max stddev (ms) of delta_t in a sustained chain.
    pub sustained_ur_threshold: f64,
    /// Multiplier given to a qualifying sustained chain.
    pub sustained_bonus: f64,
    /// Floor for rhythm_quality in vanilla.
    pub rhythm_floor_vanilla: f64,
    /// Floor for rhythm_quality in autopilot.
    pub rhythm_floor_autopilot: f64,
    /// Ceiling for rhythm_quality bonus.
    pub rhythm_ceil: f64,
}

impl Default for SpeedReworkParams {
    fn default() -> Self {
        Self {
            nerf_lo_bpm: 300.0,
            nerf_hi_bpm: 360.0,
            buff_lo_bpm: 380.0,
            buff_hi_bpm: 440.0,
            nerf_mult: 0.92,
            buff_cap_vanilla: 1.08,
            buff_cap_autopilot: 1.12,
            sustained_chain_min: 150,
            sustained_ur_threshold: 50.0,
            sustained_bonus: 1.04,
            rhythm_floor_vanilla: 0.50,
            rhythm_floor_autopilot: 0.60,
            rhythm_ceil: 1.15,
        }
    }
}

// ─── Live-data fallback (no-op) ──────────────────────────────────
//
// These exist only so call sites in performance/mod.rs that pass an
// empty slice still type-check. The real path is precompute via
// difficulty/speed_precal.rs and reading the precomputed value off
// OsuDifficultyAttributes::speed_rework_mult_*.

pub fn compute_vanilla_speed_multiplier(
    _objects: &[OsuDifficultyObject<'_>],
    _dominant_tap_bpm: f64,
    _params: &SpeedReworkParams,
) -> f64 {
    1.0
}

pub fn compute_autopilot_speed_multiplier(
    _objects: &[OsuDifficultyObject<'_>],
    _dominant_tap_bpm: f64,
    _params: &SpeedReworkParams,
) -> f64 {
    1.0
}
