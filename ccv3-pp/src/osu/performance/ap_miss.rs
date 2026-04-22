// CC V3 (Autopilot): standalone miss scoring (v2).
//
// AP has assisted aim — tap misses are a different creature from legit
// misses. This module handles real misses and n50-derived penalties
// separately, with BPM-relative and n100-context adjustments.
//
// v2 enhancements over v1:
//   * BPM-relative miss weighting: misses at BPMs significantly above
//     the map's dominant_tap_bpm are more justified (harder to tap) and
//     get a lighter penalty. Misses at BPMs well below are less
//     justified and cost more.
//   * Continuous OD scaling for n50 penalty instead of binary <7.5.
//     At OD 5 nearly every n50 counts; at OD 9+ they barely matter.
//   * n100 context: a high n100 count signals ongoing struggle, which
//     softens the real-miss penalty (the player was already in trouble).
//   * Per-n50 decay is continuous with a smooth floor rather than
//     first=harsh/rest=flat. Uses 0.92^(n50 × od_factor) with a floor.
//
// CRITICAL: combo scaling applies ONLY to real misses, NEVER to the
// n50 penalty. Top tappers are human — n50s shouldn't cascade into
// combo-position penalties.

/// Compute the AP miss penalty multiplier.
///
/// Returns [0.45, 1.00]. 1.0 on FC with no n50s.
#[allow(clippy::too_many_arguments)]
pub(crate) fn ap_miss_multiplier(
    od: f64,
    dominant_tap_bpm: f64,
    chunk_hardness: &[f64],
    chunk_avg_delta: &[f64],
    n300: u32,
    n100: u32,
    n50: u32,
    real_misses: u32,
    state_max_combo: u32,
    map_max_combo: u32,
) -> f64 {
    let total_hits = n300 + n100 + n50 + real_misses;
    if total_hits == 0 {
        return 1.0;
    }

    // ── 1. Real-miss combo scaling ──────────────────────────────────
    // Only real misses contribute. Toned-down exponent (0.65 vs 0.8).
    // Combo scaling is NOT applied to n50 penalties per spec.
    let combo_scaling = if real_misses > 0 && map_max_combo > 0 {
        let ratio =
            (f64::from(state_max_combo) / f64::from(map_max_combo)).clamp(0.0, 1.0);
        (0.70 + 0.30 * ratio.powf(0.65)).min(1.0)
    } else {
        1.0
    };

    // ── 2. Real-miss per-miss penalty with BPM-relative weighting ───
    let real_miss_penalty = if real_misses > 0 {
        // Estimate where the first miss happened
        let combo_ratio = if map_max_combo > 0 {
            (f64::from(state_max_combo) / f64::from(map_max_combo)).clamp(0.0, 1.0)
        } else {
            0.5
        };

        // Determine the BPM at the miss location
        let miss_bpm = estimate_bpm_at(combo_ratio, chunk_avg_delta, dominant_tap_bpm);

        // BPM-relative factor: how does the miss BPM compare to dominant?
        //   miss_bpm / dominant  > 1.15  → faster section → miss justified → softer
        //   miss_bpm / dominant  < 0.85  → slower section → miss unjustified → harsher
        //   else                         → neutral
        let bpm_factor = if dominant_tap_bpm > 0.0 && miss_bpm > 0.0 {
            let ratio = miss_bpm / dominant_tap_bpm;
            if ratio > 1.15 {
                // Softer: base per-miss decay increases toward 0.96
                let relief = ((ratio - 1.15) / 0.45).clamp(0.0, 1.0);
                0.93 + 0.03 * relief // 0.93 → 0.96 as BPM climbs
            } else if ratio < 0.85 {
                // Harsher: base per-miss decay drops toward 0.88
                let extra = ((0.85 - ratio) / 0.35).clamp(0.0, 1.0);
                0.93 - 0.05 * extra // 0.93 → 0.88 as BPM drops
            } else {
                0.93 // neutral
            }
        } else {
            0.93
        };

        // n100 context: if the player got lots of n100s (≥ 5% of hits),
        // it signals ongoing struggle before the miss → soften per-miss.
        let n100_ratio = f64::from(n100) / f64::from(total_hits);
        let n100_relief = if n100_ratio > 0.05 {
            // Each % of n100 above 5% adds 0.5% relief, up to 3%
            ((n100_ratio - 0.05) * 50.0).clamp(0.0, 1.0) * 0.03
        } else {
            0.0
        };

        let per_miss = (bpm_factor + n100_relief).clamp(0.86, 0.97);
        per_miss.powf(f64::from(real_misses)).max(0.45)
    } else {
        1.0
    };

    // ── 3. n50 penalty — continuous OD scaling ──────────────────────
    // Instead of a binary <7.5 threshold, the n50 penalty scales
    // continuously with OD. At low OD, n50s are essentially free (the
    // hit window is huge) so each one should cost more. At high OD,
    // n50s represent genuine timing pressure and cost less.
    //
    // od_factor: 1.0 at OD 0 → 0.0 at OD 10+
    //   maps the OD into how much each n50 "matters"
    //
    // Per-n50 decay: 0.92^(n50_count × od_factor)
    //   OD 5, 3 n50s → 0.92^(3 × 0.50) = 0.92^1.5 = 0.881
    //   OD 8, 3 n50s → 0.92^(3 × 0.20) = 0.92^0.6 = 0.952
    //   OD 10, any   → 0.92^0 = 1.0 (no penalty)
    //
    // Floor at 0.70 so extreme n50 counts don't zero out.
    // COMBO SCALING DOES NOT APPLY to this term.
    let n50_penalty = if n50 > 0 {
        let od_factor = (1.0 - od / 10.0).clamp(0.0, 1.0);
        let exponent = f64::from(n50) * od_factor;

        if exponent > 0.0 {
            // First n50 hits harder: 0.88 base for the first, then 0.92 for the rest
            let first_hit = 0.88_f64.powf(od_factor.min(1.0));
            let rest = if n50 > 1 {
                0.92_f64.powf((f64::from(n50) - 1.0) * od_factor)
            } else {
                1.0
            };
            (first_hit * rest).max(0.70)
        } else {
            1.0
        }
    } else {
        1.0
    };

    // ── 4. BPM-variability bonus ────────────────────────────────────
    // If the map has high BPM variation (lots of speed changes), AP
    // tapping is genuinely harder even with aim assist. Give a small
    // relief to the overall penalty.
    let bpm_variability_relief = if chunk_avg_delta.len() >= 4 {
        let mean_d: f64 = chunk_avg_delta.iter().sum::<f64>() / chunk_avg_delta.len() as f64;
        if mean_d > 0.0 {
            let variance: f64 = chunk_avg_delta
                .iter()
                .map(|d| (d - mean_d).powi(2))
                .sum::<f64>()
                / chunk_avg_delta.len() as f64;
            let cv = variance.sqrt() / mean_d; // coefficient of variation
            // cv > 0.15 = meaningful BPM variation → up to 5% relief
            if cv > 0.15 {
                ((cv - 0.15) / 0.30).clamp(0.0, 1.0) * 0.05
            } else {
                0.0
            }
        } else {
            0.0
        }
    } else {
        0.0
    };

    // ── Compose ─────────────────────────────────────────────────────
    // combo_scaling × real_miss_penalty are the "real miss" path.
    // n50_penalty is the "tap fumble" path (no combo scaling).
    // They multiply together — a play with both real misses AND n50s
    // gets hit by both, but they don't interact via combo position.
    let raw = combo_scaling * real_miss_penalty * n50_penalty + bpm_variability_relief;

    raw.clamp(0.45, 1.0)
}

/// Estimate the effective 1/4 BPM at a given position in the map.
///
/// Uses chunk_avg_delta to look up the delta at the position, then
/// converts to BPM. Falls back to dominant_tap_bpm if chunk data
/// is unavailable.
fn estimate_bpm_at(combo_ratio: f64, chunk_avg_delta: &[f64], dominant_bpm: f64) -> f64 {
    if chunk_avg_delta.is_empty() {
        return dominant_bpm;
    }
    let n = chunk_avg_delta.len();
    let idx = ((combo_ratio * n as f64) as usize).min(n - 1);
    let delta = chunk_avg_delta[idx];
    if delta > 0.0 {
        15_000.0 / delta // 1/4 BPM
    } else {
        dominant_bpm
    }
}