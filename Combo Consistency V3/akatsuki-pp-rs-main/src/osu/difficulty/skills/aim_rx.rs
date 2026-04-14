// Relax-specific aim evaluator for Combo Consistency V3.
//
// This is a sibling to AimEvaluator in aim.rs. When the Aim skill is
// constructed with has_relax=true, strain_value_at dispatches here instead
// of the vanilla evaluator. Same input signature, same output meaning
// (an aim strain contribution), but with different scoring rules:
//
//   1. Enhanced aim-control reward: wide/acute/velocity-change bonuses
//      get a +8% uplift because RX players specifically train aim
//      control (no tap timing noise to hide imprecision).
//
//   2. Harsher aim-slop nerf: if recent pattern has very low angle
//      variance AND low velocity change, nerf it — this is repetitive
//      cursor-dragging that RX trivializes.
//
//   3. Slow-slider-velocity nerf: slider_bonus tapers hard below a
//      velocity floor, since slow sliders are essentially free movement
//      on RX (no aim precision needed to follow).
//
//   4. Cross-screen same-distance nerf: patterns where curr_dist is
//      close to prev_dist AND neither is truly edge-to-edge get an
//      extra nerf. This catches "big jumps that are really just
//      constant-distance spam". Exempt above 350 BPM 1/2 effective
//      (these patterns are genuinely hard at that speed).
//
//   5. Repetition buff at high BPM: preserved from vanilla evaluator.
//      410-500 BPM effective (1/2) fades out the repetition penalty
//      and replaces it with a small buff, since mechanically holding
//      repetitive aim at that speed is genuinely hard.

use std::f64::consts::{FRAC_PI_2, PI};

use crate::any::difficulty::object::IDifficultyObject;
use crate::osu::difficulty::object::OsuDifficultyObject;
use crate::util::float_ext::FloatExt;

pub struct AimRxEvaluator;

/// Collect up to `window` previous angles (including curr) and return
/// (mean, stddev). Returns (0.0, 0.0) if fewer than 3 angles could be
/// collected — too little data for meaningful variance.
fn windowed_angle_stats<'a>(
    curr: &'a OsuDifficultyObject<'a>,
    diff_objects: &'a [OsuDifficultyObject<'a>],
    window: usize,
) -> (f64, f64, usize) {
    let mut angles: Vec<f64> = Vec::with_capacity(window);

    if let Some(a) = curr.angle {
        angles.push(a);
    }
    for back in 0..window {
        match curr.previous(back, diff_objects) {
            Some(prev) => {
                if let Some(a) = prev.angle {
                    angles.push(a);
                }
            }
            None => break,
        }
    }

    let n = angles.len();
    if n < 3 {
        return (0.0, 0.0, n);
    }

    let mean: f64 = angles.iter().sum::<f64>() / n as f64;
    let variance: f64 = angles.iter().map(|a| (a - mean).powi(2)).sum::<f64>() / n as f64;
    let stddev = variance.sqrt();

    (mean, stddev, n)
}

impl AimRxEvaluator {
    // Base multipliers. Slightly uplifted from vanilla (1.45/1.90/1.35/0.70)
    // to reward aim-control playstyles on RX.
    const WIDE_ANGLE_MULTIPLIER: f64 = 1.56;      // +7.6% vs vanilla 1.45
    const ACUTE_ANGLE_MULTIPLIER: f64 = 2.05;     // +7.9% vs vanilla 1.90
    const SLIDER_MULTIPLIER: f64 = 1.20;          // -11% (slow sliders farmed on RX)
    const VELOCITY_CHANGE_MULTIPLIER: f64 = 0.78; // +11% (vel change = real aim control)

    // Slow slider velocity threshold. Below this, slider_bonus tapers.
    // Typical slow slider ~0.3 normalized-px/ms. Fast ~2.0+.
    const SLOW_SLIDER_VEL_FLOOR: f64 = 0.55;

    // Cross-screen constant-distance threshold. If |curr - prev| / curr
    // is below this, the pattern is "repetitive distance" — nerf unless
    // the distance is actually edge-to-edge.
    const CONSTANT_DIST_RATIO: f64 = 0.18;

    // Edge-to-edge cutoff. Playfield in normalized-radius coordinates is
    // ~512 wide; genuine full-stretch jumps are 400+. Below this, a
    // "constant distance" pattern counts as slop; above this it's
    // genuinely hard and gets full credit.
    const EDGE_TO_EDGE_THRESHOLD: f64 = 400.0;

