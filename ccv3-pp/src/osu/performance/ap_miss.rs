// CC V3 (Autopilot): standalone miss scoring.
//
// AP has assisted aim, so the usual miss penalty model doesn't really
// fit — a tap miss with aim held by the game is a different creature
// from a legit miss. This module applies:
//
//   1. Below OD 7.5, every n50 is effectively ~1 extra miss *for pp
//      purposes* (players shouldn't be getting 50s on easy-OD with
//      autopilot aim unless they're dropping notes).
//   2. BUT combo scaling does NOT apply to these n50-derived misses —
//      top players are still human and fumbling a handful of taps
//      shouldn't catastrophically tank pp the way a real combo break
//      would.
//   3. For the weight decay: very extreme per-n50 decay for the FIRST
//      n50 (cap established quickly), then every subsequent n50 is
//      capped at a floor so pp doesn't keep hemorrhaging from what is
//      effectively tapping nerves.
//
// Returns a multiplier in [0.50, 1.00] applied to final pp.

/// Parameters:
///   * `od` — overall difficulty (after mods).
///   * `n50` — score-state n50 count.
///   * `real_misses` — actual miss count (for combo-scaling path).
///   * `state_max_combo`, `map_max_combo` — for the combo scaling that
///     applies ONLY to real misses.
pub(crate) fn ap_miss_multiplier(
    od: f64,
    n50: u32,
    real_misses: u32,
    state_max_combo: u32,
    map_max_combo: u32,
) -> f64 {
    // --- 1. Real-miss combo scaling ---------------------------------
    // Only real misses contribute here. This matches a toned-down
    // standard combo scaling: (player_combo / map_combo)^0.65.
    let combo_scaling = if real_misses > 0 && map_max_combo > 0 {
        let ratio =
            (f64::from(state_max_combo) / f64::from(map_max_combo)).clamp(0.0, 1.0);
        (0.70 + 0.30 * ratio.powf(0.65)).min(1.0)
    } else {
        1.0
    };

    // --- 2. Real-miss flat penalty ----------------------------------
    // Light per-miss decay on top of combo scaling.
    let real_miss_penalty = if real_misses > 0 {
        0.93_f64.powf(f64::from(real_misses)).max(0.50)
    } else {
        1.0
    };

    // --- 3. n50-based penalty (ONLY below OD 7.5) -------------------
    // The spec: very extreme for the first n50, then floored at a
    // constant for every subsequent n50 so pp doesn't cascade to 0.
    //
    //   OD >= 7.5       → no n50 penalty at all
    //   OD < 7.5, 0 n50 → 1.0
    //   OD < 7.5, 1 n50 → 0.80 (extreme drop)
    //   OD < 7.5, 2 n50 → 0.75 (floor)
    //   OD < 7.5, 3+    → stays at 0.75 floor
    //
    // Note: combo scaling does NOT apply to this term. Spec: "do not
    // apply the combo scaling for n50's even tho … they are still
    // human and applying combo scale for something that isnt an
    // 'actual miss' is incredibly annoying".
    let n50_penalty = if od < 7.5 && n50 >= 1 {
        if n50 == 1 {
            0.80
        } else {
            0.75
        }
    } else {
        1.0
    };

    // Compose. Floor at 0.50 so nothing degenerate zeros out.
    (combo_scaling * real_miss_penalty * n50_penalty).max(0.50)
}
