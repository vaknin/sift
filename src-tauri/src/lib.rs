//! Thin Tauri layer over `sift-engine`: open a folder (analysis streams
//! progress events), then serve groups + pre-marks and store decisions.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use serde::Serialize;
use sift_engine::cull::{self, Config, Verdict};
use sift_engine::export::{self, Darktable, Report};
use sift_engine::session::{Decision, Session};
use sift_engine::{Cache, Models, ShotAnalysis, analyze_folder, cache};
use tauri::{AppHandle, Emitter, Manager, State};

struct Open {
    cache: Cache,
    shots: Vec<ShotAnalysis>,
    auto: Vec<Vec<usize>>,
    session: Session,
}

#[derive(Default)]
struct AppState {
    models: OnceLock<Models>,
    open: Mutex<Option<Open>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FaceView {
    blink: [f32; 2],
    eye_sharp: [f32; 2],
    presence: f32,
    smile: f32,
    face_luma: f32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ShotView {
    file: String,
    time: f64,
    width: u32,
    height: u32,
    error: Option<String>,
    verdict: Verdict,
    decision: Option<Decision>,
    display: PathBuf,
    thumb: PathBuf,
    /// Full-resolution eye crops, one per face.
    eyes: Vec<PathBuf>,
    faces: Vec<FaceView>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct View {
    folder: PathBuf,
    shots: Vec<ShotView>,
    /// Indices into `shots`, in capture order.
    groups: Vec<Vec<usize>>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Progress {
    done: usize,
    total: usize,
    thumb: Option<PathBuf>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DarktableStatus {
    /// Changes waiting for the user to press "apply sift decisions".
    queued: usize,
    /// Files sift.lua reported as no longer in darktable.
    missing: Vec<String>,
}

impl Open {
    /// Groups (with the user's splits/joins) and their pre-marks.
    fn judged(&self) -> (Vec<Vec<usize>>, Vec<Verdict>) {
        let files: Vec<&str> = self.shots.iter().map(|s| s.file.as_str()).collect();
        let groups = self.session.regroup(&files, &self.auto);
        let verdicts = cull::judge(&self.shots, &groups, &Config::default());
        (groups, verdicts)
    }

    fn view(&self) -> View {
        let (groups, verdicts) = self.judged();
        let shots = self
            .shots
            .iter()
            .zip(verdicts)
            .map(|(s, verdict)| ShotView {
                file: s.file.clone(),
                time: s.time,
                width: s.width,
                height: s.height,
                error: s.error.clone(),
                verdict,
                decision: self.session.decisions.get(&s.file).copied(),
                display: self.cache.img(&cache::display_name(s)),
                thumb: self.cache.img(&cache::thumb_name(s)),
                eyes: (0..s.faces.len()).map(|i| self.cache.img(&cache::eyes_name(s, i))).collect(),
                faces: s
                    .faces
                    .iter()
                    .map(|f| FaceView {
                        blink: f.blink,
                        eye_sharp: f.eye_sharp,
                        presence: f.presence,
                        smile: f.smile,
                        face_luma: f.face_luma,
                    })
                    .collect(),
            })
            .collect();
        View { folder: self.cache.folder.clone(), shots, groups }
    }
}

fn with_open<T>(state: &AppState, f: impl FnOnce(&mut Open) -> anyhow::Result<T>) -> Result<T, String> {
    let mut open = state.open.lock().unwrap();
    let open = open.as_mut().ok_or("no folder open")?;
    f(open).map_err(|e| format!("{e:#}"))
}

/// The folder given on the command line, if any.
#[tauri::command]
fn initial_folder() -> Option<PathBuf> {
    std::env::args_os().nth(1).map(PathBuf::from).filter(|p| p.is_dir())
}

/// Analyse (or load from cache) a folder. Emits `progress` per shot.
#[tauri::command]
async fn open_folder(app: AppHandle, path: PathBuf) -> Result<View, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<View, String> {
        let state = app.state::<AppState>();
        let models = match state.models.get() {
            Some(m) => m,
            None => {
                let m = Models::new().map_err(|e| format!("loading models: {e:#}"))?;
                state.models.get_or_init(|| m)
            }
        };
        let thumbs = Cache::for_folder(&path).map_err(|e| format!("{e:#}"))?;
        let on_shot = |done: usize, total: usize, s: &ShotAnalysis| {
            let thumb = s.error.is_none().then(|| thumbs.img(&cache::thumb_name(s)));
            let _ = app.emit("progress", Progress { done, total, thumb });
        };
        let (cache, shots) = analyze_folder(&path, models, &on_shot).map_err(|e| format!("{e:#}"))?;
        let auto = cull::group(&shots, &Config::default());
        let session = Session::load(&cache);
        let open = Open { cache, shots, auto, session };
        let view = open.view();
        *state.open.lock().unwrap() = Some(open);
        Ok(view)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Set (`Some`) or clear (`None`) the decisions for several files at once,
/// saving the session immediately.
#[tauri::command]
fn set_decisions(state: State<'_, AppState>, changes: Vec<(String, Option<Decision>)>) -> Result<(), String> {
    with_open(&state, |o| {
        for (file, d) in changes {
            match d {
                Some(d) => o.session.decisions.insert(file, d),
                None => o.session.decisions.remove(&file),
            };
        }
        o.session.save(&o.cache)
    })
}

/// Start a new group at `file` (`split`), or merge `file`'s group into the
/// previous one (`!split`, with `file` first in its group). Pre-marks are
/// recomputed for the new groups.
#[tauri::command]
fn regroup(state: State<'_, AppState>, file: String, split: bool) -> Result<View, String> {
    with_open(&state, |o| {
        // Undo a manual edit when that alone gives the wanted grouping, so
        // the session only records departures from the automatic groups.
        let auto_start = o.auto.iter().any(|g| o.shots[g[0]].file == file);
        let s = &mut o.session;
        if split {
            if !(s.joins.remove(&file) && auto_start) {
                s.splits.insert(file);
            }
        } else if !(s.splits.remove(&file) && !auto_start) {
            s.joins.insert(file);
        }
        o.session.save(&o.cache)?;
        Ok(o.view())
    })
}

/// Send the confirmed decisions to darktable: sidecars for files it hasn't
/// imported, a queue for sift.lua for the rest.
#[tauri::command]
async fn send_to_darktable(app: AppHandle) -> Result<Report, String> {
    tauri::async_runtime::spawn_blocking(move || {
        with_open(&app.state::<AppState>(), |o| {
            let (_, verdicts) = o.judged();
            let report = export::send(&Darktable::detect()?, &o.cache.folder, &o.shots, &verdicts, &mut o.session);
            // Sidecars already written are recorded in `sent` even when the
            // queue then fails, so save either way.
            o.session.save(&o.cache)?;
            report
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Pick up sift.lua's receipts and report what is still waiting in darktable.
#[tauri::command]
fn darktable_status(state: State<'_, AppState>) -> Result<DarktableStatus, String> {
    with_open(&state, |o| {
        let dt = Darktable::detect()?;
        let Some((roll, _)) = dt.imported(&o.cache.folder)? else {
            return Ok(DarktableStatus { queued: 0, missing: vec![] });
        };
        let before = o.session.sent.clone();
        let missing = dt.collect(&roll, &mut o.session.sent)?;
        if o.session.sent != before {
            o.session.save(&o.cache)?;
        }
        Ok(DarktableStatus { queued: dt.queued(&roll), missing })
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            initial_folder,
            open_folder,
            set_decisions,
            regroup,
            send_to_darktable,
            darktable_status
        ])
        .run(tauri::generate_context!())
        .expect("error while running sift");
}