    // BPM condition for the constant-distance nerf. 350 BPM 1/2 → 
    // strain_time = 60000 / (350 * 2) ≈ 85.7 ms. Above that effective
    // BPM (i.e. strain_time < 85.7), these patterns are actually hard
    // and don't get nerfed.
    const CONSTANT_DIST_BPM_STRAIN_TIME: f64 = 85.7;

    // BPM condition for the extreme-flow nerf. 410 BPM 1/4 → 
    // strain_time = 60000 / (410 * 4) ≈ 35.58 ms. Above that effective
    // BPM (i.e. strain_time < 35.58), these patterns are actually hard
    // and don't get nerfed.
    const FLOW_CONSTANT_DIST_BPM_STRAIN_TIME: f64 = 36.58

    // Windowed angle variance: number of previous diff objects to sample
    // when computing variance. 6 covers ~1.5 beats at moderate tempo —
    // long enough to see a real pattern, short enough that unrelated
    // sections don't bleed in.
    const ANGLE_WINDOW: usize = 6;

    // Flow aim detection thresholds. A flow pattern has:
    //   - mean angle above 2.0 rad (~115°, "wide curve")
    //   - stddev below 0.3 rad (~17°, "consistent direction")
    // Both must hold across the window for the nerf to fire.
    const FLOW_MEAN_ANGLE_THRESHOLD: f64 = 2.0;
    const FLOW_STDDEV_THRESHOLD: f64 = 0.3;

    // Extreme flow nerf: max cut on aim strain when flow signature is
    // fully matched (stddev at 0, mean at π). −50% at worst case.
    const FLOW_MAX_NERF: f64 = 0.50;

