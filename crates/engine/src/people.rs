//! People: faces grouped by identity within a folder, and a library of named
//! people (shared by every folder) that new folders are matched against.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::ShotAnalysis;

/// Cosine similarity above which two faces are the same person. SFace's
/// published threshold is 0.363; average linkage lets a cluster go a little
/// looser than any single pair would.
pub const SAME: f32 = 0.363;
/// Only confident faces found people: a clear, frontal-enough face gives an
/// embedding at least this long (sharp portraits sit at 11–13; blur,
/// profiles and face-like objects mostly under 8).
pub const CORE_NORM: f32 = 9.5;
const CORE_PRESENCE: f32 = 0.5;
/// Weaker faces (blurred, turned) join a person only on a closer match, and
/// below this length they are not identified at all.
const WEAK_NORM: f32 = 5.5;
const ATTACH: f32 = 0.30;
/// Embeddings kept per named person; the oldest drop out.
const SAMPLES: usize = 24;

/// One face in a folder: shot index and face index within the shot.
pub type FaceRef = (usize, usize);

pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn normalized(mut v: Vec<f32>) -> Vec<f32> {
    let n = dot(&v, &v).sqrt().max(1e-6);
    v.iter_mut().for_each(|x| *x /= n);
    v
}

fn mean(vs: &[&[f32]]) -> Vec<f32> {
    let mut m = vec![0.0; vs.first().map_or(0, |v| v.len())];
    for v in vs {
        m.iter_mut().zip(*v).for_each(|(a, b)| *a += b);
    }
    normalized(m)
}

/// Average-linkage agglomerative clustering of unit vectors: repeatedly merge
/// the two clusters whose mean pairwise similarity is highest, while it is
/// above `thr`. Returns clusters of indices, largest first.
pub fn cluster(vs: &[&[f32]], thr: f32) -> Vec<Vec<usize>> {
    let n = vs.len();
    let mut sim = vec![vec![0f32; n]; n];
    for i in 0..n {
        for j in i + 1..n {
            let s = dot(vs[i], vs[j]);
            sim[i][j] = s;
            sim[j][i] = s;
        }
    }
    // Sum of pairwise similarities between live clusters, kept up to date on merge.
    let mut members: Vec<Vec<usize>> = (0..n).map(|i| vec![i]).collect();
    let mut live: Vec<bool> = vec![true; n];
    loop {
        let mut best = (thr, usize::MAX, usize::MAX);
        for i in 0..n {
            if !live[i] {
                continue;
            }
            for j in i + 1..n {
                if !live[j] {
                    continue;
                }
                let avg = sim[i][j] / (members[i].len() * members[j].len()) as f32;
                if avg > best.0 {
                    best = (avg, i, j);
                }
            }
        }
        let (_, i, j) = best;
        if i == usize::MAX {
            break;
        }
        live[j] = false;
        let moved = std::mem::take(&mut members[j]);
        members[i].extend(moved);
        for k in 0..n {
            if live[k] && k != i {
                let s = sim[i][k] + sim[j][k];
                sim[i][k] = s;
                sim[k][i] = s;
            }
        }
    }
    let mut out: Vec<Vec<usize>> = (0..n).filter(|&i| live[i]).map(|i| std::mem::take(&mut members[i])).collect();
    out.iter_mut().for_each(|c| c.sort());
    out.sort_by(|a, b| b.len().cmp(&a.len()).then(a[0].cmp(&b[0])));
    out
}

/// A named person, remembered across folders.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Known {
    pub id: u32,
    pub name: String,
    pub samples: Vec<Vec<f32>>,
}

/// `~/.local/share/sift/people.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Library {
    #[serde(default)]
    pub people: Vec<Known>,
}

fn library_path() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .context("no data directory")?;
    Ok(base.join("sift").join("people.json"))
}

