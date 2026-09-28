//! Reframe: crop suggestions for picked frames.
//!
//! A [`Scene`] holds what the scorer needs from one frame: face landmarks and
//! coarse tile grids (sharpness, luma, edge energy). It is built on demand for
//! picks only and cached per frame, so the main analysis stays as it is.
//! Candidates are scored with summed-area tables, so a few hundred thousand
//! of them take milliseconds.
//!
//! Crops are normalised to the upright frame: the space of the display JPEG,
//! and of darktable's crop, which runs after flip.
//!
//! Not yet, both need a pose or person model: cuts through joints (knees,
//! elbows, wrists; only the band under the chin is checked), and frames
//! without a face, which get only the frame as shot.

use std::path::PathBuf;

use anyhow::Result;
use image::{GrayImage, RgbImage, imageops};
use serde::{Deserialize, Serialize};

use crate::cache::{self, Cache};
use crate::face::Models;
use crate::preview::{self, Shot};
use crate::{measure, resize};

/// Bump when the stored [`Scene`] changes meaning.
const SCENE_VERSION: u32 = 1;
/// Tiles along the long side of the grids.
const GRID: usize = 64;
/// Border strips are this fraction of the crop's side, cut into `SEGS` pieces.
const STRIP: f32 = 0.06;
const SEGS: usize = 4;

/// x, y, w, h as fractions of the upright frame.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Crop {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Crop {
    pub const FULL: Crop = Crop { x: 0.0, y: 0.0, w: 1.0, h: 1.0 };

    fn rect(&self) -> Rect {
        [self.x, self.y, self.x + self.w, self.y + self.h]
    }

    pub fn iou(&self, o: &Crop) -> f32 {
        let i = area(inter(self.rect(), o.rect()));
        i / (self.w * self.h + o.w * o.h - i)
    }

    /// x, y, w, h in pixels of a `width` × `height` frame.
    pub fn pixels(&self, width: u32, height: u32) -> [u32; 4] {
        let (fw, fh) = (width as f32, height as f32);
        let x = (self.x * fw).round().clamp(0.0, fw - 1.0);
        let y = (self.y * fh).round().clamp(0.0, fh - 1.0);
        let w = (self.w * fw).round().clamp(1.0, fw - x);
        let h = (self.h * fh).round().clamp(1.0, fh - y);
        [x as u32, y as u32, w as u32, h as u32]
    }

    /// Long side in pixels of a `width` × `height` frame.
    pub fn long_side(&self, width: u32, height: u32) -> u32 {
        (self.w * width as f32).max(self.h * height as f32).round() as u32
    }

    /// Short stable id, for kept crops.
    pub fn id(&self) -> String {
        let bytes: Vec<u8> = [self.x, self.y, self.w, self.h].iter().flat_map(|v| v.to_bits().to_le_bytes()).collect();
        format!("{:08x}", cache::fnv1a(&bytes) as u32)
    }
}

/// Width : height.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ratio(pub u8, pub u8);

impl Ratio {
    pub fn value(self) -> f32 {
        self.0 as f32 / self.1 as f32
    }

    /// The simplest ratio within 0.5% of `w` : `h`.
    pub fn of(w: u32, h: u32) -> Ratio {
        let v = w as f32 / h.max(1) as f32;
        let mut best = (f32::MAX, Ratio(1, 1));
        for d in 1..=32u8 {
            let n = (v * d as f32).round().clamp(1.0, 255.0) as u8;
            let err = ((n as f32 / d as f32) / v - 1.0).abs();
            if err < 0.005 {
                return Ratio(n, d);
            }
            if err < best.0 {
                best = (err, Ratio(n, d));
            }
        }
        best.1
    }
}

impl std::fmt::Display for Ratio {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}:{}", self.0, self.1)
    }
}

/// Why a suggestion scored well, shown in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Note {
    /// The frame as shot, offered as a baseline.
    Original,
    EyesOnThird,
    /// Room in front of a turned face.
    LeadRoom,
    /// A frontal face placed centrally.
    Centered,
    /// No bright or busy patch on the border.
    CleanEdges,
    /// The face is much sharper than the rest of the crop.
    SubjectPops,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Suggestion {
    pub crop: Crop,
    pub ratio: Ratio,
    pub score: f32,
    pub notes: Vec<Note>,
}

/// A crop the user kept; sent to darktable as a duplicate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeptCrop {
    pub id: String,
    pub crop: Crop,
    pub ratio: Ratio,
}

