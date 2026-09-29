//! How well the pre-marks match what the user decided, and which `Config`
//! thresholds would have matched better. Only frames the user reviewed
//! count; groups are the user's (with splits and joins), so only the
//! judging thresholds are tuned, not grouping.

use std::collections::BTreeMap;

use crate::ShotAnalysis;
use crate::cull::{self, Config, Mark, Reason, Verdict};
use crate::session::Decision;

/// One shoot's analysis with the user's groups and decisions.
pub struct Shoot<'a> {
    pub shots: &'a [ShotAnalysis],
    pub groups: Vec<Vec<usize>>,
    pub decisions: &'a BTreeMap<String, Decision>,
}

/// Agreement for one mark: predicted = pre-mark, actual = user's decision.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Agreement {
    pub both: usize,
    /// Pre-marked, but the user decided otherwise.
    pub only_pre: usize,
    /// The user's mark the pre-marks missed.
    pub only_user: usize,
}

impl Agreement {
    pub fn precision(&self) -> Option<f32> {
        let n = self.both + self.only_pre;
        (n > 0).then(|| self.both as f32 / n as f32)
    }
    pub fn recall(&self) -> Option<f32> {
        let n = self.both + self.only_user;
        (n > 0).then(|| self.both as f32 / n as f32)
    }
    pub fn errors(&self) -> usize {
        self.only_pre + self.only_user
    }
}

/// What the user did with the frames a reason was given for.
#[derive(Debug, Default, Clone, Copy)]
pub struct ReasonStat {
    pub flagged: usize,
    pub rejected: usize,
    pub picked: usize,
}

/// A frame the user rejected that the pre-marks didn't, with the numbers
/// that would have had to catch it.
#[derive(Debug, Clone)]
pub struct Missed {
    /// Index into the shoots given to `calibrate`.
    pub shoot: usize,
    pub file: String,
    pub reasons: Vec<Reason>,
    /// eyeBlink (right, left) of the face with the most closed eyes.
    pub blink: Option<[f32; 2]>,
    /// Eye sharpness (frame sharpness without a usable face) relative to the group's best.
    pub rel_sharp: f32,
}

#[derive(Debug, Clone)]
pub struct Suggestion {
    pub param: &'static str,
    pub current: f32,
    pub suggested: f32,
}

#[derive(Debug, Default)]
pub struct Report {
    pub decided: usize,
    pub reject: Agreement,
    pub pick: Agreement,
    pub reasons: BTreeMap<String, ReasonStat>,
    pub missed: Vec<Missed>,
    /// Changes that together lower the reject errors, and the errors after.
    pub suggestions: Vec<Suggestion>,
    pub reject_errors_after: usize,
}

type Knob = (&'static str, fn(&mut Config) -> &mut f32, f32, f32, f32);

/// Thresholds that decide rejects, with the range (from, to, step) to try.
const KNOBS: &[Knob] = &[
    ("blink_closed", |c| &mut c.blink_closed, 0.30, 0.70, 0.025),
    ("blink_both", |c| &mut c.blink_both, 0.10, 0.50, 0.025),
    ("soft_rel", |c| &mut c.soft_rel, 0.25, 0.80, 0.025),
    ("min_presence", |c| &mut c.min_presence, 0.20, 0.80, 0.05),
    ("min_eye_px", |c| &mut c.min_eye_px, 15.0, 80.0, 5.0),
];

fn verdicts(shoots: &[Shoot], cfg: &Config) -> Vec<Vec<Verdict>> {
    shoots.iter().map(|s| cull::judge(s.shots, &s.groups, cfg)).collect()
}

/// (shoot, frame, user's mark) for every decided frame.
fn decided<'a>(shoots: &'a [Shoot]) -> impl Iterator<Item = (usize, usize, Mark)> + 'a {
    shoots.iter().enumerate().flat_map(|(k, s)| {
        s.shots.iter().enumerate().filter_map(move |(i, shot)| s.decisions.get(&shot.file).map(|d| (k, i, d.mark)))
    })
}

fn agreement(shoots: &[Shoot], verdicts: &[Vec<Verdict>], mark: Mark) -> Agreement {
    let mut a = Agreement::default();
    for (k, i, user) in decided(shoots) {
        match (verdicts[k][i].mark == mark, user == mark) {
            (true, true) => a.both += 1,
            (true, false) => a.only_pre += 1,
            (false, true) => a.only_user += 1,
            (false, false) => {}
        }
    }
    a
}

