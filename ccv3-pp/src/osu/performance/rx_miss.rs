// CC V3 (Relax): acc-drop-weight-ranked miss penalty system.
//
// Concept
// -------
// The difficulty pass stores a list of hardness values, one per 4-note
// chunk of the map. At performance time we:
//
//   1. Distribute the player's total n100 + n50 counts across chunks
//      proportional to hardness (harder chunks absorb more drops).
//   2. Combine adjacent chunks into 8-note pairs (non-overlapping).
//   3. For each pair, compute an "acc drop weight" ∈ [0.85, 1.0] —
//      the weighted hit sum divided by the max possible. HIGHER weight
//      means a cleaner pair; LOWER weight means more drops.
//   4. Rank all pairs. Identify:
//        - average weight across all pairs
//        - top-5-lowest weight threshold (cleanest-5 boundary -> NO wait:
//          LOWEST weight = MOST drops = HARDEST → that's the "lowest
//          weight" group per the spec)
//        - top-5-highest weight threshold (cleanest-5 boundary)
//   5. For the first miss, locate the 8-note pair containing it (using
//      state.max_combo / 8 as the position proxy), and compare that
//      pair's weight against the thresholds:
//          weight in lowest-5 bucket   → MAX penalty
//          weight in highest-5 bucket  → MIN penalty
//          else                        → linearly interpolated
//   6. For subsequent misses, fall back to the 4-note chunk granularity
//      and re-apply the same logic at chunk resolution.
//
// None of this is strain-based. It is driven entirely by object timing
// (chunk hardness proxy) and score state (total n100/n50/miss counts
// plus state.max_combo for miss-position approximation).