/// One face, normalised to the frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Face {
    /// Landmark box, x, y, w, h.
    pub bbox: [f32; 4],
    /// Eye centres (subject's right, left).
    pub eyes: [[f32; 2]; 2],
    /// Mesh 152 and 10.
    pub chin: [f32; 2],
    pub forehead: [f32; 2],
    /// Nose offset from the outer eye corners' midpoint along the eye line,
    /// over the eye distance: > 0 looks toward the right of the image.
    pub yaw: f32,
}

/// Per-tile measurements, row-major, `cols` × `rows` over the whole frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Grid {
    pub cols: usize,
    pub rows: usize,
    /// [`measure::sharpness`]; 0 for flat tiles.
    pub sharp: Vec<f32>,
    /// Mean luma, 0–1.
    pub luma: Vec<f32>,
    /// Mean gradient magnitude, 0–1.
    pub edge: Vec<f32>,
}

impl Grid {
    pub fn from_fn(cols: usize, rows: usize, f: impl Fn(usize, usize) -> [f32; 3]) -> Self {
        let (mut sharp, mut luma, mut edge) = (vec![], vec![], vec![]);
        for r in 0..rows {
            for c in 0..cols {
                let [s, l, e] = f(c, r);
                sharp.push(s);
                luma.push(l);
                edge.push(e);
            }
        }
        Grid { cols, rows, sharp, luma, edge }
    }
}

/// What the scorer knows about one frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scene {
    /// Full-resolution size, upright.
    pub width: u32,
    pub height: u32,
    /// Largest first; the first is the subject.
    pub faces: Vec<Face>,
    pub grid: Grid,
}

impl Scene {
    pub fn build(models: &Models, img: &RgbImage) -> Result<Scene> {
        let dets = models.detect(img, crate::MIN_FACE_SCORE)?;
        let biggest = dets.first().map(|d| d.bbox[2]).unwrap_or(0.0);
        let mut faces = vec![];
        for d in dets.iter().filter(|d| d.bbox[2] >= biggest * crate::MIN_FACE_REL) {
            faces.push(face(&models.mesh(img, d)?.points, img.width(), img.height()));
        }
        Ok(Scene { width: img.width(), height: img.height(), faces, grid: grid(img) })
    }

    /// The shot's scene from `reframe/<stem>.json`, built and stored if missing or stale.
    pub fn cached(models: &Models, shot: &Shot, cache: &Cache) -> Result<Scene> {
        let path = scene_path(shot, cache);
        let (size, mtime) = cache::stamp(&shot.path);
        if let Some(s) = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<Stored>(&b).ok())
            && s.version == SCENE_VERSION
            && (s.size, s.mtime) == (size, mtime)
        {
            return Ok(s.scene);
        }
        let scene = Scene::build(models, &preview::load(shot)?)?;
        std::fs::create_dir_all(path.parent().unwrap())?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(&Stored { version: SCENE_VERSION, size, mtime, scene: scene.clone() })?)?;
        std::fs::rename(tmp, path)?;
        Ok(scene)
    }
}

#[derive(Serialize, Deserialize)]
struct Stored {
    version: u32,
    size: u64,
    mtime: i64,
    scene: Scene,
}

fn scene_path(shot: &Shot, cache: &Cache) -> PathBuf {
    let stem = shot.path.file_stem().unwrap_or_default().to_string_lossy();
    cache.dir.join("reframe").join(format!("{stem}.json"))
}

fn face(p: &[[f32; 3]], width: u32, height: u32) -> Face {
    let (fw, fh) = (width as f32, height as f32);
    let n = |q: [f32; 2]| [q[0] / fw, q[1] / fh];
    let mid = |a: usize, b: usize| [(p[a][0] + p[b][0]) / 2.0, (p[a][1] + p[b][1]) / 2.0];
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for q in p {
        (x0, y0, x1, y1) = (x0.min(q[0]), y0.min(q[1]), x1.max(q[0]), y1.max(q[1]));
    }
    // Project the nose onto the outer-corner line, so head roll doesn't read as yaw.
    let (r, l) = (p[33], p[263]);
    let (ex, ey) = (l[0] - r[0], l[1] - r[1]);
    let m = mid(33, 263);
    let yaw = ((p[1][0] - m[0]) * ex + (p[1][1] - m[1]) * ey) / (ex * ex + ey * ey).max(1.0);
    Face {
        bbox: [x0 / fw, y0 / fh, (x1 - x0) / fw, (y1 - y0) / fh],
        eyes: [n(mid(33, 133)), n(mid(263, 362))],
        chin: n([p[152][0], p[152][1]]),
        forehead: n([p[10][0], p[10][1]]),
        yaw,
    }
}

