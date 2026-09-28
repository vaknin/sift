//! Grouping similar frames and pre-marking them: sure rejects, best picks,
//! each with its reasons. Everything is relative to the group, because
//! absolute sharpness depends on the scene and lighting, not only on focus.

use serde::{Deserialize, Serialize};

use crate::{FaceMetrics, ShotAnalysis};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// A gap this long (seconds) always starts a new group.
    pub scene_gap: f64,
    /// Distance to the previous frame that starts a new group: grey-signature
    /// distance (0–1) plus `face_weight` × how far the main face moved.
    pub split_dist: f32,
    pub face_weight: f32,
    /// Eyes closed = one eye's eyeBlink at least `blink_closed` and the other
    /// at least `blink_both`. Open eyes stay under ~0.3, closed land at
    /// 0.45–0.75; one open eye with a turned head reads as (0.46, 0.03).
    pub blink_closed: f32,
    pub blink_both: f32,
    /// Face mesh presence below this = face not usable (profile, back of head, hands).
    pub min_presence: f32,
    /// Eye narrower than this (full-res px) is too small to judge sharpness.
    pub min_eye_px: f32,
    /// Eyes sharper than this fraction of the group's best are fine.
    pub soft_rel: f32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            scene_gap: 45.0,
            split_dist: 0.17,
            face_weight: 0.5,
            blink_closed: 0.45,
            blink_both: 0.3,
            min_presence: 0.5,
            min_eye_px: 40.0,
            soft_rel: 0.5,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mark {
    Pick,
    Reject,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reason {
    EyesClosed,
    SoftEyes,
    /// No face at all (e.g. shot from behind). Informational.
    NoFace,
    /// A face, but turned away or covered. Informational.
    FaceHidden,
    SmallFace,
    FaceCut,
    /// Sharpest eyes in the group.
    Sharpest,
    Smile,
    /// No usable face and softer than the group's other such frames.
    SoftFrame,
}

impl Reason {
    /// The kebab-case name used in JSON and darktable tags.
    pub fn slug(self) -> String {
        serde_json::to_value(self).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
    pub mark: Mark,
    pub reasons: Vec<Reason>,
    /// 0–1, relative to the group's best; used for ordering and picks.
    pub score: f32,
    /// Eye (or frame) sharpness relative to the group's best, 0–1.
    pub rel_sharp: f32,
}

pub fn sig_dist(a: &[u8], b: &[u8]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 1.0;
    }
    a.iter().zip(b).map(|(&x, &y)| (x as f32 - y as f32).abs()).sum::<f32>() / (a.len() as f32 * 255.0)
}

/// How far the main face moved between two frames: centre shift (fraction of
/// the frame) plus the log of the size change. None when either has no face.
pub fn face_dist(a: &ShotAnalysis, b: &ShotAnalysis) -> Option<f32> {
    let (fa, fb) = (a.faces.first()?, b.faces.first()?);
    let c = |s: &ShotAnalysis, f: &FaceMetrics| {
        ((f.bbox[0] + f.bbox[2] / 2.0) / s.width as f32, (f.bbox[1] + f.bbox[3] / 2.0) / s.height as f32)
    };
    let ((ax, ay), (bx, by)) = (c(a, fa), c(b, fb));
    Some((ax - bx).abs() + (ay - by).abs() + (fa.bbox[2].max(1.0) / fb.bbox[2].max(1.0)).ln().abs())
}

/// Consecutive runs of similar frames. `shots` must be in capture order.
pub fn group(shots: &[ShotAnalysis], cfg: &Config) -> Vec<Vec<usize>> {
    let mut groups: Vec<Vec<usize>> = vec![];
    for (i, s) in shots.iter().enumerate() {
        let new = match i.checked_sub(1).map(|j| &shots[j]) {
            None => true,
            Some(p) => {
                s.time - p.time > cfg.scene_gap
                    || (s.width > s.height) != (p.width > p.height)
                    || sig_dist(&s.sig, &p.sig) + cfg.face_weight * face_dist(s, p).unwrap_or(0.0) > cfg.split_dist
            }
        };
        if new {
            groups.push(vec![i]);
        } else {
            groups.last_mut().unwrap().push(i);
        }
    }
    groups
}

fn usable<'a>(s: &'a ShotAnalysis, cfg: &Config) -> Vec<&'a FaceMetrics> {
    s.faces.iter().filter(|f| f.presence >= cfg.min_presence && f.eye_px[0].max(f.eye_px[1]) >= cfg.min_eye_px).collect()
}

