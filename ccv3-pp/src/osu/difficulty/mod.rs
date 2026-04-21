use std::{cmp, pin::Pin};

use rosu_map::section::general::GameMode;
use skills::{
    flashlight::Flashlight,
    strain::{DifficultyValue, OsuStrainSkill, UsedOsuStrainSkills},
};

use crate::{
    any::difficulty::{skills::Skill, Difficulty},
    model::{beatmap::BeatmapAttributes, mode::ConvertError, mods::GameMods},
    osu::{
        convert::convert_objects,
        difficulty::{object::OsuDifficultyObject, scaling_factor::ScalingFactor},
        object::OsuObject,
        performance::PERFORMANCE_BASE_MULTIPLIER,
    },
    Beatmap,
};

use self::skills::OsuSkills;

use super::attributes::OsuDifficultyAttributes;

pub mod gradual;
mod object;
pub mod scaling_factor;
pub mod skills;
pub mod tap_bpm;
pub mod speed_precal;

const DIFFICULTY_MULTIPLIER: f64 = 0.0675;

const HD_FADE_IN_DURATION_MULTIPLIER: f64 = 0.4;
const HD_FADE_OUT_DURATION_MULTIPLIER: f64 = 0.3;

pub fn difficulty(
    difficulty: &Difficulty,
    map: &Beatmap,
) -> Result<OsuDifficultyAttributes, ConvertError> {
    let map = map.convert_ref(GameMode::Osu, difficulty.get_mods())?;

    let DifficultyValues {
        skills:
            OsuSkills {
                aim,
                aim_no_sliders,
                speed,
                flashlight,
            },
        mut attrs,
        speed_object_data,
    } = DifficultyValues::calculate(difficulty, &map);

    // CC V3: Before consuming skills with .difficulty_value(), extract:
    //   1. speed per-object strains for dominant_tap_bpm
    //   2. aim + speed section peaks for per-minute local SR (relax marathon)
    // All extractions are non-consuming (clone internally).
    let speed_object_strains: Vec<f64> = speed.object_strains().to_vec();
    let aim_peaks: Vec<f64> = aim.clone_strain_peaks();
    let speed_peaks: Vec<f64> = speed.clone_strain_peaks();

    let aim_difficulty_value = aim.difficulty_value();
    let aim_no_sliders_difficulty_value = aim_no_sliders.difficulty_value();
    let speed_relevant_note_count = speed.relevant_note_count();
    let speed_difficulty_value = speed.difficulty_value();
    let flashlight_difficulty_value = flashlight.difficulty_value();

    let mods = difficulty.get_mods();

    DifficultyValues::eval(
        &mut attrs,
        mods,
        &aim_difficulty_value,
        &aim_no_sliders_difficulty_value,
        &speed_difficulty_value,
        speed_relevant_note_count,
        flashlight_difficulty_value,
    );

    // CC V3: Compute dominant_tap_bpm from owned data.
    attrs.dominant_tap_bpm = tap_bpm::dominant_tap_bpm_from_owned(
        &speed_object_strains,
        &speed_object_data,
        0.10,
    );

    // CC V3: Precompute speed rework multipliers (vanilla + autopilot).
    let (v_mult, ap_mult) = speed_precal::precompute_speed_rework_from_owned(
        &speed_object_data,
        attrs.dominant_tap_bpm,
    );
    attrs.speed_rework_mult_vanilla = v_mult;
    attrs.speed_rework_mult_autopilot = ap_mult;

    // CC V3: Bin aim+speed section peaks into per-minute local SR for the
    // relax marathon decay. Stored on attrs so performance/mod.rs can
    // read it directly without needing skill access.
    attrs.local_sr_per_minute = crate::osu::performance::relax_marathon::local_sr_per_minute(
        &aim_peaks,
        &speed_peaks,
    );

    // CC V3: Compute average jump distance and median delta time across
    // all diff objects. Both are rate-adjusted (clock_rate already applied
    // upstream). Used by the distance inflation nerf in compute_aim_value.
    if !speed_object_data.is_empty() {
        // Average pairwise spacing (skip the first object which has no prev).
        let mut dist_sum = 0.0;
        let mut dist_count = 0u32;
        for pair in speed_object_data.windows(2) {
            let dx = (pair[1].pos_x - pair[0].pos_x) as f64;
            let dy = (pair[1].pos_y - pair[0].pos_y) as f64;
            dist_sum += (dx * dx + dy * dy).sqrt();
            dist_count += 1;
        }
        attrs.avg_jump_dist = if dist_count > 0 {
            dist_sum / dist_count as f64
        } else {
            0.0
        };

        // Median delta_time. Collect positive deltas, sort, pick middle.
        let mut deltas: Vec<f64> = speed_object_data
            .iter()
            .map(|o| o.delta_time)
            .filter(|d| *d > 0.0)
            .collect();
        if !deltas.is_empty() {
            deltas.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let m = deltas.len() / 2;
            attrs.median_delta_time = if deltas.len() % 2 == 1 {
                deltas[m]
            } else {
                (deltas[m - 1] + deltas[m]) / 2.0
            };
        }

        // CC V3 (Relax): hardness-per-4-notes for the RX miss system.
        // Non-strain-based — pure timing. Each 4-note chunk's hardness
        // is the sum of (1.0 / delta_time) across its four objects, so
        // faster sections register as harder. The performance pass uses
        // this to distribute the total n100/n50 drops across sections
        // proportional to their hardness, then pair-up to 8-note windows
        // and rank the pairs by estimated "accuracy drop weight".
        let mut hardness_chunks: Vec<f64> = Vec::new();
        let mut chunk_sum = 0.0;
        let mut chunk_count: u32 = 0;
        for obj in &speed_object_data {
            if obj.delta_time > 0.0 {
                chunk_sum += 1.0 / obj.delta_time;
                chunk_count += 1;
                if chunk_count == 4 {
                    hardness_chunks.push(chunk_sum);
                    chunk_sum = 0.0;
                    chunk_count = 0;
                }
            }
        }
        // If a partial chunk remains (< 4 notes), push it anyway so the
        // tail of the map isn't silently dropped.
        if chunk_count > 0 {
            hardness_chunks.push(chunk_sum);
        }
        attrs.rx_hardness_per_4notes = hardness_chunks;
    }

    Ok(attrs)
}