/// Tile grids on a half-resolution grey copy (edges at quarter resolution,
/// so they follow structure rather than texture).
fn grid(img: &RgbImage) -> Grid {
    let (w, h) = img.dimensions();
    let (cols, rows) = if w >= h {
        (GRID, ((GRID as f32 * h as f32 / w as f32).round() as usize).max(1))
    } else {
        (((GRID as f32 * w as f32 / h as f32).round() as usize).max(1), GRID)
    };
    let half = imageops::grayscale(&resize::rgb(img, None, w / 2, h / 2));
    let quarter = imageops::grayscale(&resize::rgb(img, None, w / 4, h / 4));
    let grad = gradient(&quarter);
    let span = |i: usize, n: usize, len: u32| ((i as u64 * len as u64 / n as u64) as u32, ((i + 1) as u64 * len as u64 / n as u64) as u32);
    Grid::from_fn(cols, rows, |c, r| {
        let (x0, x1) = span(c, cols, half.width());
        let (y0, y1) = span(r, rows, half.height());
        let tile = imageops::crop_imm(&half, x0, y0, x1 - x0, y1 - y0).to_image();
        let n = tile.len().max(1) as f32;
        let mean = tile.iter().map(|&v| v as f32).sum::<f32>() / n;
        let std = (tile.iter().map(|&v| (v as f32 - mean).powi(2)).sum::<f32>() / n).sqrt();
        // Flat tiles (sky, a wall, deep shadow) have no detail to judge.
        let sharp = if std > 6.0 { measure::sharpness(&tile) } else { 0.0 };
        let (gx0, gx1) = span(c, cols, grad.width());
        let (gy0, gy1) = span(r, rows, grad.height());
        let mut e = 0.0;
        for y in gy0..gy1 {
            for x in gx0..gx1 {
                e += grad.get_pixel(x, y)[0] as f32;
            }
        }
        let e = e / ((gx1 - gx0) * (gy1 - gy0)).max(1) as f32 / 255.0;
        [sharp, mean / 255.0, e]
    })
}

/// |∂x| + |∂y| by central differences, halved to stay in u8.
fn gradient(g: &GrayImage) -> GrayImage {
    let (w, h) = g.dimensions();
    GrayImage::from_fn(w, h, |x, y| {
        let px = |x: u32, y: u32| g.get_pixel(x.min(w - 1), y.min(h - 1))[0] as i32;
        let dx = px(x + 1, y) - px(x.saturating_sub(1), y);
        let dy = px(x, y + 1) - px(x, y.saturating_sub(1));
        image::Luma([((dx.abs() + dy.abs()) / 2).min(255) as u8])
    })
}

#[derive(Debug, Clone)]
pub struct ReframeConfig {
    pub ratios: Vec<Ratio>,
    /// Candidate sizes as fractions of the largest crop of a ratio that fits.
    pub sizes: usize,
    pub min_scale: f32,
    pub max_scale: f32,
    /// Position grid, fraction of the frame.
    pub step: f32,
    /// Smallest crop: long side in full-resolution pixels (WhatsApp HD needs
    /// > 1600; 2560 keeps it clearly HD).
    pub min_long: u32,
    /// Space kept around the face landmark box, in face heights: sides, top, bottom.
    pub margin: [f32; 3],
    /// |yaw| above this is a turned face that wants lead room.
    pub yaw_turned: f32,
    /// Depth of the band under the chin a bottom edge shouldn't cut, in face heights.
    pub chin_band: f32,
    /// Border patches brighter than the border's mean by more than this (luma 0–1) count.
    pub bright_slack: f32,
    /// Background tiles sharper than this fraction of the face count as clutter.
    pub clutter: f32,
    pub w_eyes: f32,
    pub w_lead: f32,
    pub w_cut: f32,
    pub w_bright: f32,
    pub w_busy: f32,
    pub w_pop: f32,
    pub w_clutter: f32,
    pub max_iou: f32,
    /// Suggestions score within this of the best, and place the eyes and the
    /// look direction within `gate` of the best any candidate reaches, so
    /// diversity never fills the list with poor framings.
    pub slack: f32,
    pub gate: f32,
    /// Eyes and look-direction errors this small always pass the gate.
    pub tolerance: f32,
    pub per_ratio: usize,
    pub total: usize,
}

