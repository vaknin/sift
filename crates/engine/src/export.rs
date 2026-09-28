//! Sending decisions to darktable.
//!
//! Only the user's decisions are sent, never unconfirmed pre-marks:
//! reject → rating -1; pick → green label (+ stars if set); both get
//! `sift|reason|<reason>` tags. `Session::sent` remembers what sift changed
//! and what was there before, so a re-send undoes marks the user removed and
//! otherwise leaves the user's own ratings and labels alone.
//!
//! Files darktable already has in its library can't be edited through their
//! XMP (darktable's database wins), so their changes are queued for
//! `darktable/sift.lua`, which applies them through the Lua API and writes a
//! receipt back. Files darktable hasn't imported get their `.xmp` edited or
//! created in place, and darktable reads it on import.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::ShotAnalysis;
use crate::cull::{Mark, Verdict};
use crate::session::Session;
use crate::xmp::{self, Xmp};

/// darktable's colour label index for green.
const GREEN: &str = "2";
pub const TAG_PREFIX: &str = "sift|reason|";

/// What sift wants darktable to show for one file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Target {
    pub rating: Option<i8>,
    pub green: bool,
    pub tags: BTreeSet<String>,
}

/// What sift changed on one file, and what it replaced.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sent {
    /// (rating sift set, rating before).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<(i8, i8)>,
    /// Some(whether it was green before) while sift holds the green label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub green: Option<bool>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub tags: BTreeSet<String>,
}