pub fn calibrate(shoots: &[Shoot], cfg: &Config) -> Report {
    let v = verdicts(shoots, cfg);
    let mut r = Report {
        decided: decided(shoots).count(),
        reject: agreement(shoots, &v, Mark::Reject),
        pick: agreement(shoots, &v, Mark::Pick),
        ..Report::default()
    };
    for (k, i, user) in decided(shoots) {
        let verdict = &v[k][i];
        for reason in &verdict.reasons {
            let e = r.reasons.entry(reason.slug()).or_default();
            e.flagged += 1;
            e.rejected += (user == Mark::Reject) as usize;
            e.picked += (user == Mark::Pick) as usize;
        }
        if user == Mark::Reject && verdict.mark != Mark::Reject {
            let shot = &shoots[k].shots[i];
            let blink = shot.faces.iter().map(|f| f.blink).max_by(|a, b| {
                (a[0].min(a[1])).total_cmp(&b[0].min(b[1]))
            });
            r.missed.push(Missed {
                shoot: k,
                file: shot.file.clone(),
                reasons: verdict.reasons.clone(),
                blink,
                rel_sharp: verdict.rel_sharp,
            });
        }
    }

    // Coordinate descent on reject errors. A threshold only moves when that
    // fixes at least one more frame, and then as little as possible.
    let errors = |c: &Config| agreement(shoots, &verdicts(shoots, c), Mark::Reject).errors();
    let mut best = cfg.clone();
    let mut best_err = errors(&best);
    for _ in 0..3 {
        let mut moved = false;
        for &(_, field, from, to, step) in KNOBS {
            let now = *field(&mut best.clone());
            let steps = ((to - from) / step).round() as usize;
            let tried = (0..=steps).map(|n| {
                let mut c = best.clone();
                *field(&mut c) = from + n as f32 * step;
                (errors(&c), (from + n as f32 * step - now).abs(), c)
            });
            if let Some((e, _, c)) = tried.min_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)))
                && e < best_err
            {
                (best, best_err, moved) = (c, e, true);
            }
        }
        if !moved {
            break;
        }
    }
    r.reject_errors_after = best_err;
    for &(param, field, ..) in KNOBS {
        let (now, then) = (*field(&mut cfg.clone()), *field(&mut best.clone()));
        if (now - then).abs() > 1e-6 {
            r.suggestions.push(Suggestion { param, current: now, suggested: then });
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FaceMetrics;

    fn shot(t: f64, blink: f32) -> ShotAnalysis {
        let face = FaceMetrics {
            bbox: [2000.0, 1000.0, 800.0, 800.0],
            det_score: 0.9,
            presence: 1.0,
            eye_px: [200.0; 2],
            eye_sharp: [0.4, 0.4],
            face_sharp: 0.2,
            blink: [blink; 2],
            ear: [0.3; 2],
            smile: 0.0,
            face_luma: 50.0,
            face_clip: 0.0,
            cut: false,
            eye_boxes: [[0.0; 4]; 2],
            eyes_box: [0.0; 4],
            embed: vec![],
            embed_norm: 0.0,
        };
        ShotAnalysis {
            file: format!("{t}.CR3"),
            size: 0,
            mtime: 0,
            time: t,
            width: 6000,
            height: 4000,
            faces: vec![face],
            frame_sharp: 0.3,
            sig: vec![100; 256],
            error: None,
            embedded: false,
        }
    }

    #[test]
    fn suggests_lower_blink_threshold_for_missed_half_blinks() {
        // Half-closed eyes (0.40) the user rejects, which the default 0.45 misses.
        let shots = vec![shot(0.0, 0.05), shot(1.0, 0.40), shot(2.0, 0.42), shot(3.0, 0.10), shot(4.0, 0.60)];
        let d = |m| Decision { mark: m };
        let decisions: BTreeMap<_, _> = [
            ("0.CR3", d(Mark::Pick)),
            ("1.CR3", d(Mark::Reject)),
            ("2.CR3", d(Mark::Reject)),
            ("3.CR3", d(Mark::None)),
            ("4.CR3", d(Mark::Reject)),
        ]
        .into_iter()
        .map(|(f, d)| (f.to_string(), d))
        .collect();
        let shoots = [Shoot { shots: &shots, groups: vec![(0..5).collect()], decisions: &decisions }];
        let r = calibrate(&shoots, &Config::default());
        assert_eq!(r.decided, 5);
        assert_eq!(r.reject, Agreement { both: 1, only_pre: 0, only_user: 2 });
        assert_eq!(r.reasons["eyes-closed"].rejected, 1);
        assert_eq!(r.missed.len(), 2);
        assert_eq!(r.reject_errors_after, 0);
        let s = r.suggestions.iter().find(|s| s.param == "blink_closed").expect("blink_closed suggestion");
        // The smallest move that catches both: 0.45 → 0.40.
        assert!((s.suggested - 0.40).abs() < 1e-4, "{s:?}");
    }
}