impl Default for ReframeConfig {
    fn default() -> Self {
        Self {
            ratios: vec![Ratio(4, 5), Ratio(1, 1), Ratio(3, 2), Ratio(16, 9), Ratio(2, 3)],
            sizes: 12,
            min_scale: 0.35,
            max_scale: 0.9,
            step: 0.015,
            min_long: 2560,
            margin: [0.3, 0.25, 0.1],
            yaw_turned: 0.1,
            chin_band: 0.3,
            bright_slack: 0.1,
            clutter: 0.6,
            w_eyes: 4.0,
            w_lead: 3.0,
            w_cut: 1.0,
            w_bright: 2.0,
            w_busy: 0.25,
            w_pop: 0.4,
            w_clutter: 2.0,
            max_iou: 0.5,
            slack: 1.0,
            gate: 0.04,
            tolerance: 0.05,
            per_ratio: 2,
            total: 5,
        }
    }
}

/// The score's parts for one crop (penalties ≥ 0; `pop` is a reward).
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct Terms {
    /// |eye line − upper third|, fraction of the crop's height.
    pub eyes: f32,
    /// Distance of the eyes from where the look direction wants them, fraction of width.
    pub lead: f32,
    /// Bottom edge in the band under the chin.
    pub cut: f32,
    /// Brightest border patch over the border's mean luma, past the slack.
    pub bright: f32,
    /// Busiest border patch's edge energy over the crop's, minus 1.
    pub busy: f32,
    /// log2(face sharpness / crop sharpness), capped.
    pub pop: f32,
    /// Density of clutter (sharp detail outside the face and figure) in the crop.
    pub clutter: f32,
    pub score: f32,
}

type Rect = [f32; 4];

fn area(r: Rect) -> f32 {
    (r[2] - r[0]).max(0.0) * (r[3] - r[1]).max(0.0)
}

fn inter(a: Rect, b: Rect) -> Rect {
    [a[0].max(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].min(b[3])]
}

fn inside(inner: Rect, outer: Rect) -> bool {
    inner[0] >= outer[0] && inner[1] >= outer[1] && inner[2] <= outer[2] && inner[3] <= outer[3]
}

/// Summed-area table over a tile grid; lookups interpolate, so rects needn't
/// fall on tile edges (exact for per-tile constants).
struct Sat {
    cols: usize,
    rows: usize,
    s: Vec<f64>,
}

impl Sat {
    fn new(cols: usize, rows: usize, v: &[f32]) -> Self {
        let mut s = vec![0f64; (cols + 1) * (rows + 1)];
        for r in 0..rows {
            let mut run = 0.0;
            for c in 0..cols {
                run += v[r * cols + c] as f64;
                s[(r + 1) * (cols + 1) + c + 1] = s[r * (cols + 1) + c + 1] + run;
            }
        }
        Sat { cols, rows, s }
    }

    fn at(&self, u: f32, v: f32) -> f64 {
        let gx = (u.clamp(0.0, 1.0) * self.cols as f32) as f64;
        let gy = (v.clamp(0.0, 1.0) * self.rows as f32) as f64;
        let (i, j) = ((gx as usize).min(self.cols - 1), (gy as usize).min(self.rows - 1));
        let (fx, fy) = (gx - i as f64, gy - j as f64);
        let k = |c: usize, r: usize| self.s[r * (self.cols + 1) + c];
        let top = k(i, j) * (1.0 - fx) + k(i + 1, j) * fx;
        let bot = k(i, j + 1) * (1.0 - fx) + k(i + 1, j + 1) * fx;
        top * (1.0 - fy) + bot * fy
    }

    /// Sum over `r` in tile units.
    fn sum(&self, r: Rect) -> f32 {
        if r[2] <= r[0] || r[3] <= r[1] {
            return 0.0;
        }
        (self.at(r[2], r[3]) - self.at(r[0], r[3]) - self.at(r[2], r[1]) + self.at(r[0], r[1])) as f32
    }

    fn mean(&self, r: Rect) -> f32 {
        self.sum(r) / (area(r) * (self.cols * self.rows) as f32).max(1e-6)
    }
}

struct Subject {
    /// Face box plus margins; must be inside every crop.
    keep: Rect,
    eye: [f32; 2],
    chin: f32,
    height: f32,
    yaw: f32,
    /// Mean sharpness over the face box.
    sharp: f32,
    /// Where the body probably is: a column three face heights wide from the
    /// forehead down. Sharp detail there is the subject, not clutter.
    figure: Rect,
}

