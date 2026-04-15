// Pre-computes speed rework multipliers during the difficulty pipeline.
// Results are stored on OsuDifficultyAttributes so the performance
// calculator can read them directly without needing access to the
// difficulty object slice.

use super::tap_bpm::SpeedObjectData;
use crate::osu::performance::speed_rework::SpeedReworkParams;

/// Compute both vanilla and autopilot speed multipliers from owned data.
/// Returns (vanilla_mult, autopilot_mult).
pub fn precompute_speed_rework_from_owned(
    objects: &[SpeedObjectData],
    dominant_tap_bpm: f64,
) -> (f64, f64) {
    let p = SpeedReworkParams::default();
    let vanilla = compute_mult(objects, dominant_tap_bpm, &p, false);
    let autopilot = compute_mult(objects, dominant_tap_bpm, &p, true);
    (vanilla, autopilot)
}

fn compute_mult(
    objects: &[SpeedObjectData],
    dominant_tap_bpm: f64,
    p: &SpeedReworkParams,
    autopilot: bool,
) -> f64 {
    // 1) BPM curve
    let mut bpm_mult = bpm_curve(dominant_tap_bpm, p, autopilot);

    // 2) Sustained chain override: if we're in the nerf zone AND a long
    //    consecutive 1/4 chain exists with low UR, upgrade the bpm_mult
    //    from nerf to slight buff (saves real stamina streamers).
    let in_nerf_zone = dominant_tap_bpm > p.nerf_lo_bpm
        && dominant_tap_bpm <= p.nerf_hi_bpm;
    if in_nerf_zone && find_sustained_chain(objects, p) {
        bpm_mult = p.sustained_bonus;
    }

    // 3) Rhythm quality (anti-vibro)
    let rq = rhythm_quality(objects, p, autopilot);

    bpm_mult * rq
}

// ─── BPM curve ───────────────────────────────────────────────────────

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

fn bpm_curve(bpm: f64, p: &SpeedReworkParams, autopilot: bool) -> f64 {
    let buff_cap = if autopilot {
        p.buff_cap_autopilot
    } else {
        p.buff_cap_vanilla
    };

    if bpm <= p.nerf_lo_bpm {
        // Below 300 BPM — untouched.
        1.0
    } else if bpm <= p.nerf_hi_bpm {
        // 300–360 ramp from 1.0 down to nerf_mult
        let t = (bpm - p.nerf_lo_bpm) / (p.nerf_hi_bpm - p.nerf_lo_bpm);
        lerp(1.0, p.nerf_mult, t)
    } else if bpm < p.buff_lo_bpm {
        // 360–380 dead zone (flat nerf).
        p.nerf_mult
    } else if bpm < p.buff_hi_bpm {
        // 380–440 buff ramp.
        let t = (bpm - p.buff_lo_bpm) / (p.buff_hi_bpm - p.buff_lo_bpm);
        lerp(1.0, buff_cap, t)
    } else {
        // >=440 BPM cap.
        buff_cap
    }
}

// ─── Sustained chain detector ────────────────────────────────────────

fn find_sustained_chain(objects: &[SpeedObjectData], p: &SpeedReworkParams) -> bool {
    if objects.len() < p.sustained_chain_min {
        return false;
    }

    let lo_delta = 15_000.0 / p.nerf_hi_bpm;
    let hi_delta = 15_000.0 / p.nerf_lo_bpm;

    let mut chain_start = 0usize;

    for i in 1..objects.len() {
        let dt = objects[i].delta_time;

        if dt >= lo_delta && dt <= hi_delta {
            let len = i - chain_start + 1;

            if len >= p.sustained_chain_min {
                let slice = &objects[chain_start..=i];
                let n = slice.len() as f64;
                let mean: f64 = slice.iter().map(|o| o.delta_time).sum::<f64>() / n;
                let variance: f64 = slice
                    .iter()
                    .map(|o| (o.delta_time - mean).powi(2))
                    .sum::<f64>()
                    / n;
                let stddev = variance.sqrt();

                if stddev <= p.sustained_ur_threshold {
                    return true;
                }
            }
        } else {
            chain_start = i;
        }
    }

    false
}

