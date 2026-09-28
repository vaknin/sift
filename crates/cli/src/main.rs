use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use sift_engine::calibrate::{self, Agreement, Shoot};
use sift_engine::cull::{self, Config, Mark};
use sift_engine::export::{self, Darktable};
use sift_engine::preview;
use sift_engine::reframe::{self, ReframeConfig, Scene};
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
    /// Suggest crops for one frame.
    Reframe {
        file: PathBuf,
        /// Write each suggestion as a JPEG here, plus an overview with the crops outlined.
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Analyze { dir, json } => run_analyze(&dir, json),
        Cmd::Export { dir, dry_run } => run_export(&dir, dry_run),
        Cmd::Calibrate { dirs } => run_calibrate(&dirs),
        Cmd::Reframe { file, out } => run_reframe(&file, out.as_deref()),
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

/// Outline colours for the overview, in suggestion order.
const COLOURS: [[u8; 3]; 6] = [[255, 60, 60], [60, 220, 60], [70, 140, 255], [255, 210, 0], [230, 80, 255], [255, 255, 255]];
const COLOUR_NAMES: [&str; 6] = ["red", "green", "blue", "yellow", "magenta", "white"];

fn run_reframe(file: &Path, out: Option<&Path>) -> Result<()> {
    let shot = preview::read_exif(&[file.to_path_buf()])?.pop().context("no such photo")?;
    let t0 = Instant::now();
    let img = preview::load(&shot)?;
    let models = Models::new()?;
    let t1 = Instant::now();
    let scene = Scene::build(&models, &img)?;
    let t2 = Instant::now();
    let cfg = ReframeConfig::default();
    let sugg = reframe::suggest(&scene, &cfg);
    eprintln!(
        "load {:.1?} · scene {:.1?} · suggest {:.1?} · {} face(s)",
        t1 - t0,
        t2 - t1,
        t2.elapsed(),
        scene.faces.len()
    );
    if let Some(f) = scene.faces.first() {
        eprintln!("face yaw {:+.2} · eyes y {:.3}", f.yaw, (f.eyes[0][1] + f.eyes[1][1]) / 2.0);
    }
    for (i, s) in sugg.iter().enumerate() {
        let [x, y, w, h] = s.crop.pixels(scene.width, scene.height);
        let notes: Vec<String> = s.notes.iter().map(|n| serde_json::to_string(n).unwrap().trim_matches('"').to_string()).collect();
        let t = reframe::terms(&scene, &cfg, &s.crop).unwrap_or_default();
        println!(
            "{} {:<7} {:<5} {w:>4}×{h:<4} at {x:>4},{y:<4} {:>4.1} MP {:>4} px  score {:+.2}  {}",
            i + 1,
            COLOUR_NAMES[i % 6],
            s.ratio.to_string(),
            (w * h) as f32 / 1e6,
            s.crop.long_side(scene.width, scene.height),
            s.score,
            notes.join(" ")
        );
        println!(
            "    eyes {:.3} lead {:.3} cut {:.0} bright {:.2} busy {:.2} pop {:.2} clutter {:.2}",
            t.eyes, t.lead, t.cut, t.bright, t.busy, t.pop, t.clutter
        );
    }
    let Some(out) = out else { return Ok(()) };
    std::fs::create_dir_all(out)?;
    let stem = file.file_stem().unwrap_or_default().to_string_lossy();
    let fit = |im: &image::RgbImage, long: u32| {
        let s = (long as f32 / im.width().max(im.height()) as f32).min(1.0);
        image::imageops::resize(im, (im.width() as f32 * s) as u32, (im.height() as f32 * s) as u32, image::imageops::FilterType::Triangle)
    };
    let mut overview = fit(&img, 2000);
    for (i, s) in sugg.iter().enumerate() {
        let [x, y, w, h] = s.crop.pixels(img.width(), img.height());
        let crop = image::imageops::crop_imm(&img, x, y, w, h).to_image();
        let name = format!("{stem}_{}_{}.jpg", i + 1, s.ratio.to_string().replace(':', "x"));
        fit(&crop, 1600).save(out.join(&name))?;
        let [x, y, w, h] = s.crop.pixels(overview.width(), overview.height());
        outline(&mut overview, [x, y, w, h], COLOURS[i % 6], 4 + 2 * (i % 2) as u32);
    }
    overview.save(out.join(format!("{stem}_overview.jpg")))?;
    eprintln!("wrote {} crops and an overview to {}", sugg.len(), out.display());
    Ok(())
}

fn outline(img: &mut image::RgbImage, [x, y, w, h]: [u32; 4], c: [u8; 3], t: u32) {
    let (iw, ih) = img.dimensions();
    for py in y..(y + h).min(ih) {
        for px in x..(x + w).min(iw) {
            if px < x + t || px + t >= x + w || py < y + t || py + t >= y + h {
                img.put_pixel(px, py, image::Rgb(c));
            }
        }
    }
}