struct Scorer<'a> {
    cfg: &'a ReframeConfig,
    width: u32,
    height: u32,
    subject: Option<Subject>,
    /// Other faces: never cut, either in or out.
    others: Vec<Rect>,
    sharp: Sat,
    luma: Sat,
    edge: Sat,
    clutter: Sat,
}

impl<'a> Scorer<'a> {
    fn new(scene: &Scene, cfg: &'a ReframeConfig) -> Self {
        let g = &scene.grid;
        let sharp = Sat::new(g.cols, g.rows, &g.sharp);
        let rect = |b: [f32; 4]| [b[0], b[1], b[0] + b[2], b[1] + b[3]];
        // Face heights are fractions of the frame's height; this turns them into widths.
        let aspect = scene.height as f32 / scene.width as f32;
        let subject = scene.faces.first().map(|f| {
            let fh = f.chin[1] - f.forehead[1];
            let [ms, mt, mb] = cfg.margin.map(|m| m * fh);
            let ms = ms * aspect;
            let b = rect(f.bbox);
            let eye = [(f.eyes[0][0] + f.eyes[1][0]) / 2.0, (f.eyes[0][1] + f.eyes[1][1]) / 2.0];
            Subject {
                keep: [b[0] - ms, b[1] - mt, b[2] + ms, b[3] + mb],
                eye,
                chin: f.chin[1],
                height: fh,
                yaw: f.yaw,
                sharp: sharp.mean(b),
                figure: [eye[0] - 1.5 * fh * aspect, b[1] - mt, eye[0] + 1.5 * fh * aspect, 1.0],
            }
        });
        let others = scene.faces.iter().skip(1).map(|f| rect(f.bbox)).collect();
        let clutter = match &subject {
            Some(s) => {
                let thr = s.sharp * cfg.clutter;
                g.sharp.iter().map(|&x| ((x - thr) / s.sharp.max(1e-3)).max(0.0)).collect()
            }
            None => vec![0.0; g.sharp.len()],
        };
        Scorer {
            cfg,
            width: scene.width,
            height: scene.height,
            subject,
            others,
            sharp,
            luma: Sat::new(g.cols, g.rows, &g.luma),
            edge: Sat::new(g.cols, g.rows, &g.edge),
            clutter: Sat::new(g.cols, g.rows, &clutter),
        }
    }

    /// Area of `r` in tiles.
    fn tiles(&self, r: Rect) -> f32 {
        (area(r) * (self.sharp.cols * self.sharp.rows) as f32).max(1e-6)
    }

    /// None when the crop breaks a hard rule.
    fn eval(&self, c: &Crop) -> Option<Terms> {
        let cfg = self.cfg;
        if c.long_side(self.width, self.height) < cfg.min_long {
            return None;
        }
        let r = c.rect();
        if self.others.iter().any(|&f| {
            let i = area(inter(f, r));
            i > 0.02 * area(f) && i < 0.98 * area(f)
        }) {
            return None;
        }
        let mut t = Terms::default();
        if let Some(s) = &self.subject {
            if !inside(s.keep, r) {
                return None;
            }
            t.eyes = ((s.eye[1] - c.y) / c.h - 1.0 / 3.0).abs();
            let fx = (s.eye[0] - c.x) / c.w;
            t.lead = if s.yaw > cfg.yaw_turned {
                (fx - 1.0 / 3.0).abs()
            } else if s.yaw < -cfg.yaw_turned {
                (fx - 2.0 / 3.0).abs()
            } else {
                (1.0 / 3.0 - fx).max(fx - 2.0 / 3.0).max(0.0)
            };
            let bottom = r[3];
            if bottom > s.chin && bottom < s.chin + cfg.chin_band * s.height {
                t.cut = 1.0;
            }
            t.pop = (s.sharp / self.sharp.mean(r).max(1e-3)).log2().clamp(0.0, 1.5);
            t.clutter = (self.clutter.sum(r) - self.clutter.sum(inter(r, s.figure))) / self.tiles(r);
        }

        let (tw, th) = (c.w * STRIP, c.h * STRIP);
        let [x0, y0, x1, y1] = r;
        let strips = [[x0, y0, x0 + tw, y1], [x1 - tw, y0, x1, y1], [x0, y0, x1, y0 + th], [x0, y1 - th, x1, y1]];
        // Hot spots: border patches brighter than the border as a whole, so a
        // backlit frame's bright surround isn't held against every crop.
        let ring = strips.iter().map(|&s| self.luma.mean(s)).sum::<f32>() / 4.0;
        let edge = self.edge.mean(r);
        let (mut bright, mut busy) = (0f32, 0f32);
        for (k, s) in strips.iter().enumerate() {
            for i in 0..SEGS {
                let f = |a: f32, b: f32, i: usize| a + (b - a) * i as f32 / SEGS as f32;
                let seg = if k < 2 { [s[0], f(y0, y1, i), s[2], f(y0, y1, i + 1)] } else { [f(x0, x1, i), s[1], f(x0, x1, i + 1), s[3]] };
                bright = bright.max(self.luma.mean(seg) - ring);
                busy = busy.max(self.edge.mean(seg) / (edge + 0.01) - 1.0);
            }
        }
        t.bright = (bright - cfg.bright_slack).max(0.0);
        t.busy = busy.clamp(0.0, 3.0);
        t.score = cfg.w_pop * t.pop
            - cfg.w_eyes * t.eyes
            - cfg.w_lead * t.lead
            - cfg.w_cut * t.cut
            - cfg.w_bright * t.bright
            - cfg.w_busy * t.busy
            - cfg.w_clutter * t.clutter;
        Some(t)
    }