pub struct OsuDifficultySetup {
    scaling_factor: ScalingFactor,
    map_attrs: BeatmapAttributes,
    attrs: OsuDifficultyAttributes,
    time_preempt: f64,
}

impl OsuDifficultySetup {
    pub fn new(difficulty: &Difficulty, map: &Beatmap) -> Self {
        let clock_rate = difficulty.get_clock_rate();
        let map_attrs = map.attributes().difficulty(difficulty).build();
        let scaling_factor = ScalingFactor::new(map_attrs.cs);

        let attrs = OsuDifficultyAttributes {
            ar: map_attrs.ar,
            hp: map_attrs.hp,
            od: map_attrs.od,
            cs: map_attrs.cs,
            ..Default::default()
        };

        let time_preempt = f64::from((map_attrs.hit_windows.ar * clock_rate) as f32);

        Self {
            scaling_factor,
            map_attrs,
            attrs,
            time_preempt,
        }
    }
}

pub struct DifficultyValues {
    pub skills: OsuSkills,
    pub attrs: OsuDifficultyAttributes,
    /// CC V3: Owned per-object data extracted before diff_objects drops,
    /// used as input to the speed rework precompute pipeline.
    pub speed_object_data: Vec<tap_bpm::SpeedObjectData>,
}

impl DifficultyValues {
    pub fn calculate(difficulty: &Difficulty, map: &Beatmap) -> Self {
        let mods = difficulty.get_mods();
        let take = difficulty.get_passed_objects();

        let OsuDifficultySetup {
            scaling_factor,
            map_attrs,
            mut attrs,
            time_preempt,
        } = OsuDifficultySetup::new(difficulty, map);

        let mut osu_objects = convert_objects(
            map,
            &scaling_factor,
            mods.reflection(),
            time_preempt,
            take,
            &mut attrs,
        );

        let osu_object_iter = osu_objects.iter_mut().map(Pin::new);

        let diff_objects =
            Self::create_difficulty_objects(difficulty, &scaling_factor, osu_object_iter);

        let mut skills = OsuSkills::new(mods, &scaling_factor, &map_attrs, time_preempt);

        {
            let mut aim = Skill::new(&mut skills.aim, &diff_objects);
            let mut aim_no_sliders = Skill::new(&mut skills.aim_no_sliders, &diff_objects);
            let mut speed = Skill::new(&mut skills.speed, &diff_objects);
            let mut flashlight = Skill::new(&mut skills.flashlight, &diff_objects);

            // The first hit object has no difficulty object
            let take_diff_objects = cmp::min(map.hit_objects.len(), take).saturating_sub(1);

            for hit_object in diff_objects.iter().take(take_diff_objects) {
                aim.process(hit_object);
                aim_no_sliders.process(hit_object);
                speed.process(hit_object);
                flashlight.process(hit_object);
            }
        }

        // CC V3: Extract owned per-object data before diff_objects drops.
        let speed_object_data: Vec<tap_bpm::SpeedObjectData> = diff_objects
            .iter()
            .map(|obj| tap_bpm::SpeedObjectData {
                delta_time: obj.delta_time,
                pos_x: obj.base.pos.x,
                pos_y: obj.base.pos.y,
            })
            .collect();

        Self { skills, attrs, speed_object_data }
    }