impl Library {
    pub fn load() -> Self {
        library_path()
            .ok()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let p = library_path()?;
        std::fs::create_dir_all(p.parent().unwrap())?;
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(self)?)?;
        std::fs::rename(tmp, p)?;
        Ok(())
    }

    /// The named person `centroid` looks like, if any: the best mean
    /// similarity to their samples, above `thr`.
    pub fn recognize(&self, centroid: &[f32], thr: f32) -> Option<&Known> {
        self.people
            .iter()
            .map(|k| (k, k.samples.iter().map(|s| dot(s, centroid)).sum::<f32>() / k.samples.len().max(1) as f32))
            .filter(|(_, s)| *s > thr)
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(k, _)| k)
    }

    /// Name a set of faces: adds them to the person called `name` (created if
    /// new, matched case-insensitively). Returns that person's id.
    pub fn learn(&mut self, name: &str, embeds: &[&[f32]]) -> u32 {
        let name = name.trim();
        let idx = match self.people.iter().position(|k| k.name.eq_ignore_ascii_case(name)) {
            Some(i) => i,
            None => {
                let id = self.people.iter().map(|k| k.id).max().unwrap_or(0) + 1;
                self.people.push(Known { id, name: name.to_string(), samples: vec![] });
                self.people.len() - 1
            }
        };
        let k = &mut self.people[idx];
        // Spread the kept samples over the set instead of taking its start.
        let step = (embeds.len() as f32 / SAMPLES as f32).max(1.0);
        let mut t = 0.0;
        while (t as usize) < embeds.len() {
            k.samples.push(embeds[t as usize].to_vec());
            t += step;
        }
        let extra = k.samples.len().saturating_sub(SAMPLES);
        k.samples.drain(..extra);
        k.id
    }
}

impl Library {
    /// Rename person `id`. Taking another person's name merges the two (the
    /// fix for one person split in two); an empty name forgets `id`. Returns
    /// the id that now carries the name, if any.
    pub fn rename(&mut self, id: u32, name: &str) -> Option<u32> {
        let name = name.trim();
        let from = self.people.iter().position(|k| k.id == id)?;
        if name.is_empty() {
            self.people.remove(from);
            return None;
        }
        match self.people.iter().position(|k| k.id != id && k.name.eq_ignore_ascii_case(name)) {
            Some(to) => {
                let moved = self.people.remove(from);
                let to = if to > from { to - 1 } else { to };
                let refs: Vec<&[f32]> = moved.samples.iter().map(|v| v.as_slice()).collect();
                let target = self.people[to].name.clone();
                Some(self.learn(&target, &refs))
            }
            None => {
                self.people[from].name = name.to_string();
                Some(id)
            }
        }
    }
}

/// A person as shown for one folder.
#[derive(Debug, Clone, Serialize)]
pub struct Person {
    /// Stable within the folder while the analysis is unchanged: the library
    /// id for a named person, otherwise 1000 + the cluster's rank.
    pub id: u32,
    pub name: Option<String>,
    pub faces: Vec<FaceRef>,
}

pub const UNNAMED_BASE: u32 = 1000;

