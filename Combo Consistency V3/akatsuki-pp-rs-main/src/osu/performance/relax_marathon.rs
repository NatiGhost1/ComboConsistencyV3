#[derive(Clone, Copy)]
pub struct MarathonDecayParams {
    pub tau: f64, // tolerance in stars, e.g. 0.50
    pub b: f64,   // e.g. 0.02
    pub q: f64,   // e.g. 1.35
    pub double_at: u32, // minutes, e.g. 5
}

pub fn decay_divisor(r: u32, p: MarathonDecayParams) -> f64 {
    let rf = r as f64;
    let base = 1.0 + p.b * rf.powf(p.q);
    if r >= p.double_at {
        2.0 * base
    } else {
        base
    } 
}

const DIFFICULTY_MULTIPLIER: f64 = 0.0675;
const PEAK_SECTION_LEN_MS: f64 = 400.0;
const MINUTE_MS: f64 = 60_000.0;

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

// Replicates OsuStrainSkill::difficulty_value logic but on a slice of peaks.
fn difficulty_value_from_peaks(peaks: &[f64]) -> f64 {
    let mut v: Vec<f64> = peaks.iter().copied().filter(|x| *x > 0.0).collect();
    if v.is_empty() { return 0.0; }
    
    v.sort_by(|a, b| b.partial_cmp(a).unwrap());

    // same intent as reduced top peaks (see OsuStrainSkill constants)
    let reduced_section_count = 10usize;
    let reduced_baseline = 0.75;
    let decay_weight = 0.9;

    let take = reduced_section_count.min(v.len());
    for i in 0..take {
        // 1. Calc porgress thru top sections
        let t = i as f64 / reduced_section_count as f64;
        // 2. Apply the logarithmic reduction (OsuStrainSkill style)
        let scale = lerp(1.0, reduced_baseline, t).log10().abs();
        v[i] *= lerp(reduced_baseline, 1.0, scale.min(1.0));
    }

    v.sort_by(|a, b| b.partial_cmp(a).unwrap());

    let mut difficulty = 0.0;
    let mut w = 1.0;
    for s in v {
        difficulty += s * w;
        w *= decay_weight;
    }
    difficulty
}

fn star_from_aim_speed(aim_peaks: &[f64], speed_peaks: &[f64]) -> f64 {
    // difficulty values (weighted strain sums)
    let aim_dv = difficulty_value_from_peaks(aim_peaks);
    let speed_dv = difficulty_value_from_peaks(speed_peaks);

// convert to "ratings"
let aim_rating = aim_dv.sqrt() * DIFFICULTY_MULTIPLIER;
let speed_rating = speed_dv.sqrt() * DIFFICULTY_MULTIPLIER;

// convert to "performance"
let base_aim_perf = crate::difficulty::skills::strain::OsuStrainSkill::difficulty_to_performance(aim_rating);
let base_speed_perf = crate::difficulty::skills::strain::OsuStrainSkill::difficulty_to_performance(speed_rating);

    // flashlight ignored for nomod SR (consistent with difficulty eval)
    let base_perf = (base_aim_perf.powf(1.1) + base_speed_perf.powf(1.1)).powf(1.0 / 1.1);

    if base_perf <= 0.00001 {
        return 0.0;
    }

    // same star mapping used in difficulty::eval
    crate::PERFORMANCE_BASE_MULTIPLIER.cbrt()
        * 0.027
        * ((100_000.0 / 2.0_f64.powf(1.0 / 1.1) *base_perf).cbrt() + 4.0)
}

fn local_sr_per_minute(strains_aim: &[f64], strains_speed: &[f64]) -> Vec<f64> {
    let peaks_per_min = (MINUTE_MS / PEAK_SECTION_LEN_MS).round() as usize; // 150
    let n_minutes = strains_aim.len().div_ceil(peaks_per_min);



    

    let mut out = Vec::with_capacity(n_minutes);
    for k in 0..n_minutes {
        let start = k * peaks_per_min;
        let end = ((k + 1) * peaks_per_min).min(strains_aim.len());
        let aim_slice = &strains_aim[start..end];
        let speed_slice = &strains_speed[start..end];

        out.push(star_from_aim_speed(aim_slice, speed_slice));
    }

    out
}

pub fn relax_marathon_multiplier(local_sr: &[f64], params: MarathonDecayParams) -> f64 {
    if local_sr.len() < 2 {
        return 1.0;
    }

    let mut r: u32 = 0;
    let mut weighted = 0.0;
    let mut total = 0.0;

    for (k, &sr) in local_sr.iter().enumerate() {
        if k > 0 && (sr - local_sr[k - 1]).abs() <= params.tau {
            r += 1;
        } else {
            r = 0;
        }

        let lambda = 1.0 / decay_divisor(r, params);

        // Weight by SR so "dead minutes" don't dominate.
        weighted += sr * lambda;
        total += sr;
    }

    if total > 0.0 { (weighted / total).clamp(0.0, 1.0) } else { 1.0 }
}