/// One eye as the UI shows it, from its eyeBlink score.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EyeState {
    Open,
    Half,
    Closed,
}

pub fn eye_state(blink: f32, cfg: &Config) -> EyeState {
    if blink >= cfg.blink_closed {
        EyeState::Closed
    } else if blink >= cfg.blink_both {
        EyeState::Half
    } else {
        EyeState::Open
    }
}

fn eyes_closed(f: &FaceMetrics, cfg: &Config) -> bool {
    f.blink[0].max(f.blink[1]) >= cfg.blink_closed && f.blink[0].min(f.blink[1]) >= cfg.blink_both
}

pub fn judge(shots: &[ShotAnalysis], groups: &[Vec<usize>], cfg: &Config) -> Vec<Verdict> {
    let mut out: Vec<Verdict> =
        shots.iter().map(|_| Verdict { mark: Mark::None, reasons: vec![], score: 0.0, rel_sharp: 0.0 }).collect();

    for g in groups {
        struct Row {
            i: usize,
            faces: bool,
            closed: bool,
            sharp: f32,
            smile: f32,
            cut: bool,
        }
        let rows: Vec<Row> = g
            .iter()
            .map(|&i| {
                let s = &shots[i];
                let fs = usable(s, cfg);
                if fs.is_empty() {
                    return Row { i, faces: false, closed: false, sharp: s.frame_sharp, smile: 0.0, cut: false };
                }
                // Group shots: the worst face decides.
                Row {
                    i,
                    faces: true,
                    closed: fs.iter().any(|f| eyes_closed(f, cfg)),
                    sharp: fs.iter().map(|f| f.eye_sharp[0].max(f.eye_sharp[1])).fold(f32::MAX, f32::min),
                    smile: fs.iter().map(|f| f.smile).fold(f32::MAX, f32::min),
                    cut: fs.iter().any(|f| f.cut),
                }
            })
            .collect();

        let best_face = rows.iter().filter(|r| r.faces && !r.closed).map(|r| r.sharp).fold(0.0, f32::max);
        let best_frame = rows.iter().filter(|r| !r.faces).map(|r| r.sharp).fold(0.0, f32::max);
        let n_face = rows.iter().filter(|r| r.faces).count();
        let n_frame = rows.len() - n_face;

        for r in &rows {
            let s = &shots[r.i];
            let v = &mut out[r.i];
            if r.faces {
                v.rel_sharp = if best_face > 0.0 { (r.sharp / best_face).min(1.0) } else { 0.0 };
                if r.closed {
                    v.reasons.push(Reason::EyesClosed);
                    v.mark = Mark::Reject;
                }
                if n_face >= 2 && best_face > 0.0 && v.rel_sharp < cfg.soft_rel {
                    v.reasons.push(Reason::SoftEyes);
                    v.mark = Mark::Reject;
                }
                if r.cut {
                    v.reasons.push(Reason::FaceCut);
                }
                if r.smile > 0.5 {
                    v.reasons.push(Reason::Smile);
                }
                v.score = v.rel_sharp * if r.closed { 0.3 } else { 1.0 } * if r.cut { 0.9 } else { 1.0 } + 0.05 * r.smile;
            } else {
                v.reasons.push(if s.faces.is_empty() {
                    Reason::NoFace
                } else if s.faces.iter().all(|f| f.presence < cfg.min_presence) {
                    Reason::FaceHidden
                } else {
                    Reason::SmallFace
                });
                v.rel_sharp = if best_frame > 0.0 { (r.sharp / best_frame).min(1.0) } else { 0.0 };
                // Frames without a judgeable face are never auto-rejected: a
                // figure from behind is often the point of the picture.
                if n_frame >= 2 && v.rel_sharp < 0.6 {
                    v.reasons.push(Reason::SoftFrame);
                }
                // Ranked below faces when a group mixes both.
                v.score = v.rel_sharp * 0.9;
            }
        }

        // Picks: the best non-rejected frame, plus runners-up in big groups
        // when they're nearly as good.
        let mut order: Vec<usize> = g.iter().copied().filter(|&i| out[i].mark != Mark::Reject).collect();
        order.sort_by(|&a, &b| out[b].score.total_cmp(&out[a].score));
        let n_picks = 1 + (g.len() >= 8) as usize + (g.len() >= 16) as usize;
        if let Some(&top) = order.first() {
            let top_score = out[top].score;
            for &i in order.iter().take(n_picks) {
                if out[i].score >= top_score * 0.9 {
                    out[i].mark = Mark::Pick;
                    if out[i].rel_sharp >= 0.999 && !usable(&shots[i], cfg).is_empty() {
                        out[i].reasons.insert(0, Reason::Sharpest);
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(sharp: f32, blink: f32) -> FaceMetrics {
        FaceMetrics {
            bbox: [2000.0, 1000.0, 800.0, 800.0],
            det_score: 0.9,
            presence: 1.0,
            eye_px: [200.0; 2],
            eye_sharp: [sharp, sharp * 0.9],
            face_sharp: 0.2,
            blink: [blink; 2],
            ear: [0.3; 2],
            smile: 0.0,
            face_luma: 50.0,
            face_clip: 0.0,
            cut: false,
            eye_boxes: [[0.0; 4]; 2],
            eyes_box: [0.0; 4],
        }
    }

    fn shot(t: f64, faces: Vec<FaceMetrics>) -> ShotAnalysis {
        ShotAnalysis {
            file: format!("{t}.CR3"),
            size: 0,
            mtime: 0,
            time: t,
            width: 6000,
            height: 4000,
            faces,
            frame_sharp: 0.3,
            sig: vec![100; 256],
            error: None,
        }
    }

    #[test]
    fn marks_closed_soft_and_best() {
        let shots = vec![
            shot(0.0, vec![face(0.40, 0.05)]),
            shot(1.0, vec![face(0.15, 0.05)]),
            shot(2.0, vec![face(0.45, 0.70)]),
            shot(3.0, vec![face(0.38, 0.10)]),
            shot(4.0, vec![]),
        ];
        let cfg = Config::default();
        let groups = group(&shots, &cfg);
        assert_eq!(groups, vec![vec![0, 1, 2, 3, 4]]);
        let v = judge(&shots, &groups, &cfg);
        assert_eq!(v[0].mark, Mark::Pick);
        assert!(v[0].reasons.contains(&Reason::Sharpest));
        assert_eq!(v[1].mark, Mark::Reject);
        assert_eq!(v[1].reasons, vec![Reason::SoftEyes]);
        // Closed eyes don't set the group's sharpness bar.
        assert_eq!(v[2].mark, Mark::Reject);
        assert!(v[2].reasons.contains(&Reason::EyesClosed));
        assert_eq!(v[3].mark, Mark::None);
        assert_eq!(v[4].mark, Mark::None);
        assert_eq!(v[4].reasons, vec![Reason::NoFace]);
    }

    #[test]
    fn splits_on_gap_orientation_and_content() {
        let mut shots: Vec<_> = (0..6).map(|i| shot(i as f64, vec![])).collect();
        shots[2].time = 100.0;
        (shots[3].time, shots[4].time, shots[5].time) = (101.0, 102.0, 103.0);
        (shots[4].width, shots[4].height) = (4000, 6000);
        shots[5].sig = vec![200; 256];
        (shots[5].width, shots[5].height) = (4000, 6000);
        assert_eq!(group(&shots, &Config::default()), vec![vec![0, 1], vec![2, 3], vec![4], vec![5]]);
    }
}
