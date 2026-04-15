// Extracts the dominant 1/4 tapping BPM from the difficulty pipeline.
// Called once during difficulty calculation; result is stored on
// OsuDifficultyAttributes::dominant_tap_bpm.

/// Minimal owned data per difficulty object, extracted before the
/// borrow on OsuObject drops. Parallel to speed `object_strains`.
#[derive(Clone, Debug)]
pub struct SpeedObjectData {
    pub delta_time: f64,
    pub pos_x: f32,
    pub pos_y: f32,
}

/// Compute the dominant 1/4 tapping BPM from owned data.
///
/// Algorithm:
///   1. Find the strain threshold that marks the top `top_pct` of
///      per-object speed strains.
///   2. Collect `delta_time` for every object whose strain >= threshold.
///   3. Return the median delta converted to 1/4 BPM (15000 / dt).
///
/// This gives us the BPM the player is *actually tapping at* during
/// the hardest sections, which is more meaningful than the map's
/// stated BPM (a 180 BPM map can have 1/4 bursts at 360 BPM effective).
pub fn dominant_tap_bpm_from_owned(
    object_strains: &[f64],
    objects: &[SpeedObjectData],
    top_pct: f64,
) -> f64 {
    if object_strains.is_empty() || objects.is_empty() {
        return 0.0;
    }

    // 1) strain threshold
    let mut sorted: Vec<f64> = object_strains.iter().copied().collect();
    sorted.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));

    let cutoff = ((sorted.len() as f64 * top_pct).ceil() as usize)
        .max(1)
        .min(sorted.len());
    let threshold = sorted[cutoff - 1];

    if threshold <= 0.0 {
        return 0.0;
    }

    // 2) collect qualifying deltas
    let len = object_strains.len().min(objects.len());
    let mut deltas: Vec<f64> = Vec::with_capacity(cutoff);

    for i in 0..len {
        if object_strains[i] >= threshold && objects[i].delta_time > 0.0 {
            deltas.push(objects[i].delta_time);
        }
    }

    if deltas.is_empty() {
        return 0.0;
    }

    // 3) median -> BPM
    deltas.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let median = if deltas.len() % 2 == 1 {
        deltas[deltas.len() / 2]
    } else {
        let m = deltas.len() / 2;
        (deltas[m - 1] + deltas[m]) / 2.0
    };

    if median > 0.0 { 15_000.0 / median } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_bpm_extraction() {
        // Simulate 200 objects at 180 BPM 1/4 (delta = 83.33ms)
        let n = 200;
        let dt = 15_000.0 / 180.0;
        let objects: Vec<SpeedObjectData> = (0..n)
            .map(|_| SpeedObjectData {
                delta_time: dt,
                pos_x: 256.0,
                pos_y: 192.0,
            })
            .collect();
        let mut strains: Vec<f64> = vec![1.0; n];
        for i in (n - 20)..n {
            strains[i] = 5.0;
        }

        let bpm = dominant_tap_bpm_from_owned(&strains, &objects, 0.10);
        assert!((bpm - 180.0).abs() < 1.0, "Expected ~180 BPM, got {bpm}");
    }

    #[test]
    fn test_mixed_bpm() {
        // Half at 160 BPM, half at 320 BPM. Top 10% strains only on 320 BPM section.
        let dt_slow = 15_000.0 / 160.0;
        let dt_fast = 15_000.0 / 320.0;
        let mut objects = Vec::new();
        let mut strains = Vec::new();

        for _ in 0..100 {
            objects.push(SpeedObjectData { delta_time: dt_slow, pos_x: 0.0, pos_y: 0.0 });
            strains.push(1.0);
        }
        for _ in 0..100 {
            objects.push(SpeedObjectData { delta_time: dt_fast, pos_x: 0.0, pos_y: 0.0 });
            strains.push(5.0);
        }

        let bpm = dominant_tap_bpm_from_owned(&strains, &objects, 0.10);
        assert!((bpm - 320.0).abs() < 5.0, "Expected ~320 BPM, got {bpm}");
    }
}