    /// Process the difficulty values and store the results in `attrs`.
    pub fn eval(
        attrs: &mut OsuDifficultyAttributes,
        mods: &GameMods,
        aim: &UsedOsuStrainSkills<DifficultyValue>,
        aim_no_sliders: &UsedOsuStrainSkills<DifficultyValue>,
        speed: &UsedOsuStrainSkills<DifficultyValue>,
        speed_relevant_note_count: f64,
        flashlight_difficulty_value: f64,
    ) {
        let mut aim_rating = aim.difficulty_value().sqrt() * DIFFICULTY_MULTIPLIER;
        let aim_rating_no_sliders =
            aim_no_sliders.difficulty_value().sqrt() * DIFFICULTY_MULTIPLIER;
        let mut speed_rating = speed.difficulty_value().sqrt() * DIFFICULTY_MULTIPLIER;
        let mut flashlight_rating = flashlight_difficulty_value.sqrt() * DIFFICULTY_MULTIPLIER;

        let slider_factor = if aim_rating > 0.0 {
            aim_rating_no_sliders / aim_rating
        } else {
            1.0
        };

        let aim_difficult_strain_count = aim.count_difficult_strains();
        let speed_difficult_strain_count = speed.count_difficult_strains();

        if mods.td() {
            aim_rating = aim_rating.powf(0.6);
            flashlight_rating = flashlight_rating.powf(0.7);
        }

        if mods.rx() {
            aim_rating = 1.0;
            speed_rating = 0.0;
            flashlight_rating *= 0.7; 
        }

        if mods.ap() {
            aim_rating = 0.0;
            speed_rating = 1.0;
            flashlight_rating *= 0.55; 
        }

        // Buffs 4 mod (Not EZ)
        if mods.ap() && mods.dt() && mods.hd() && mods.hr() && mods.fl() && !mods.ez() {
            aim_rating = 0.0;
            speed_rating = 1.0;
            flashlight_rating *= 0.72;
        }

        let base_aim_performance = OsuStrainSkill::difficulty_to_performance(aim_rating);
        let base_speed_performance = OsuStrainSkill::difficulty_to_performance(speed_rating);

        let base_flashlight_performance = if mods.fl() {
            Flashlight::difficulty_to_performance(flashlight_rating)
        } else {
            0.0
        };

        let base_performance = ((base_aim_performance).powf(1.1)
            + (base_speed_performance).powf(1.1)
            + (base_flashlight_performance).powf(1.1))
        .powf(1.0 / 1.1);

        let star_rating = if base_performance > 0.00001 {
            PERFORMANCE_BASE_MULTIPLIER.cbrt()
                * 0.027
                * ((100_000.0 / 2.0_f64.powf(1.0 / 1.1) * base_performance).cbrt() + 4.0)
        } else {
            0.0
        };

        attrs.aim = aim_rating;
        attrs.speed = speed_rating;
        attrs.flashlight = flashlight_rating;
        attrs.slider_factor = slider_factor;
        attrs.aim_difficult_strain_count = aim_difficult_strain_count;
        attrs.speed_difficult_strain_count = speed_difficult_strain_count;
        attrs.stars = star_rating;
        attrs.speed_note_count = speed_relevant_note_count;
    }

    pub fn create_difficulty_objects<'a>(
        difficulty: &Difficulty,
        scaling_factor: &ScalingFactor,
        osu_objects: impl ExactSizeIterator<Item = Pin<&'a mut OsuObject>>,
    ) -> Vec<OsuDifficultyObject<'a>> {
        let take = difficulty.get_passed_objects();
        let clock_rate = difficulty.get_clock_rate();

        let mut osu_objects_iter = osu_objects
            .map(|h| OsuDifficultyObject::compute_slider_cursor_pos(h, scaling_factor.radius))
            .map(Pin::into_ref);

        let Some(mut last) = osu_objects_iter.next().filter(|_| take > 0) else {
            return Vec::new();
        };

        let mut last_last = None;

        osu_objects_iter
            .enumerate()
            .map(|(idx, h)| {
                let diff_object = OsuDifficultyObject::new(
                    h.get_ref(),
                    last.get_ref(),
                    last_last.as_deref(),
                    clock_rate,
                    idx,
                    scaling_factor,
                );

                last_last = Some(last);
                last = h;

                diff_object
            })
            .collect()
    }
}
