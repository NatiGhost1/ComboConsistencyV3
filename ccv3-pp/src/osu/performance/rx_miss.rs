// CC V3 (Relax): acc-drop-weight ranked miss penalty system (v2).
//
// Distributes total n100/n50 across 4-note chunks weighted by hardness,
// combines into 8-note pairs, assigns each pair a continuous percentile
// rank, then applies a smooth cubic penalty curve based on where each
// miss falls in the distribution.
//
// v2 over v1:
//   * Continuous cubic penalty across the full percentile range — only
//     the literal lowest-ranked pair hits absolute max penalty, only the
//     literal highest hits absolute min. Everything in between is smooth.
//   * Context factor: the pair BEFORE the miss pair is checked — if the
//     player was already dropping accuracy, the miss is less surprising
//     and the penalty is softened.
//   * BPM-relative factor: if the miss pair is significantly faster than
//     the map median delta, the miss is more justified → reduced penalty.
//     If significantly slower, extra penalty.
//   * Multi-miss: each subsequent miss is individually estimated to a
//     chunk position (distributed across the map tail weighted by
//     hardness) and scored at chunk-level percentile, with per-miss
//     damping that depends on that specific chunk's ranking.

/// Compute the RX miss penalty multiplier.
///
/// Returns [0.35, 1.00]. 1.0 on FC.
#[allow(clippy::too_many_arguments)]
pub(crate) fn rx_miss_multiplier(
    chunk_hardness: &[f64],
    chunk_avg_delta: &[f64],
    map_median_delta: f64,
    n300: u32,
    n100: u32,
    n50: u32,
    misses: u32,
    state_max_combo: u32,
    map_max_combo: u32,
) -> f64 {
    if misses == 0 {
        return 1.0;
    }
    let chunks_n = chunk_hardness.len();
    if chunks_n == 0 || map_max_combo == 0 {
        return flat_fallback(misses);
    }

    // ── Step 1: distribute n100/n50 across chunks ───────────────────
    let total_hardness: f64 = chunk_hardness.iter().sum();
    if total_hardness <= 0.0 {
        return flat_fallback(misses);
    }

    let n100_f = f64::from(n100);
    let n50_f = f64::from(n50);

    let mut chunk_n100 = vec![0.0f64; chunks_n];
    let mut chunk_n50 = vec![0.0f64; chunks_n];
    for (i, h) in chunk_hardness.iter().enumerate() {
        let share = h / total_hardness;
        chunk_n100[i] = n100_f * share;
        chunk_n50[i] = n50_f * share;
    }

    // ── Step 2: build non-overlapping 8-note pairs ──────────────────
    let pair_count = (chunks_n + 1) / 2;
    let mut pair_weights: Vec<f64> = Vec::with_capacity(pair_count);
    let mut pair_avg_deltas: Vec<f64> = Vec::with_capacity(pair_count);

    for p in 0..pair_count {
        let i0 = 2 * p;
        let i1 = i0 + 1;

        let (n_notes, p_n100, p_n50, avg_d) = if i1 < chunks_n {
            (
                8.0,
                chunk_n100[i0] + chunk_n100[i1],
                chunk_n50[i0] + chunk_n50[i1],
                (chunk_avg_delta.get(i0).copied().unwrap_or(0.0)
                    + chunk_avg_delta.get(i1).copied().unwrap_or(0.0))
                    / 2.0,
            )
        } else {
            (
                4.0,
                chunk_n100[i0],
                chunk_n50[i0],
                chunk_avg_delta.get(i0).copied().unwrap_or(0.0),
            )
        };

        let p_n300 = (n_notes - p_n100 - p_n50).max(0.0);
        let weighted_sum = p_n300 * 1.0 + p_n100 * 0.9 + p_n50 * 0.85;
        pair_weights.push((weighted_sum / n_notes).clamp(0.0, 1.0));
        pair_avg_deltas.push(avg_d);
    }

    if pair_weights.is_empty() {
        return flat_fallback(misses);
    }

    // ── Step 3: percentile ranks (pairs) ────────────────────────────
    let pair_percentile = percentile_ranks(&pair_weights);

    // ── Step 4: percentile ranks (chunks, for subsequent misses) ────
    let mut chunk_weights: Vec<f64> = Vec::with_capacity(chunks_n);
    for i in 0..chunks_n {
        let c_n300 = (4.0 - chunk_n100[i] - chunk_n50[i]).max(0.0);
        let ws = c_n300 * 1.0 + chunk_n100[i] * 0.9 + chunk_n50[i] * 0.85;
        chunk_weights.push((ws / 4.0).clamp(0.0, 1.0));
    }
    let chunk_percentile = percentile_ranks(&chunk_weights);

    // ── Step 5: score the first miss ────────────────────────────────
    let combo_ratio =
        (f64::from(state_max_combo) / f64::from(map_max_combo)).clamp(0.0, 1.0);
    let miss_pair_idx = ((combo_ratio * pair_count as f64) as usize).min(pair_count - 1);

    let first_pct = pair_percentile[miss_pair_idx];
    let first_delta = pair_avg_deltas[miss_pair_idx];

    // Cubic ease-in-out mapping from percentile to penalty:
    //   pct 0.0 (worst drops, lowest ranked) → MAX_PENALTY 0.45
    //   pct 0.5 (median)                     → ~0.67
    //   pct 1.0 (cleanest, highest ranked)   → MIN_PENALTY 0.88
    const MAX_PENALTY: f64 = 0.45;
    const MIN_PENALTY: f64 = 0.88;

    let t = cubic_ease(first_pct);
    let mut first_mult = MAX_PENALTY + (MIN_PENALTY - MAX_PENALTY) * t;

    // ── Context factor ──────────────────────────────────────────────
    // If the PREVIOUS pair was also struggling (bottom 30%), soften
    // penalty — the miss was a continuation of difficulty, not a fluke.
    if miss_pair_idx > 0 {
        let prev_pct = pair_percentile[miss_pair_idx - 1];
        if prev_pct < 0.30 {
            let relief = 0.08 * (1.0 - prev_pct / 0.30);
            first_mult += relief;
        }
    }
    // If the NEXT pair was also struggling, additional smaller relief
    // (confirms the miss was in a sustained hard zone, not isolated).
    if miss_pair_idx + 1 < pair_count {
        let next_pct = pair_percentile[miss_pair_idx + 1];
        if next_pct < 0.30 {
            let relief = 0.04 * (1.0 - next_pct / 0.30);
            first_mult += relief;
        }
    }

    // ── BPM-relative factor ─────────────────────────────────────────
    if map_median_delta > 0.0 && first_delta > 0.0 {
        // speed_ratio > 1 = section is faster than map median
        let speed_ratio = map_median_delta / first_delta;

        if speed_ratio > 1.10 {
            // Faster than normal → miss more justified → soften.
            // Scales 0% at 1.10× up to 12% relief at 1.6×+.
            let relief = ((speed_ratio - 1.10) / 0.50).clamp(0.0, 1.0) * 0.12;
            first_mult += relief;
        } else if speed_ratio < 0.90 {
            // Slower than normal → miss less justified → harshen.
            // Scales 0% at 0.90× down to 6% extra at 0.5×.
            let extra = ((0.90 - speed_ratio) / 0.40).clamp(0.0, 1.0) * 0.06;
            first_mult -= extra;
        }
    }

    first_mult = first_mult.clamp(MAX_PENALTY, MIN_PENALTY);

    // ── Step 6: subsequent misses — individually scored ──────────────
    let extra_misses = misses.saturating_sub(1);
    let mut mult = first_mult;

    if extra_misses > 0 {
        let first_chunk = ((combo_ratio * chunks_n as f64) as usize).min(chunks_n - 1);
        let tail_hardness: f64 = chunk_hardness[first_chunk..].iter().sum();

        let mut extra_applied = 0u32;
        if tail_hardness > 0.0 {
            for i in first_chunk..chunks_n {
                if extra_applied >= extra_misses {
                    break;
                }
                let share = chunk_hardness[i] / tail_hardness;
                let misses_here =
                    (f64::from(extra_misses) * share).round().max(0.0) as u32;
                let misses_here = misses_here.min(extra_misses - extra_applied);

                if misses_here > 0 {
                    let cpct = chunk_percentile[i];
                    let ct = cubic_ease(cpct);
                    // Per-miss range: [0.86, 0.97]
                    //   cpct 0.0 (hardest chunk) → 0.86 per miss (−14%)
                    //   cpct 1.0 (easiest chunk) → 0.97 per miss (−3%)
                    let per_miss = 0.86 + (0.97 - 0.86) * ct;

                    // BPM adjustment for this specific chunk too
                    let chunk_d = chunk_avg_delta.get(i).copied().unwrap_or(0.0);
                    let bpm_adj = if map_median_delta > 0.0 && chunk_d > 0.0 {
                        let sr = map_median_delta / chunk_d;
                        if sr > 1.15 {
                            // Faster → gentler per-miss
                            1.0 + ((sr - 1.15) / 0.50).clamp(0.0, 1.0) * 0.03
                        } else if sr < 0.85 {
                            // Slower → harsher per-miss
                            1.0 - ((0.85 - sr) / 0.35).clamp(0.0, 1.0) * 0.02
                        } else {
                            1.0
                        }
                    } else {
                        1.0
                    };

                    let adjusted = (per_miss * bpm_adj).clamp(0.84, 0.98);
                    mult *= adjusted.powf(f64::from(misses_here));
                    extra_applied += misses_here;
                }
            }
        }

        // Unplaced remainder gets flat mild penalty
        if extra_applied < extra_misses {
            let rem = extra_misses - extra_applied;
            mult *= 0.92_f64.powf(f64::from(rem));
        }
    }

    mult.max(0.35)
}

/// Cubic ease-in-out: steeper at extremes, flatter in middle.
/// Input [0, 1] → output [0, 1].
fn cubic_ease(x: f64) -> f64 {
    if x < 0.5 {
        4.0 * x * x * x
    } else {
        1.0 - (-2.0 * x + 2.0).powi(3) / 2.0
    }
}

/// Assign percentile rank to each element. 0.0 = lowest, 1.0 = highest.
fn percentile_ranks(values: &[f64]) -> Vec<f64> {
    let n = values.len();
    if n <= 1 {
        return vec![0.5; n];
    }
    let mut indices: Vec<usize> = (0..n).collect();
    indices.sort_by(|&a, &b| {
        values[a]
            .partial_cmp(&values[b])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut ranks = vec![0.0f64; n];
    for (rank, &idx) in indices.iter().enumerate() {
        ranks[idx] = rank as f64 / (n - 1) as f64;
    }
    ranks
}

fn flat_fallback(misses: u32) -> f64 {
    0.80_f64.powf(f64::from(misses)).max(0.40)
}