    fn notes(&self, t: &Terms) -> Vec<Note> {
        let mut n = vec![];
        if let Some(s) = &self.subject {
            if t.eyes < 0.03 {
                n.push(Note::EyesOnThird);
            }
            if s.yaw.abs() > self.cfg.yaw_turned && t.lead < 0.05 {
                n.push(Note::LeadRoom);
            } else if s.yaw.abs() <= self.cfg.yaw_turned && t.lead == 0.0 {
                n.push(Note::Centered);
            }
            if t.pop >= 1.0 && t.clutter < 0.05 {
                n.push(Note::SubjectPops);
            }
        }
        if t.bright == 0.0 && t.busy < 0.3 {
            n.push(Note::CleanEdges);
        }
        n
    }
}

/// Every candidate that passes the hard rules: (crop, ratio, terms).
fn candidates(scene: &Scene, sc: &Scorer, cfg: &ReframeConfig) -> Vec<(Crop, Ratio, Terms)> {
    let frame = scene.width as f32 / scene.height as f32;
    let steps = |len: f32| {
        let n = ((1.0 - len) / cfg.step).floor().max(0.0) as usize;
        let mut v: Vec<f32> = (0..=n).map(|i| i as f32 * cfg.step).collect();
        if 1.0 - len - v[n] > 1e-4 {
            v.push(1.0 - len);
        }
        v
    };
    let mut out = vec![];
    for &ratio in &cfg.ratios {
        // Largest crop of this ratio that fits, as fractions of the frame.
        let (fw, fh) = if ratio.value() >= frame { (1.0, frame / ratio.value()) } else { (ratio.value() / frame, 1.0) };
        // Spread the sizes over what the floor allows, so none are wasted below it.
        let long = Crop { x: 0.0, y: 0.0, w: fw, h: fh }.long_side(scene.width, scene.height) as f32;
        let lo = cfg.min_scale.max(cfg.min_long as f32 / long);
        if lo > 1.0 {
            continue;
        }
        let hi = cfg.max_scale.max(lo);
        // Only one size left when the floor is above `max_scale`.
        let sizes = if hi > lo { cfg.sizes } else { 1 };
        for k in 0..sizes {
            let s = lo + (hi - lo) * k as f32 / (sizes - 1).max(1) as f32;
            // A hair above the floor, so rounding doesn't drop the smallest size.
            let s = if k == 0 && lo > cfg.min_scale { (s * 1.0005).min(1.0) } else { s };
            let (w, h) = (fw * s, fh * s);
            for &y in &steps(h) {
                for &x in &steps(w) {
                    let c = Crop { x, y, w, h };
                    if let Some(t) = sc.eval(&c) {
                        out.push((c, ratio, t));
                    }
                }
            }
        }
    }
    out
}