/// Compute the RX miss penalty multiplier.
///
/// Parameters:
///   * `hardness_per_4notes` — the attrs field populated during the
///     difficulty pass. One f64 per 4-note chunk.
///   * `n300`, `n100`, `n50` — score-state totals.
///   * `misses` — raw miss count.
///   * `state_max_combo` — player's max combo (position proxy for first
///     miss).
///   * `map_max_combo` — map's max combo.
///
/// Returns a multiplier in roughly [0.40, 1.00] applied to pp.
/// Returns 1.0 on 0 misses (FC passes through untouched).
#[allow(clippy::too_many_arguments)]
pub(crate) fn rx_miss_multiplier(
    hardness_per_4notes: &[f64],
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
    if hardness_per_4notes.is_empty() || map_max_combo == 0 {
        // Degenerate / missing data: fall back to a flat modest penalty.
        return 0.80_f64.powf(f64::from(misses)).max(0.45);
    }

    // --- Step 1: distribute drops across chunks ---------------------
    let total_hardness: f64 = hardness_per_4notes.iter().sum();
    if total_hardness <= 0.0 {
        return 0.80_f64.powf(f64::from(misses)).max(0.45);
    }

    let n100_f = f64::from(n100);
    let n50_f = f64::from(n50);

    // drops_in_chunk[i] = (n100 + n50) share proportional to hardness[i]
    // We keep n100 and n50 separately so weights stay accurate.
    let chunks_n = hardness_per_4notes.len();
    let mut chunk_n100 = vec![0.0f64; chunks_n];
    let mut chunk_n50 = vec![0.0f64; chunks_n];
    for (i, h) in hardness_per_4notes.iter().enumerate() {
        let share = h / total_hardness;
        chunk_n100[i] = n100_f * share;
        chunk_n50[i] = n50_f * share;
    }

    // --- Step 2: build 8-note pairs (non-overlapping) ---------------
    // Pair i covers chunks [2i, 2i+1]. If an odd chunk remains at the
    // end, it becomes a short pair on its own.
    let pair_count = (chunks_n + 1) / 2;
    let mut pair_weights = Vec::with_capacity(pair_count);
    for p in 0..pair_count {
        let i0 = 2 * p;
        let i1 = i0 + 1;

        let (n_notes, pair_n100, pair_n50) = if i1 < chunks_n {
            (8.0, chunk_n100[i0] + chunk_n100[i1], chunk_n50[i0] + chunk_n50[i1])
        } else {
            (4.0, chunk_n100[i0], chunk_n50[i0])
        };

        let pair_n300 = (n_notes - pair_n100 - pair_n50).max(0.0);
        // Same weighting as accuracy_drop_based_miss_weight: 1.0 / 0.9 / 0.85
        let weighted_sum = pair_n300 * 1.0 + pair_n100 * 0.9 + pair_n50 * 0.85;
        let weight = (weighted_sum / n_notes).clamp(0.0, 1.0);
        pair_weights.push(weight);
    }

    if pair_weights.is_empty() {
        return 0.80_f64.powf(f64::from(misses)).max(0.45);
    }

    // --- Step 3: ranking ---------------------------------------------
    // avg, plus thresholds for the "top 5 lowest weight" and "top 5
    // highest weight" buckets.
    let avg_weight: f64 = pair_weights.iter().sum::<f64>() / pair_weights.len() as f64;

    let mut sorted_weights = pair_weights.clone();
    sorted_weights.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    // Lowest-5: the 5 smallest weights. Threshold = the 5th smallest.
    // If there are fewer than 5 pairs, use min weight (every pair
    // effectively qualifies).
    let low_idx = 4.min(sorted_weights.len() - 1);
    let lowest5_threshold = sorted_weights[low_idx];

    // Highest-5: threshold = the 5th largest.
    let high_idx = sorted_weights
        .len()
        .saturating_sub(5);
    let highest5_threshold = sorted_weights[high_idx];

    // --- Step 4: locate the first-miss pair --------------------------
    let combo_ratio = (f64::from(state_max_combo) / f64::from(map_max_combo)).clamp(0.0, 1.0);
    let miss_pair_idx = ((combo_ratio * pair_count as f64) as usize).min(pair_count - 1);
    let first_miss_weight = pair_weights[miss_pair_idx];

    // Per spec: a pair scoring "as the lowest weight (meaning more acc
    // drop) use max penalty". So LOW weight = MAX penalty.
    // Highest weight (cleanest) = MIN penalty.
    //
    // Scale from [lowest5_threshold, highest5_threshold] to [max_pen, min_pen].
    const MAX_PENALTY: f64 = 0.50; // multiplier, so low value = harsh
    const MIN_PENALTY: f64 = 0.85;

    let first_miss_mult = if first_miss_weight <= lowest5_threshold {
        MAX_PENALTY
    } else if first_miss_weight >= highest5_threshold {
        MIN_PENALTY
    } else {
        // Linear interp. t=0 at lowest, t=1 at highest.
        let range = (highest5_threshold - lowest5_threshold).max(1e-9);
        let t = ((first_miss_weight - lowest5_threshold) / range).clamp(0.0, 1.0);
        MAX_PENALTY + (MIN_PENALTY - MAX_PENALTY) * t
    };

    // --- Step 5: subsequent misses — chunk granularity ---------------
    // For misses beyond the first, use the 4-note chunks directly. Each
    // extra miss applies a damped multiplicative penalty based on where
    // *it* likely landed. Since we only know state.max_combo (the first
    // break), we approximate additional miss positions as distributed
    // across the map weighted by chunk hardness (harder chunks more
    // likely to absorb misses too).
    let extra_misses = misses.saturating_sub(1);
    let mut mult = first_miss_mult;

    if extra_misses > 0 {
        // Per-chunk weight, same formula as pair weight but on 4-note.
        let mut chunk_weights: Vec<f64> = Vec::with_capacity(chunks_n);
        for i in 0..chunks_n {
            let pair_n300 = (4.0 - chunk_n100[i] - chunk_n50[i]).max(0.0);
            let weighted_sum = pair_n300 * 1.0 + chunk_n100[i] * 0.9 + chunk_n50[i] * 0.85;
            chunk_weights.push((weighted_sum / 4.0).clamp(0.0, 1.0));
        }
        let chunk_avg: f64 =
            chunk_weights.iter().sum::<f64>() / chunk_weights.len() as f64;

        // Each extra miss:  mult *= subsequent_factor(chunk_avg)
        // chunk_avg is the "typical section cleanliness". We use it as a
        // gentle baseline — extra misses on a generally hard-playing map
        // cost less per-miss than extra misses on an otherwise clean run.
        //
        //   chunk_avg 0.85 → 0.95 per extra miss
        //   chunk_avg 1.00 → 0.88 per extra miss
        let subsequent_factor = {
            // t ∈ [0, 1] where 0 = dirty map, 1 = clean map
            let t = ((chunk_avg - 0.85) / 0.15).clamp(0.0, 1.0);
            // cleaner = harsher per miss
            0.95 - 0.07 * t
        };

        mult *= subsequent_factor.powf(f64::from(extra_misses));
    }

    // Floor at 0.40 so extreme runs don't zero out.
    mult.max(0.40)
}