/// Group a folder's faces into people and put names on the ones the library
/// knows. `overrides` are the user's per-face fixes ("file:face" → person id,
/// 0 = nobody); they are applied after clustering. Clusters of one face are
/// not people unless named or overridden into existence.
pub fn people(shots: &[ShotAnalysis], lib: &Library, overrides: &BTreeMap<String, u32>, thr: f32) -> Vec<Person> {
    let face = |(s, i): FaceRef| &shots[s].faces[i];
    let all = shots
        .iter()
        .enumerate()
        .flat_map(|(s, a)| (0..a.faces.len()).map(move |i| (s, i)))
        .filter(|&r| !face(r).embed.is_empty() && face(r).embed_norm >= WEAK_NORM);
    let (core, weak): (Vec<FaceRef>, Vec<FaceRef>) =
        all.partition(|&r| face(r).embed_norm >= CORE_NORM && face(r).presence >= CORE_PRESENCE);
    let vecs: Vec<&[f32]> = core.iter().map(|&r| face(r).embed.as_slice()).collect();

    let mut by_id: BTreeMap<u32, Person> = BTreeMap::new();
    let mut centroids: BTreeMap<u32, Vec<f32>> = BTreeMap::new();
    let mut unnamed = 0;
    for c in cluster(&vecs, thr) {
        let members: Vec<&[f32]> = c.iter().map(|&k| vecs[k]).collect();
        let centroid = mean(&members);
        let known = lib.recognize(&centroid, thr);
        if known.is_none() && c.len() < 2 {
            continue;
        }
        let (id, name) = match known {
            Some(k) => (k.id, Some(k.name.clone())),
            None => {
                unnamed += 1;
                (UNNAMED_BASE + unnamed, None)
            }
        };
        // Two clusters recognised as the same named person become one.
        by_id.entry(id).or_insert(Person { id, name, faces: vec![] }).faces.extend(c.iter().map(|&k| core[k]));
        centroids.insert(id, centroid);
    }
    for r in weak {
        let best = centroids.iter().map(|(&id, c)| (id, dot(c, &face(r).embed))).max_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((id, _)) = best.filter(|b| b.1 > ATTACH) {
            by_id.get_mut(&id).unwrap().faces.push(r);
        }
    }

    if !overrides.is_empty() {
        let key = |(s, i): FaceRef| format!("{}:{i}", shots[s].file);
        for p in by_id.values_mut() {
            p.faces.retain(|&f| !overrides.contains_key(&key(f)));
        }
        let index: BTreeMap<&str, usize> = shots.iter().enumerate().map(|(i, a)| (a.file.as_str(), i)).collect();
        for (k, &id) in overrides {
            let Some((file, face)) = k.rsplit_once(':') else { continue };
            let (Some(&s), Ok(i)) = (index.get(file), face.parse::<usize>()) else { continue };
            if id == 0 || i >= shots[s].faces.len() {
                continue;
            }
            let name = lib.people.iter().find(|p| p.id == id).map(|p| p.name.clone());
            by_id.entry(id).or_insert(Person { id, name, faces: vec![] }).faces.push((s, i));
        }
    }

    let mut out: Vec<Person> = by_id.into_values().filter(|p| !p.faces.is_empty()).collect();
    out.iter_mut().for_each(|p| p.faces.sort());
    // Named first, then by how many photos they're in.
    out.sort_by(|a, b| b.name.is_some().cmp(&a.name.is_some()).then(b.faces.len().cmp(&a.faces.len())));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(v: &[f32]) -> Vec<f32> {
        normalized(v.to_vec())
    }

    #[test]
    fn clusters_two_people_apart() {
        let a = [unit(&[1.0, 0.1, 0.0]), unit(&[0.9, 0.0, 0.1]), unit(&[1.0, -0.1, 0.05])];
        let b = [unit(&[0.0, 1.0, 0.1]), unit(&[0.1, 0.95, 0.0])];
        let odd = unit(&[0.0, 0.0, 1.0]);
        let all: Vec<&[f32]> = [&a[0], &b[0], &a[1], &odd, &b[1], &a[2]].iter().map(|v| v.as_slice()).collect();
        assert_eq!(cluster(&all, SAME), vec![vec![0, 2, 5], vec![1, 4], vec![3]]);
    }

    #[test]
    fn library_learns_merges_and_recognizes() {
        let mut lib = Library::default();
        let a = unit(&[1.0, 0.0, 0.0]);
        let b = unit(&[0.0, 1.0, 0.0]);
        let id = lib.learn("Maya", &[&a]);
        assert_eq!(lib.learn(" maya ", &[&unit(&[0.95, 0.1, 0.0])]), id);
        assert_eq!(lib.people.len(), 1);
        assert_eq!(lib.people[0].samples.len(), 2);
        assert_eq!(lib.recognize(&a, SAME).map(|k| k.id), Some(id));
        assert!(lib.recognize(&b, SAME).is_none());
        let many: Vec<Vec<f32>> = (0..100).map(|_| a.clone()).collect();
        let refs: Vec<&[f32]> = many.iter().map(|v| v.as_slice()).collect();
        lib.learn("Maya", &refs);
        assert_eq!(lib.people[0].samples.len(), SAMPLES);

        let other = lib.learn("Maia", &[&b]);
        assert_eq!(lib.rename(other, "MAYA"), Some(id));
        assert_eq!(lib.people.len(), 1);
        assert_eq!(lib.rename(id, "Maya R."), Some(id));
        assert_eq!(lib.people[0].name, "Maya R.");
        assert_eq!(lib.rename(id, " "), None);
        assert!(lib.people.is_empty());
    }
}
