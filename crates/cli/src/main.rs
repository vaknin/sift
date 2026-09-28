use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Result;
use clap::{Parser, Subcommand};
use sift_engine::calibrate::{self, Agreement, Shoot};
use sift_engine::cull::{self, Config, Mark};
use sift_engine::export::{self, Darktable};
use sift_engine::session::Session;
use sift_engine::{Models, analyze_folder};

#[derive(Parser)]
#[command(about = "Portrait culling helper")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Analyse a folder (cached) and print groups with pre-marks.
    Analyze {
        dir: PathBuf,
        /// Print the full analysis as JSON instead.
        #[arg(long)]
        json: bool,
    },
    /// Send the folder's confirmed decisions to darktable.
    Export {
        dir: PathBuf,
        /// Only print what would change and where it would go.
        #[arg(long)]
        dry_run: bool,
    },
    /// Compare pre-marks with the decisions made in sift and suggest thresholds.
    Calibrate {
        #[arg(required = true)]
        dirs: Vec<PathBuf>,
    },
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Analyze { dir, json } => run_analyze(&dir, json),
        Cmd::Export { dir, dry_run } => run_export(&dir, dry_run),
        Cmd::Calibrate { dirs } => run_calibrate(&dirs),
    }
}

fn run_analyze(dir: &Path, json: bool) -> Result<()> {
    let t0 = Instant::now();
    let models = Models::new()?;
    let (cache, shots) = analyze_folder(dir, &models, &|done, total, _| {
        if done % 10 == 0 || done == total {
            eprint!("\r{done}/{total}");
        }
    })?;
    eprintln!("\ranalysed {} photos in {:.1?} · cache {}", shots.len(), t0.elapsed(), cache.dir.display());
    if json {
        println!("{}", serde_json::to_string_pretty(&shots)?);
        return Ok(());
    }
    let cfg = Config::default();
    let session = Session::load(&cache);
    let files: Vec<&str> = shots.iter().map(|s| s.file.as_str()).collect();
    let groups = session.regroup(&files, &cull::group(&shots, &cfg));
    let verdicts = cull::judge(&shots, &groups, &cfg);
    let (mut picks, mut rejects) = (0, 0);
    for (gi, g) in groups.iter().enumerate() {
        println!("group {} ({} frames)", gi + 1, g.len());
        for &i in g {
            let v = &verdicts[i];
            let m = match v.mark {
                Mark::Pick => { picks += 1; "★" }
                Mark::Reject => { rejects += 1; "✗" }
                Mark::None => "·",
            };
            let reasons: Vec<String> = v.reasons.iter().map(|r| serde_json::to_string(r).unwrap().trim_matches('"').to_string()).collect();
            println!("  {m} {:<28} score {:.2} sharp {:.2}  {}", shots[i].file, v.score, v.rel_sharp, reasons.join(" "));
        }
    }
    println!("{} groups · {picks} picks · {rejects} rejects · {} photos", groups.len(), shots.len());
    Ok(())
}

fn run_export(dir: &Path, dry_run: bool) -> Result<()> {
    let models = Models::new()?;
    let (cache, shots) = analyze_folder(dir, &models, &|_, _, _| {})?;
    let cfg = Config::default();
    let mut session = Session::load(&cache);
    let files: Vec<&str> = shots.iter().map(|s| s.file.as_str()).collect();
    let groups = session.regroup(&files, &cull::group(&shots, &cfg));
    let verdicts = cull::judge(&shots, &groups, &cfg);
    let dt = Darktable::detect()?;
    if dry_run {
        let imported = dt.imported(&cache.folder)?;
        let changes = export::plan(&export::targets(&shots, &verdicts, &session), &session.sent);
        for c in &changes {
            let via = if imported.as_ref().is_some_and(|(_, f)| f.contains(&c.file)) { "darktable" } else { "xmp" };
            println!("{via:<9} {}", c.to_line());
        }
        println!(
            "{} changes · darktable film roll: {}",
            changes.len(),
            imported.map_or("none".into(), |(roll, f)| format!("{roll} ({} files)", f.len()))
        );
        return Ok(());
    }
    let report = export::send(&dt, &cache.folder, &shots, &verdicts, &mut session)?;
    session.save(&cache)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn run_calibrate(dirs: &[PathBuf]) -> Result<()> {
    let models = Models::new()?;
    let cfg = Config::default();
    let mut loaded = vec![];
    for dir in dirs {
        let (cache, shots) = analyze_folder(dir, &models, &|_, _, _| {})?;
        let session = Session::load(&cache);
        eprintln!("{}: {} of {} frames decided", dir.display(), session.decisions.len(), shots.len());
        loaded.push((shots, session));
    }
    let shoots: Vec<Shoot> = loaded
        .iter()
        .map(|(shots, session)| {
            let files: Vec<&str> = shots.iter().map(|s| s.file.as_str()).collect();
            let groups = session.regroup(&files, &cull::group(shots, &cfg));
            Shoot { shots, groups, decisions: &session.decisions }
        })
        .collect();
    let r = calibrate::calibrate(&shoots, &cfg);
    if r.decided == 0 {
        println!("No decisions yet: cull the folder in sift first.");
        return Ok(());
    }

    let pct = |x: Option<f32>| x.map_or("  –".into(), |x| format!("{:3.0}%", x * 100.0));
    let row = |name: &str, a: &Agreement| {
        println!(
            "{name:<8} precision {} ({}/{})   recall {} ({}/{})",
            pct(a.precision()),
            a.both,
            a.both + a.only_pre,
            pct(a.recall()),
            a.both,
            a.both + a.only_user
        )
    };
    println!("\n{} decided frames", r.decided);
    row("rejects", &r.reject);
    row("picks", &r.pick);

    println!("\n{:<12} {:>7} {:>8} {:>6}", "reason", "flagged", "rejected", "picked");
    for (reason, s) in &r.reasons {
        println!("{reason:<12} {:>7} {:>8} {:>6}", s.flagged, s.rejected, s.picked);
    }

    if !r.missed.is_empty() {
        println!("\nrejected by you, not pre-marked:");
        for m in &r.missed {
            let blink = m.blink.map_or("no face".into(), |[a, b]| format!("blink {a:.2}/{b:.2}"));
            let reasons: Vec<String> = m.reasons.iter().map(|r| r.slug()).collect();
            let dir = dirs[m.shoot].file_name().unwrap_or_default().to_string_lossy();
            let file = if dirs.len() > 1 { format!("{dir}/{}", m.file) } else { m.file.clone() };
            println!("  {file:<36} {blink:<16} sharpness {:.2} of best  {}", m.rel_sharp, reasons.join(" "));
        }
    }

    let before = r.reject.errors();
    if r.suggestions.is_empty() {
        println!("\nNo threshold change would reduce the {before} reject disagreement(s).");
    } else {
        println!("\nSuggested (cull.rs Config), reject disagreements {before} → {}:", r.reject_errors_after);
        for s in &r.suggestions {
            println!("  {:<13} {} → {}", s.param, s.current, s.suggested);
        }
    }
    if r.decided < 50 || r.reject.both + r.reject.only_user < 10 {
        println!("\nFew decisions so far: treat suggestions as hints; confirm over more shoots.");
    }
    Ok(())
}
