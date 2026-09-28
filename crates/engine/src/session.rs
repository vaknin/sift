//! The user's decisions for a folder, saved on every change so a session
//! can be resumed. Pre-marks only become decisions when the user accepts them.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::cache::Cache;
use crate::cull::Mark;
use crate::export::Sent;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub mark: Mark,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Session {
    #[serde(default)]
    pub decisions: BTreeMap<String, Decision>,
    /// Files that must start a new group.
    #[serde(default)]
    pub splits: BTreeSet<String>,
    /// Files that must stay in the previous frame's group.
    #[serde(default)]
    pub joins: BTreeSet<String>,
    /// What Send changed in darktable, per file, so a re-send can undo removed marks.
    #[serde(default)]
    pub sent: BTreeMap<String, Sent>,
}

impl Session {
    pub fn load(cache: &Cache) -> Self {
        std::fs::read(cache.dir.join("session.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, cache: &Cache) -> Result<()> {
        let tmp = cache.dir.join("session.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(tmp, cache.dir.join("session.json"))?;
        Ok(())
    }

    /// Apply manual splits/joins on top of automatic groups (capture order kept).
    pub fn regroup(&self, files: &[&str], auto: &[Vec<usize>]) -> Vec<Vec<usize>> {
        let starts: BTreeSet<usize> = auto.iter().map(|g| g[0]).collect();
        let mut out: Vec<Vec<usize>> = vec![];
        for (i, f) in files.iter().enumerate() {
            let start = i == 0 || (!self.joins.contains(*f) && (starts.contains(&i) || self.splits.contains(*f)));
            if start {
                out.push(vec![i]);
            } else {
                out.last_mut().unwrap().push(i);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regroup_applies_splits_and_joins() {
        let files = ["a", "b", "c", "d", "e"];
        let auto = vec![vec![0, 1, 2], vec![3, 4]];
        let mut s = Session::default();
        s.splits.insert("b".into());
        s.joins.insert("d".into());
        assert_eq!(s.regroup(&files, &auto), vec![vec![0], vec![1, 2, 3, 4]]);
    }

    #[test]
    fn loads_sessions_that_still_have_stars() {
        let s: Session = serde_json::from_str(r#"{"decisions":{"a":{"mark":"pick","stars":3}}}"#).unwrap();
        assert_eq!(s.decisions["a"], Decision { mark: Mark::Pick });
    }
}