/// Diverse crop suggestions, best first, then the frame as shot.
pub fn suggest(scene: &Scene, cfg: &ReframeConfig) -> Vec<Suggestion> {
    let sc = Scorer::new(scene, cfg);
    // Without a face nothing protects the subject from being cut.
    let mut all = if sc.subject.is_some() { candidates(scene, &sc, cfg) } else { vec![] };
    all.sort_by(|a, b| b.2.score.total_cmp(&a.2.score));
    let floor = all.first().map_or(0.0, |c| c.2.score - cfg.slack);
    let best = |f: fn(&Terms) -> f32| all.iter().map(|c| f(&c.2)).fold(f32::MAX, f32::min);
    let (eyes, lead) = ((best(|t| t.eyes) + cfg.gate).max(cfg.tolerance), (best(|t| t.lead) + cfg.gate).max(cfg.tolerance));
    let mut out: Vec<Suggestion> = vec![];
    for (crop, ratio, t) in all {
        if out.len() == cfg.total || t.score < floor {
            break;
        }
        if t.eyes > eyes
            || t.lead > lead
            || t.cut > 0.0
            || out.iter().filter(|s| s.ratio == ratio).count() >= cfg.per_ratio
            || out.iter().any(|s| s.crop.iou(&crop) >= cfg.max_iou)
        {
            continue;
        }
        out.push(Suggestion { crop, ratio, score: t.score, notes: sc.notes(&t) });
    }
    let full = sc.eval(&Crop::FULL).unwrap_or_default();
    let mut notes = vec![Note::Original];
    notes.extend(sc.notes(&full));
    out.push(Suggestion { crop: Crop::FULL, ratio: Ratio::of(scene.width, scene.height), score: full.score, notes });
    out
}