impl Sent {
    fn is_empty(&self) -> bool {
        self.rating.is_none() && self.green.is_none() && self.tags.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RatingOp {
    Keep,
    Set(i8),
    /// Put back `to`, but only while the rating is still what sift set.
    Restore { expect: i8, to: i8 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GreenOp {
    Keep,
    Set,
    /// Give the label back: remove it unless the file was green before.
    Restore { to: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub file: String,
    pub rating: RatingOp,
    pub green: GreenOp,
    pub add: Vec<String>,
    pub remove: Vec<String>,
}

/// Rating and label a file had before a change was applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Before {
    pub rating: i8,
    pub green: bool,
}

/// What each decided file should look like in darktable.
pub fn targets(shots: &[ShotAnalysis], verdicts: &[Verdict], session: &Session) -> BTreeMap<String, Target> {
    let mut out = BTreeMap::new();
    for (s, v) in shots.iter().zip(verdicts) {
        let Some(d) = session.decisions.get(&s.file) else { continue };
        let tags = || v.reasons.iter().map(|r| format!("{TAG_PREFIX}{}", r.slug())).collect();
        let t = match d.mark {
            Mark::Reject => Target { rating: Some(-1), green: false, tags: tags() },
            Mark::Pick => Target { rating: d.stars.map(|n| n as i8), green: true, tags: tags() },
            Mark::None => Target { rating: d.stars.map(|n| n as i8), ..Target::default() },
        };
        if t != Target::default() {
            out.insert(s.file.clone(), t);
        }
    }
    out
}

/// The changes that take darktable from what was last sent to `targets`.
pub fn plan(targets: &BTreeMap<String, Target>, sent: &BTreeMap<String, Sent>) -> Vec<Change> {
    let files: BTreeSet<&String> = targets.keys().chain(sent.keys()).collect();
    let (none_t, none_s) = (Target::default(), Sent::default());
    let mut out = vec![];
    for file in files {
        let t = targets.get(file).unwrap_or(&none_t);
        let s = sent.get(file).unwrap_or(&none_s);
        let rating = match (t.rating, s.rating) {
            (Some(r), Some((had, _))) if r == had => RatingOp::Keep,
            (Some(r), _) => RatingOp::Set(r),
            (None, Some((had, before))) => RatingOp::Restore { expect: had, to: before },
            (None, None) => RatingOp::Keep,
        };
        let green = match (t.green, s.green) {
            (true, None) => GreenOp::Set,
            (false, Some(before)) => GreenOp::Restore { to: before },
            _ => GreenOp::Keep,
        };
        let c = Change {
            file: file.clone(),
            rating,
            green,
            add: t.tags.difference(&s.tags).cloned().collect(),
            remove: s.tags.difference(&t.tags).cloned().collect(),
        };
        if c.rating != RatingOp::Keep || c.green != GreenOp::Keep || !c.add.is_empty() || !c.remove.is_empty() {
            out.push(c);
        }
    }
    out
}

/// Remember an applied change.
pub fn record(sent: &mut BTreeMap<String, Sent>, c: &Change, before: Before) {
    let e = sent.entry(c.file.clone()).or_default();
    match c.rating {
        RatingOp::Keep => {}
        RatingOp::Set(r) => e.rating = Some((r, e.rating.map_or(before.rating, |(_, b)| b))),
        RatingOp::Restore { .. } => e.rating = None,
    }
    match c.green {
        GreenOp::Keep => {}
        GreenOp::Set => e.green = Some(e.green.unwrap_or(before.green)),
        GreenOp::Restore { .. } => e.green = None,
    }
    e.tags.extend(c.add.iter().cloned());
    e.tags.retain(|t| !c.remove.contains(t));
    if e.is_empty() {
        sent.remove(&c.file);
    }
}

/// Apply a change to a sidecar; returns what was there before.
pub fn apply_xmp(x: &mut Xmp, c: &Change, default_rating: i8) -> Result<Before> {
    let mut labels = x.list(xmp::LABELS)?;
    let before = Before { rating: x.rating()?.unwrap_or(default_rating), green: labels.iter().any(|l| l == GREEN) };
    match c.rating {
        RatingOp::Keep => {}
        RatingOp::Set(r) => x.set_rating(r)?,
        RatingOp::Restore { expect, to } if before.rating == expect => x.set_rating(to)?,
        RatingOp::Restore { .. } => {}
    }
    let green = match c.green {
        GreenOp::Keep => None,
        GreenOp::Set => Some(true),
        GreenOp::Restore { to } => (!to).then_some(false),
    };
    if let Some(on) = green.filter(|&on| on != before.green) {
        labels.retain(|l| l != GREEN);
        if on {
            labels.push(GREEN.into());
            labels.sort();
        }
        x.set_list(xmp::LABELS, "rdf:Seq", &labels)?;
    }
    if !c.add.is_empty() || !c.remove.is_empty() {
        if let Some(t) = c.add.iter().chain(&c.remove).find(|t| xmp::escape(t) != **t) {
            bail!("tag {t:?} needs escaping");
        }
        let mut hier = x.list(xmp::HIERARCHY)?;
        hier.retain(|t| !c.remove.contains(t));
        for t in &c.add {
            if !hier.contains(t) {
                hier.push(t.clone());
            }
        }
        hier.sort();
        // dc:subject lists every level of every tag, as darktable writes it.
        let levels = |tags: &mut dyn Iterator<Item = &String>| -> BTreeSet<String> {
            tags.flat_map(|t| t.split('|')).map(str::to_string).collect()
        };
        let keep = levels(&mut hier.iter());
        let gone = levels(&mut c.remove.iter());
        let mut subject = x.list(xmp::SUBJECT)?;
        subject.retain(|s| keep.contains(s) || !gone.contains(s));
        for s in levels(&mut c.add.iter()) {
            if !subject.contains(&s) {
                subject.push(s);
            }
        }
        subject.sort();
        x.set_list(xmp::HIERARCHY, "rdf:Bag", &hier)?;
        x.set_list(xmp::SUBJECT, "rdf:Bag", &subject)?;
    }
    Ok(before)
}

// ---- the line format shared with darktable/sift.lua ------------------------
// change:  file \t rating \t green \t add,tags \t remove,tags   ("-" = none)
// receipt: ok|missing \t before-rating \t before-green(0|1) \t <change>

impl Change {
    pub fn to_line(&self) -> String {
        let rating = match self.rating {
            RatingOp::Keep => "keep".into(),
            RatingOp::Set(r) => format!("set:{r}"),
            RatingOp::Restore { expect, to } => format!("restore:{expect}:{to}"),
        };
        let green = match self.green {
            GreenOp::Keep => "keep",
            GreenOp::Set => "set",
            GreenOp::Restore { to: false } => "restore:0",
            GreenOp::Restore { to: true } => "restore:1",
        };
        let list = |v: &[String]| if v.is_empty() { "-".to_string() } else { v.join(",") };
        format!("{}\t{rating}\t{green}\t{}\t{}", self.file, list(&self.add), list(&self.remove))
    }

    pub fn from_fields(f: &[&str]) -> Result<Self> {
        let [file, rating, green, add, remove] = f else { bail!("expected 5 fields, got {}", f.len()) };
        let num = |s: &str| s.parse::<i8>().with_context(|| format!("bad rating {s:?}"));
        let rating = match rating.split(':').collect::<Vec<_>>()[..] {
            ["keep"] => RatingOp::Keep,
            ["set", r] => RatingOp::Set(num(r)?),
            ["restore", e, t] => RatingOp::Restore { expect: num(e)?, to: num(t)? },
            _ => bail!("bad rating op {rating:?}"),
        };
        let green = match *green {
            "keep" => GreenOp::Keep,
            "set" => GreenOp::Set,
            "restore:0" => GreenOp::Restore { to: false },
            "restore:1" => GreenOp::Restore { to: true },
            _ => bail!("bad green op {green:?}"),
        };
        let list = |s: &str| if s == "-" { vec![] } else { s.split(',').map(str::to_string).collect() };
        Ok(Self { file: file.to_string(), rating, green, add: list(add), remove: list(remove) })
    }
}

/// Where darktable keeps its library and settings, and where sift queues
/// changes for sift.lua.
pub struct Darktable {
    pub library: PathBuf,
    pub queue: PathBuf,
    /// Rating darktable gives a file on import (when its XMP has none).
    pub default_rating: i8,
}

impl Darktable {
    pub fn detect() -> Result<Self> {
        let home = std::env::var_os("HOME").map(PathBuf::from).context("HOME not set")?;
        let env = |k: &str, d: &str| std::env::var_os(k).map(PathBuf::from).unwrap_or_else(|| home.join(d));
        let config = env("XDG_CONFIG_HOME", ".config").join("darktable");
        let default_rating = std::fs::read_to_string(config.join("darktablerc"))
            .ok()
            .and_then(|rc| {
                rc.lines().find_map(|l| l.strip_prefix("ui_last/import_initial_rating=")?.trim().parse().ok())
            })
            .unwrap_or(1);
        Ok(Self {
            library: config.join("library.db"),
            queue: env("XDG_CACHE_HOME", ".cache").join("sift").join("darktable"),
            default_rating,
        })
    }

    /// darktable's own spelling of `folder` and the files it has imported from it.
    pub fn imported(&self, folder: &Path) -> Result<Option<(String, BTreeSet<String>)>> {
        use rusqlite::{Connection, OpenFlags};
        if !self.library.exists() {
            return Ok(None);
        }
        let db = Connection::open_with_flags(&self.library, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
            .with_context(|| format!("opening {}", self.library.display()))?;
        db.busy_timeout(std::time::Duration::from_secs(3))?;
        let mut q = db.prepare("SELECT id, folder FROM film_rolls")?;
        let rolls: Vec<(i64, String)> = q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
        let same = |f: &str| Path::new(f) == folder || Path::new(f).canonicalize().is_ok_and(|c| c == folder);
        let Some((id, name)) = rolls.into_iter().find(|(_, f)| same(f)) else { return Ok(None) };
        let mut q = db.prepare("SELECT filename FROM images WHERE film_id = ?1")?;
        let files = q.query_map([id], |r| r.get::<_, String>(0))?.collect::<Result<_, _>>()?;
        Ok(Some((name, files)))
    }

    /// Queue file for a darktable film roll: its folder with `/` → `%`, which
    /// sift.lua can compute from `film.path` without listing the directory.
    fn queue_file(&self, roll: &str, ext: &str) -> PathBuf {
        self.queue.join(format!("{}.{ext}", roll.replace('/', "%")))
    }

    /// Changes queued for sift.lua and not applied yet.
    pub fn queued(&self, roll: &str) -> usize {
        ["todo", "applying"]
            .iter()
            .filter_map(|e| std::fs::read_to_string(self.queue_file(roll, e)).ok())
            .map(|t| t.lines().filter(|l| !l.is_empty() && !l.starts_with('#')).count())
            .sum()
    }

    /// Replace the queue for a film roll (an unapplied older queue is dropped:
    /// `changes` already covers it, because it was never recorded as sent).
    fn write_queue(&self, roll: &str, changes: &[Change]) -> Result<()> {
        let path = self.queue_file(roll, "todo");
        if changes.is_empty() {
            let _ = std::fs::remove_file(&path);
            return Ok(());
        }
        std::fs::create_dir_all(&self.queue)?;
        let tmp = path.with_extension("todo.tmp");
        let mut f = std::fs::File::create(&tmp)?;
        writeln!(f, "# sift 1 {roll}")?;
        for c in changes {
            writeln!(f, "{}", c.to_line())?;
        }
        drop(f);
        std::fs::rename(tmp, path)?;
        Ok(())
    }

    /// Read (and consume) sift.lua's receipts for a film roll into `sent`.
    /// Returns the files darktable couldn't find.
    pub fn collect(&self, roll: &str, sent: &mut BTreeMap<String, Sent>) -> Result<Vec<String>> {
        let done = self.queue_file(roll, "done");
        let reading = done.with_extension("done.reading");
        match std::fs::rename(&done, &reading) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !reading.exists() => return Ok(vec![]),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let mut missing = vec![];
        for line in std::fs::read_to_string(&reading)?.lines().filter(|l| !l.is_empty()) {
            let f: Vec<&str> = line.split('\t').collect();
            let [status, rating, green, change @ ..] = &f[..] else { bail!("bad receipt {line:?}") };
            let c = Change::from_fields(change)?;
            match *status {
                "ok" => {
                    let before = Before { rating: rating.parse()?, green: *green == "1" };
                    record(sent, &c, before);
                }
                _ => missing.push(c.file),
            }
        }
        std::fs::remove_file(reading)?;
        Ok(missing)
    }
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    /// Sidecars written for files darktable hasn't imported.
    pub written: usize,
    /// Changes waiting for sift.lua (files already in darktable).
    pub queued: usize,
    /// Files whose sidecar couldn't be edited, with the reason.
    pub failed: Vec<(String, String)>,
    /// Receipts from sift.lua for files no longer in darktable.
    pub missing: Vec<String>,
}

/// Send the session's decisions for `folder` to darktable. Saves nothing:
/// the caller saves the session (its `sent` is updated).
pub fn send(
    dt: &Darktable,
    folder: &Path,
    shots: &[ShotAnalysis],
    verdicts: &[Verdict],
    session: &mut Session,
) -> Result<Report> {
    let mut report = Report::default();
    let imported = dt.imported(folder)?;
    if let Some((roll, _)) = &imported {
        report.missing = dt.collect(roll, &mut session.sent)?;
    }
    let changes = plan(&targets(shots, verdicts, session), &session.sent);
    let in_dt = |f: &str| imported.as_ref().is_some_and(|(_, files)| files.contains(f));
    let (queued, direct): (Vec<Change>, Vec<Change>) = changes.into_iter().partition(|c| in_dt(&c.file));
    for c in &direct {
        match write_sidecar(&folder.join(format!("{}.xmp", c.file)), c, dt.default_rating) {
            Ok(before) => {
                record(&mut session.sent, c, before);
                report.written += 1;
            }
            Err(e) => report.failed.push((c.file.clone(), format!("{e:#}"))),
        }
    }
    if let Some((roll, _)) = &imported {
        dt.write_queue(roll, &queued)?;
        report.queued = queued.len();
    }
    Ok(report)
}

fn write_sidecar(path: &Path, c: &Change, default_rating: i8) -> Result<Before> {
    let mut x = match std::fs::read_to_string(path) {
        Ok(t) => Xmp::parse(t)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Xmp::new(&c.file),
        Err(e) => return Err(e.into()),
    };
    let before = apply_xmp(&mut x, c, default_rating)?;
    let tmp = path.with_extension("xmp.sift-tmp");
    std::fs::write(&tmp, &x.text)?;
    std::fs::rename(&tmp, path)?;
    Ok(before)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(rating: Option<i8>, green: bool, tags: &[&str]) -> Target {
        Target { rating, green, tags: tags.iter().map(|t| t.to_string()).collect() }
    }

    /// Send `t`, applying to `x` as if it were the file, and record it.
    fn send_to(x: &mut Xmp, t: Option<Target>, sent: &mut BTreeMap<String, Sent>) {
        let targets: BTreeMap<_, _> = t.into_iter().map(|t| ("a.CR3".to_string(), t)).collect();
        for c in plan(&targets, sent) {
            let before = apply_xmp(x, &c, 1).unwrap();
            record(sent, &c, before);
        }
    }

    #[test]
    fn resend_undoes_and_keeps_user_values() {
        let orig = include_str!("../tests/fixtures/edited.CR3.xmp");
        let mut x = Xmp::parse(orig.into()).unwrap();
        let mut sent = BTreeMap::new();

        send_to(&mut x, Some(target(Some(-1), false, &["sift|reason|eyes-closed"])), &mut sent);
        assert_eq!(x.rating().unwrap(), Some(-1));
        assert!(x.list(xmp::SUBJECT).unwrap().contains(&"eyes-closed".to_string()));
        assert_eq!(sent["a.CR3"].rating, Some((-1, 2)));

        // Re-sending the same thing changes nothing.
        let text = x.text.clone();
        send_to(&mut x, Some(target(Some(-1), false, &["sift|reason|eyes-closed"])), &mut sent);
        assert_eq!(x.text, text);

        // Reject → pick without stars: the user's 2 stars come back, green added.
        send_to(&mut x, Some(target(None, true, &["sift|reason|sharpest"])), &mut sent);
        assert_eq!(x.rating().unwrap(), Some(2));
        assert_eq!(x.list(xmp::LABELS).unwrap(), ["2"]);
        assert!(!x.list(xmp::SUBJECT).unwrap().contains(&"eyes-closed".to_string()));

        // Decision cleared: back to the original file exactly.
        send_to(&mut x, None, &mut sent);
        assert_eq!(x.text, orig);
        assert!(sent.is_empty());
    }

    #[test]
    fn undo_leaves_ratings_the_user_changed_in_darktable() {
        let mut x = Xmp::parse(include_str!("../tests/fixtures/fresh.CR3.xmp").into()).unwrap();
        let mut sent = BTreeMap::new();
        send_to(&mut x, Some(target(Some(-1), false, &[])), &mut sent);
        x.set_rating(4).unwrap(); // user re-rated it in darktable
        send_to(&mut x, None, &mut sent);
        assert_eq!(x.rating().unwrap(), Some(4));
    }

    #[test]
    fn green_the_user_already_had_is_kept() {
        let mut x = Xmp::new("a.CR3");
        x.set_list(xmp::LABELS, "rdf:Seq", &["2".into()]).unwrap();
        let mut sent = BTreeMap::new();
        send_to(&mut x, Some(target(None, true, &[])), &mut sent);
        send_to(&mut x, None, &mut sent);
        assert_eq!(x.list(xmp::LABELS).unwrap(), ["2"]);
        assert!(sent.is_empty());
    }

    #[test]
    fn line_format_round_trips() {
        let c = Change {
            file: "a b.CR3".into(),
            rating: RatingOp::Restore { expect: -1, to: 3 },
            green: GreenOp::Restore { to: false },
            add: vec!["sift|reason|smile".into(), "sift|reason|sharpest".into()],
            remove: vec![],
        };
        let line = c.to_line();
        assert_eq!(Change::from_fields(&line.split('\t').collect::<Vec<_>>()).unwrap(), c);
    }
}