// ─── Rhythm quality (anti-vibro) ─────────────────────────────────────

/// Combines delta-time Shannon entropy with spatial spread to
/// distinguish real rhythmic tapping from vibro spam.
///
/// Returns a value in [floor, ceil].
fn rhythm_quality(
    objects: &[SpeedObjectData],
    p: &SpeedReworkParams,
    autopilot: bool,
) -> f64 {
    if objects.len() < 8 {
        return 1.0;
    }

    // Delta entropy: bin delta_time into 16 logarithmic buckets.
    let mut bins = [0u32; 16];
    let mut total = 0u32;

    for obj in objects.iter().skip(1) {
        let dt = obj.delta_time;
        if dt <= 0.0 {
            continue;
        }
        let log_dt = (dt.max(10.0).min(500.0) / 10.0).log2();
        let idx = ((log_dt / 5.65) * 15.0).round() as usize;
        let idx = idx.min(15);
        bins[idx] += 1;
        total += 1;
    }

    if total == 0 {
        return 1.0;
    }

    let mut entropy = 0.0_f64;
    for &count in &bins {
        if count == 0 {
            continue;
        }
        let p_i = count as f64 / total as f64;
        entropy -= p_i * p_i.log2();
    }
    let entropy_norm = (entropy / 4.0).clamp(0.0, 1.0);

    // Spatial spread: average euclidean distance between consecutive objects.
    let mut total_dist = 0.0_f64;
    let mut count_dist = 0u32;

    for pair in objects.windows(2) {
        let dx = (pair[1].pos_x - pair[0].pos_x) as f64;
        let dy = (pair[1].pos_y - pair[0].pos_y) as f64;
        total_dist += (dx * dx + dy * dy).sqrt();
        count_dist += 1;
    }

    let avg_dist = if count_dist > 0 {
        total_dist / count_dist as f64
    } else {
        0.0
    };
    let spatial_norm = (avg_dist / 120.0).clamp(0.0, 1.0);

    // Combine: entropy 60%, spatial 40%
    let raw = entropy_norm * 0.6 + spatial_norm * 0.4;

    let floor = if autopilot {
        p.rhythm_floor_autopilot
    } else {
        p.rhythm_floor_vanilla
    };

    let scaled = floor + raw * (p.rhythm_ceil - floor);
    scaled.clamp(floor, p.rhythm_ceil)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_objects(n: usize, dt: f64, spacing: f32) -> Vec<SpeedObjectData> {
        (0..n)
            .map(|i| SpeedObjectData {
                delta_time: dt,
                pos_x: (i as f32) * spacing,
                pos_y: 192.0,
            })
            .collect()
    }

    #[test]
    fn test_vibro_gets_low_rhythm() {
        let objects: Vec<SpeedObjectData> = (0..200)
            .map(|_| SpeedObjectData {
                delta_time: 40.0,
                pos_x: 256.0,
                pos_y: 192.0,
            })
            .collect();
        let p = SpeedReworkParams::default();
        let rq = rhythm_quality(&objects, &p, false);
        assert!(rq < 0.65, "Vibro should get low rhythm quality, got {rq}");
    }

    #[test]
    fn test_sustained_chain_detected() {
        let dt = 15_000.0 / 320.0;
        let objects = make_objects(200, dt, 2.0);
        let p = SpeedReworkParams::default();
        assert!(find_sustained_chain(&objects, &p));
    }

    #[test]
    fn test_short_chain_not_detected() {
        let dt = 15_000.0 / 320.0;
        let objects = make_objects(50, dt, 2.0);
        let p = SpeedReworkParams::default();
        assert!(!find_sustained_chain(&objects, &p));
    }

    #[test]
    fn test_full_precompute() {
        let dt = 15_000.0 / 400.0;
        let objects = make_objects(300, dt, 8.0);
        let (v, ap) = precompute_speed_rework_from_owned(&objects, 400.0);
        assert!(v > 1.0, "400 BPM with spacing should buff vanilla, got {v}");
        assert!(ap > v, "AP buff cap should be higher, got v={v} ap={ap}");
    }
}