/// The score's parts for any crop (None if it breaks a hard rule).
pub fn terms(scene: &Scene, cfg: &ReframeConfig, crop: &Crop) -> Option<Terms> {
    Scorer::new(scene, cfg).eval(crop)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 6000×4000 frame, flat background, one face: eyes at (`ex`, `ey`),
    /// face height `fh` (forehead to chin), looking with `yaw`.
    fn scene(ex: f32, ey: f32, fh: f32, yaw: f32) -> Scene {
        let forehead = ey - 0.375 * fh;
        let chin = forehead + fh;
        let fw = fh * 0.8 * 4000.0 / 6000.0;
        let face = Face {
            bbox: [ex - fw / 2.0, forehead, fw, fh],
            eyes: [[ex - fw * 0.2, ey], [ex + fw * 0.2, ey]],
            chin: [ex, chin],
            forehead: [ex, forehead],
            yaw,
        };
        let b = face.bbox;
        let grid = Grid::from_fn(64, 43, |c, r| {
            let (u, v) = ((c as f32 + 0.5) / 64.0, (r as f32 + 0.5) / 43.0);
            let on_face = u > b[0] && u < b[0] + b[2] && v > b[1] && v < b[1] + b[3];
            if on_face { [1.0, 0.4, 0.2] } else { [0.1, 0.4, 0.05] }
        });
        Scene { width: 6000, height: 4000, faces: vec![face], grid }
    }

    fn crops(s: &[Suggestion]) -> Vec<Crop> {
        s.iter().filter(|s| !s.notes.contains(&Note::Original)).map(|s| s.crop).collect()
    }

    #[test]
    fn face_looking_right_sits_on_left_third() {
        let s = suggest(&scene(0.45, 0.35, 0.12, 0.3), &ReframeConfig::default());
        assert!(crops(&s).len() >= 2);
        for c in crops(&s) {
            let fx = (0.45 - c.x) / c.w;
            assert!((fx - 1.0 / 3.0).abs() < 0.06, "{c:?} puts the face at {fx}");
        }
        let s = suggest(&scene(0.55, 0.35, 0.12, -0.3), &ReframeConfig::default());
        for c in crops(&s) {
            let fx = (0.55 - c.x) / c.w;
            assert!((fx - 2.0 / 3.0).abs() < 0.06, "{c:?} puts the face at {fx}");
        }
    }

    #[test]
    fn eye_line_on_upper_third() {
        let s = suggest(&scene(0.5, 0.4, 0.12, 0.0), &ReframeConfig::default());
        assert!(crops(&s).len() >= 3);
        for c in crops(&s) {
            let fy = (0.4 - c.y) / c.h;
            assert!((fy - 1.0 / 3.0).abs() <= 0.05, "{c:?} puts the eyes at {fy}");
        }
    }

    #[test]
    fn respects_floor_and_face() {
        let cfg = ReframeConfig::default();
        let sc = scene(0.3, 0.3, 0.2, 0.0);
        let b = sc.faces[0].bbox;
        for c in crops(&suggest(&sc, &cfg)) {
            assert!(c.long_side(6000, 4000) >= cfg.min_long, "{c:?}");
            assert!(c.x <= b[0] && c.y <= b[1] && c.x + c.w >= b[0] + b[2] && c.y + c.h >= b[1] + b[3], "{c:?}");
        }
    }

    #[test]
    fn suggestions_stay_whatsapp_hd() {
        // A small face in a corner would otherwise favour tight crops.
        let cfg = ReframeConfig::default();
        let s = crops(&suggest(&scene(0.2, 0.2, 0.06, 0.0), &cfg));
        assert!(!s.is_empty());
        for c in s {
            assert!(c.long_side(6000, 4000) >= 2560, "{c:?} is {} px", c.long_side(6000, 4000));
        }
    }

    #[test]
    fn portrait_crops_on_a_landscape_frame() {
        // 4:5 on 6000×4000 is at most 3200 px tall, so the floor leaves room.
        let s = suggest(&scene(0.5, 0.35, 0.12, 0.0), &ReframeConfig::default());
        assert!(s.iter().any(|s| s.ratio == Ratio(4, 5)), "{s:?}");
    }

    #[test]
    fn avoids_bright_edge_blob() {
        let mut sc = scene(0.5, 0.35, 0.12, 0.0);
        let g = &mut sc.grid;
        for r in 0..g.rows {
            for c in 0..g.cols {
                let (u, v) = ((c as f32 + 0.5) / 64.0, (r as f32 + 0.5) / 43.0);
                if u > 0.88 && (0.25..0.6).contains(&v) {
                    g.luma[r * g.cols + c] = 0.98;
                }
            }
        }
        let s = suggest(&sc, &ReframeConfig::default());
        for c in crops(&s) {
            assert!(c.x + c.w <= 0.88 + 1.0 / 64.0 || c.y > 0.6 || c.y + c.h < 0.25, "{c:?} includes the blob");
        }
    }

    #[test]
    fn suggestions_differ() {
        let cfg = ReframeConfig::default();
        let s = crops(&suggest(&scene(0.5, 0.35, 0.12, 0.0), &cfg));
        assert!(s.len() >= 4, "{s:?}");
        for (i, a) in s.iter().enumerate() {
            for b in &s[i + 1..] {
                assert!(a.iou(b) < cfg.max_iou, "{a:?} {b:?}");
            }
        }
    }

    #[test]
    fn avoids_cut_under_chin() {
        // Face high in the frame: eyes on the third want a short crop whose
        // bottom falls just under the chin.
        let cfg = ReframeConfig { min_long: 1000, min_scale: 0.2, ..ReframeConfig::default() };
        let sc = scene(0.5, 0.2, 0.2, 0.0);
        let f = &sc.faces[0];
        let band = f.chin[1]..f.chin[1] + cfg.chin_band * (f.chin[1] - f.forehead[1]);
        let cut = Crop { x: 0.3, y: 0.0, w: 0.4, h: band.start + 0.04 };
        assert_eq!(terms(&sc, &cfg, &cut).unwrap().cut, 1.0);
        for c in crops(&suggest(&sc, &cfg)) {
            assert!(!band.contains(&(c.y + c.h)), "{c:?} cuts under the chin");
        }
    }

    #[test]
    fn no_face_offers_only_the_frame() {
        let mut sc = scene(0.5, 0.35, 0.12, 0.0);
        sc.faces.clear();
        let s = suggest(&sc, &ReframeConfig::default());
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].crop, Crop::FULL);
    }

    #[test]
    fn full_frame_is_offered_last() {
        let s = suggest(&scene(0.5, 0.35, 0.12, 0.0), &ReframeConfig::default());
        let last = s.last().unwrap();
        assert_eq!(last.crop, Crop::FULL);
        assert_eq!(last.ratio, Ratio(3, 2));
        assert!(last.notes.contains(&Note::Original));
    }

    #[test]
    fn sat_matches_direct_sums() {
        let v: Vec<f32> = (0..12).map(|i| i as f32).collect();
        let sat = Sat::new(4, 3, &v);
        assert!((sat.sum([0.0, 0.0, 1.0, 1.0]) - 66.0).abs() < 1e-4);
        // Tile (1,1) = 5, half of it horizontally.
        assert!((sat.sum([0.25, 1.0 / 3.0, 0.375, 2.0 / 3.0]) - 2.5).abs() < 1e-4);
    }

    #[test]
    fn ratio_of_common_frames() {
        assert_eq!(Ratio::of(6000, 4000), Ratio(3, 2));
        assert_eq!(Ratio::of(4000, 6000), Ratio(2, 3));
        assert_eq!(Ratio::of(4032, 3024), Ratio(4, 3));
        assert_eq!(Ratio::of(1920, 1080), Ratio(16, 9));
    }
}