    pub fn evaluate_diff_of<'a>(
        curr: &'a OsuDifficultyObject<'a>,
        diff_objects: &'a [OsuDifficultyObject<'a>],
        with_slider_travel_dist: bool,
    ) -> f64 {
        let osu_curr_obj = curr;

        let Some((osu_last_last_obj, osu_last_obj)) = curr
            .previous(1, diff_objects)
            .zip(curr.previous(0, diff_objects))
            .filter(|(_, last)| !(curr.base.is_spinner() || last.base.is_spinner()))
        else {
            return 0.0;
        };

        // ── Velocities ──────────────────────────────────────────────
        let mut curr_vel = osu_curr_obj.lazy_jump_dist / osu_curr_obj.strain_time;

        if osu_last_obj.base.is_slider() && with_slider_travel_dist {
            let travel_vel = osu_last_obj.travel_dist / osu_last_obj.travel_time;
            let movement_vel = osu_curr_obj.min_jump_dist / osu_curr_obj.min_jump_time;
            curr_vel = curr_vel.max(movement_vel + travel_vel);
        }

        let mut prev_vel = osu_last_obj.lazy_jump_dist / osu_last_obj.strain_time;

        if osu_last_last_obj.base.is_slider() && with_slider_travel_dist {
            let travel_vel = osu_last_last_obj.travel_dist / osu_last_last_obj.travel_time;
            let movement_vel = osu_last_obj.min_jump_dist / osu_last_obj.min_jump_time;
            prev_vel = prev_vel.max(movement_vel + travel_vel);
        }

        let mut wide_angle_bonus = 0.0;
        let mut acute_angle_bonus = 0.0;
        let mut slider_bonus = 0.0;
        let mut vel_change_bonus = 0.0;

        let mut aim_strain = curr_vel;

        // ── Angle bonuses (only when rhythm is consistent) ──────────
        if osu_curr_obj.strain_time.max(osu_last_obj.strain_time)
            < 1.25 * osu_curr_obj.strain_time.min(osu_last_obj.strain_time)
        {
            if let Some(((curr_angle, last_angle), _last_last_angle)) = osu_curr_obj
                .angle
                .zip(osu_last_obj.angle)
                .zip(osu_last_last_obj.angle)
            {
                let angle_bonus = curr_vel.min(prev_vel);

                wide_angle_bonus = Self::calc_wide_angle_bonus(curr_angle);
                acute_angle_bonus = Self::calc_acute_angle_bonus(curr_angle);

                // Only buff delta_time exceeding 300 bpm 1/2.
                if osu_curr_obj.strain_time > 100.0 {
                    acute_angle_bonus = 0.0;
                } else {
                    let base1 =
                        (FRAC_PI_2 * ((100.0 - osu_curr_obj.strain_time) / 25.0).min(1.0)).sin();
                    let base2 = (FRAC_PI_2
                        * ((osu_curr_obj.lazy_jump_dist).clamp(50.0, 100.0) - 50.0)
                        / 50.0)
                        .sin();

                    acute_angle_bonus *= Self::calc_acute_angle_bonus(last_angle)
                        * angle_bonus.min(125.0 / osu_curr_obj.strain_time)
                        * base1.powf(2.0)
                        * base2.powf(2.0);
                }

                // ── BPM-aware repetition handling (preserved from vanilla) ──
                // Below 410 BPM effective: full repetition penalty.
                // 410–500 BPM: penalty fades out, replaced by a buff.
                // Above 500 BPM: no penalty, full repetition buff.
                let eff_bpm = 30_000.0 / osu_curr_obj.strain_time;
                let high_bpm_t = ((eff_bpm - 410.0) / 90.0).clamp(0.0, 1.0);

                // CC V3: Windowed angle variance replaces the old pairwise
                // repetition check. Pairwise only saw "curr vs last" which
                // missed flat grid patterns where each adjacent pair looks
                // slightly different but the whole window is uniform.
                //
                // stddev_norm: 0.0 = perfectly repetitive, 1.0 = maximum
                // variance (π rad spread). We use min(stddev / 1.2, 1.0)
                // so stddev ≥ 1.2 rad (~69°) counts as "fully varied".
                let (_win_mean, win_stddev, win_n) =
                    windowed_angle_stats(osu_curr_obj, diff_objects, Self::ANGLE_WINDOW);

                let variance_factor = if win_n >= 3 {
                    (win_stddev / 1.2).clamp(0.0, 1.0)
                } else {
                    1.0 // not enough data → assume varied (no penalty)
                };

                // Repetition strength: 1.0 when totally flat, 0.0 when
                // fully varied. At high BPM this gets inverted into a buff.
                let rep_strength = 1.0 - variance_factor;

                // Wide angle repetition (variance-based)
                let wide_penalty = rep_strength * (1.0 - high_bpm_t);
                let wide_rep_buff = high_bpm_t * 0.15;
                wide_angle_bonus *= angle_bonus
                    * ((1.0 - wide_penalty + wide_rep_buff).max(0.0));

                // Acute angle repetition (same variance measure, gentler curve)
                let acute_penalty = rep_strength * 0.7 * (1.0 - high_bpm_t);
                let acute_rep_buff = high_bpm_t * 0.10;
                acute_angle_bonus *= (0.5
                    + 0.5 * (1.0 - acute_penalty)
                    + acute_rep_buff)
                    .max(0.0);
            }
        }

        // ── Velocity change bonus ──────────────────────────────────
        if prev_vel.max(curr_vel).not_eq(0.0) {
            prev_vel = (osu_last_obj.lazy_jump_dist + osu_last_last_obj.travel_dist)
                / osu_last_obj.strain_time;
            curr_vel =
                (osu_curr_obj.lazy_jump_dist + osu_last_obj.travel_dist) / osu_curr_obj.strain_time;

            let dist_ratio_base =
                (FRAC_PI_2 * (prev_vel - curr_vel).abs() / prev_vel.max(curr_vel)).sin();
            let dist_ratio = dist_ratio_base.powf(2.0);

            let overlap_vel_buff = (125.0 / osu_curr_obj.strain_time.min(osu_last_obj.strain_time))
                .min((prev_vel - curr_vel).abs());

            vel_change_bonus = overlap_vel_buff * dist_ratio;

            let bonus_base = (osu_curr_obj.strain_time).min(osu_last_obj.strain_time)
                / (osu_curr_obj.strain_time).max(osu_last_obj.strain_time);
            vel_change_bonus *= bonus_base.powf(2.0);
        }

        // ── Slider bonus with slow-slider nerf ──────────────────────
        if osu_last_obj.base.is_slider() {
            let travel_vel = osu_last_obj.travel_dist / osu_last_obj.travel_time;
            slider_bonus = travel_vel;

            // RX: taper slow sliders. Below the velocity floor, the
            // bonus scales from 1.0 at the floor down to 0.55 at vel=0.
            if travel_vel < Self::SLOW_SLIDER_VEL_FLOOR {
                let ratio = (travel_vel / Self::SLOW_SLIDER_VEL_FLOOR).clamp(0.0, 1.0);
                let slow_slider_taper = 0.55 + 0.45 * ratio;
                slider_bonus *= slow_slider_taper;
            }
        }

        // ── Combine angle + velocity + slider ──────────────────────
        aim_strain += (acute_angle_bonus * Self::ACUTE_ANGLE_MULTIPLIER).max(
            wide_angle_bonus * Self::WIDE_ANGLE_MULTIPLIER
                + vel_change_bonus * Self::VELOCITY_CHANGE_MULTIPLIER,
        );

        if with_slider_travel_dist {
            aim_strain += slider_bonus * Self::SLIDER_MULTIPLIER;
        }

        // ── Cross-screen constant-distance nerf ─────────────────────
        //
        // If |curr_dist - prev_dist| / max(dist) is small AND neither
        // distance is genuinely edge-to-edge, nerf the strain. This
        // catches patterns that move the cursor a lot but don't vary
        // spacing — repetitive cross-screen slop that's easy on RX.
        //
        // Exempt above 350 BPM 1/2 effective (strain_time < 85.7 ms).
        if osu_curr_obj.strain_time >= Self::CONSTANT_DIST_BPM_STRAIN_TIME {
            let curr_d = osu_curr_obj.lazy_jump_dist;
            let prev_d = osu_last_obj.lazy_jump_dist;
            let max_d = curr_d.max(prev_d);
            let min_d = curr_d.min(prev_d);

            // Only applies if there's meaningful distance at all
            // (stacks and tiny movements are handled elsewhere).
            if max_d > 80.0 {
                let change_ratio = if max_d > 0.0 {
                    (max_d - min_d) / max_d
                } else {
                    1.0
                };

                let is_edge_to_edge = max_d >= Self::EDGE_TO_EDGE_THRESHOLD;

                if !is_edge_to_edge && change_ratio < Self::CONSTANT_DIST_RATIO {
                    // Penalty severity: stronger when distances are closer
                    // (ratio → 0 = identical = max penalty) and when the
                    // distance itself is smaller (further from edge-to-edge).
                    let ratio_factor = 1.0 - (change_ratio / Self::CONSTANT_DIST_RATIO);
                    let dist_factor = 1.0
                        - ((max_d - 80.0) / (Self::EDGE_TO_EDGE_THRESHOLD - 80.0))
                            .clamp(0.0, 1.0);
                    let severity = ratio_factor * dist_factor;

                    // Max 15% cut on the aim strain contribution from
                    // this object.
                    let nerf = 1.0 - 0.15 * severity;
                    aim_strain *= nerf;
                }
            }
        }

        // ── Extreme flow aim nerf ──────────────────────────────────
        //
        // Flow aim = smooth sweeping motion through gentle curves, all
        // at similar wide angles. On RX this is trivial (no tap timing
        // to hit, just move cursor along a path). Detected via the
        // windowed angle stats:
        //   - mean angle above ~115° (wide sweeping, not sharp turns)
        //   - stddev below ~17° (consistent curve direction)
        //
        // When both hold, apply an extreme multiplicative nerf.
        // Severity scales with how tight the flow signature is:
        //   - stddev → 0 (perfectly smooth curve) + mean → π  = full −50%
        //   - stddev at threshold or mean at threshold = no nerf
        //
        // Exempt above 410 BPM 1/4 effective — at that speed, flow is
        // actually mechanically hard to execute on RX.
        if osu_curr_obj.strain_time >= Self::FLOW_CONSTANT_DIST_BPM_STRAIN_TIME {
            let (flow_mean, flow_stddev, flow_n) =
                windowed_angle_stats(osu_curr_obj, diff_objects, Self::ANGLE_WINDOW);

            if flow_n >= 4 {
                let mean_ok = flow_mean >= Self::FLOW_MEAN_ANGLE_THRESHOLD;
                let stddev_ok = flow_stddev <= Self::FLOW_STDDEV_THRESHOLD;

                if mean_ok && stddev_ok {
                    // Stddev severity: 1.0 when stddev=0, 0.0 at threshold.
                    // Squared so the curve bites harder on very tight flow.
                    let stddev_severity =
                        (1.0 - (flow_stddev / Self::FLOW_STDDEV_THRESHOLD)).powi(2);

                    // Mean severity: 0.0 at the threshold (~115°), 1.0 at
                    // π (~180°, perfectly straight-through sweep).
                    let mean_range = PI - Self::FLOW_MEAN_ANGLE_THRESHOLD;
                    let mean_severity = ((flow_mean - Self::FLOW_MEAN_ANGLE_THRESHOLD)
                        / mean_range)
                        .clamp(0.0, 1.0);

                    let combined_severity = stddev_severity * mean_severity;
                    let flow_nerf = 1.0 - Self::FLOW_MAX_NERF * combined_severity;
                    aim_strain *= flow_nerf;
                }
            }
        }

        aim_strain
    }

    fn calc_wide_angle_bonus(angle: f64) -> f64 {
        (3.0 / 4.0 * ((5.0 / 6.0 * PI).min(angle.max(PI / 6.0)) - PI / 6.0))
            .sin()
            .powf(2.0)
    }

    fn calc_acute_angle_bonus(angle: f64) -> f64 {
        1.0 - Self::calc_wide_angle_bonus(angle)
    }
}
