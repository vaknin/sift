//! Thin Tauri layer over `sift-engine`: open a folder (analysis streams
//! progress events), then serve groups + pre-marks and store decisions.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use serde::Serialize;
use sift_engine::cull::{self, Config, EyeState, Verdict};
use sift_engine::export::{self, Darktable, Report};
use sift_engine::people::{self, Library, Person};
use sift_engine::preview;
use sift_engine::reframe::{self, Crop, KeptCrop, Ratio, ReframeConfig, Scene, Suggestion};
use sift_engine::session::{Decision, Session};
use sift_engine::{Cache, Models, ShotAnalysis, analyze_folder, cache};
use tauri::{AppHandle, Emitter, Manager, State};

struct Open {
    cache: Cache,
    shots: Vec<ShotAnalysis>,
    auto: Vec<Vec<usize>>,
    session: Session,
    /// Named people, shared by every folder.
    library: Library,
    people: Vec<Person>,
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
    /// `blink` classified with the culling thresholds (subject's right, left).
    eyes: [EyeState; 2],
    eye_sharp: [f32; 2],
    presence: f32,
    smile: f32,
    face_luma: f32,
    /// Id of the person this face belongs to, see `View::people`.
    person: Option<u32>,
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
    /// Crops kept in Reframe.
    crops: Vec<KeptCrop>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct View {
    folder: PathBuf,
    shots: Vec<ShotView>,
    /// Indices into `shots`, in capture order.
    groups: Vec<Vec<usize>>,
    /// Smallest long side a Reframe crop may have, in full-resolution pixels.
    min_long: u32,
    /// People found in the folder, named first, then by photo count.
    people: Vec<PersonView>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PersonView {
    id: u32,
    /// None until the user names them; the UI shows "Person N".
    name: Option<String>,
    /// Photos they are in.
    photos: usize,
    /// Their clearest face, aligned (112 px square).
    face: PathBuf,
}

/// A face as Reframe draws and snaps to it, normalised to the upright frame.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FaceBox {
    /// x, y, w, h.
    bbox: [f32; 4],
    /// Subject's right, left.
    eyes: [[f32; 2]; 2],
    chin: [f32; 2],
    forehead: [f32; 2],
}

/// Crop suggestions for one frame, plus what the UI needs to apply the same rules.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReframeView {
    /// Full-resolution size, upright.
    width: u32,
    height: u32,
    suggestions: Vec<Suggestion>,
    faces: Vec<FaceBox>,
    /// Space around the face box, in face heights: sides, top, bottom.
    margin: [f32; 3],
    /// Depth of the band under the chin a bottom edge shouldn't cut, in face heights.
    chin_band: f32,
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

    fn regroup_people(&mut self) {
        self.people = people::people(&self.shots, &self.library, &self.session.people, people::SAME);
    }

    fn view(&self) -> View {
        let (groups, verdicts) = self.judged();
        let mut person_of = std::collections::HashMap::new();
        for p in &self.people {
            for &f in &p.faces {
                person_of.insert(f, p.id);
            }
        }
        let shots = self
            .shots
            .iter()
            .zip(verdicts)
            .enumerate()
            .map(|(si, (s, verdict))| ShotView {
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
                    .enumerate()
                    .map(|(fi, f)| FaceView {
                        blink: f.blink,
                        eyes: f.blink.map(|b| cull::eye_state(b, &Config::default())),
                        eye_sharp: f.eye_sharp,
                        presence: f.presence,
                        smile: f.smile,
                        face_luma: f.face_luma,
                        person: person_of.get(&(si, fi)).copied(),
                    })
                    .collect(),
                crops: self.session.crops.get(&s.file).cloned().unwrap_or_default(),
            })
            .collect();
        let people = self
            .people
            .iter()
            .map(|p| {
                let mut photos: Vec<usize> = p.faces.iter().map(|f| f.0).collect();
                photos.dedup();
                let &(s, f) = p
                    .faces
                    .iter()
                    .max_by(|a, b| self.shots[a.0].faces[a.1].embed_norm.total_cmp(&self.shots[b.0].faces[b.1].embed_norm))
                    .expect("people have faces");
                PersonView {
                    id: p.id,
                    name: p.name.clone(),
                    photos: photos.len(),
                    face: self.cache.img(&cache::face_name(&self.shots[s], f)),
                }
            })
            .collect();
        View { folder: self.cache.folder.clone(), shots, groups, min_long: ReframeConfig::default().min_long, people }
    }
}

/// The face models, loaded on first use.
fn models(state: &AppState) -> Result<&Models, String> {
    match state.models.get() {
        Some(m) => Ok(m),
        None => {
            let m = Models::new().map_err(|e| format!("loading models: {e:#}"))?;
            Ok(state.models.get_or_init(|| m))
        }
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

/// Ask for a shoot folder. A GTK chooser made transient for the main window,
/// so the compositor floats it over sift: the dialog plugin sets no parent
/// on Linux, and Hyprland then tiles its chooser wherever focus happens to be.
#[tauri::command]
async fn pick_folder(window: tauri::WebviewWindow, start: Option<PathBuf>) -> Result<Option<PathBuf>, String> {
    use gtk::prelude::*;
    let (tx, rx) = std::sync::mpsc::channel();
    let w = window.clone();
    window
        .run_on_main_thread(move || {
            let parent = w.gtk_window().ok();
            let d = gtk::FileChooserDialog::with_buttons(
                Some("Open a shoot folder"),
                parent.as_ref(),
                gtk::FileChooserAction::SelectFolder,
                &[("Cancel", gtk::ResponseType::Cancel), ("Open", gtk::ResponseType::Accept)],
            );
            d.set_modal(true);
            d.set_default_response(gtk::ResponseType::Accept);
            if let Some(s) = &start {
                d.set_current_folder(s);
            }
            d.connect_response(move |d, r| {
                let _ = tx.send(if r == gtk::ResponseType::Accept { d.filename() } else { None });
                // SAFETY: the dialog is only referenced by this handler and its own signals.
                unsafe { d.destroy() };
            });
            d.show();
        })
        .map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || rx.recv().ok().flatten()).await.map_err(|e| e.to_string())
}

/// Analyse (or load from cache) a folder. Emits `progress` per shot.
#[tauri::command]
async fn open_folder(app: AppHandle, path: PathBuf) -> Result<View, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<View, String> {
        let state = app.state::<AppState>();
        let models = models(&state)?;
        let thumbs = Cache::for_folder(&path).map_err(|e| format!("{e:#}"))?;
        let on_shot = |done: usize, total: usize, s: &ShotAnalysis| {
            let thumb = s.error.is_none().then(|| thumbs.img(&cache::thumb_name(s)));
            let _ = app.emit("progress", Progress { done, total, thumb });
        };
        let (cache, shots) = analyze_folder(&path, models, &on_shot).map_err(|e| format!("{e:#}"))?;
        let auto = cull::group(&shots, &Config::default());
        let session = Session::load(&cache);
        let mut open = Open { cache, shots, auto, session, library: Library::load(), people: vec![] };
        open.regroup_people();
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

/// Name person `id` (a folder's "Person N", or rename a named one). Named
/// people are remembered for every folder; taking an existing name merges
/// into that person, and an empty name forgets a named person.
#[tauri::command]
fn name_person(state: State<'_, AppState>, id: u32, name: String) -> Result<View, String> {
    with_open(&state, |o| {
        let p = o.people.iter().find(|p| p.id == id).ok_or_else(|| anyhow::anyhow!("no person {id}"))?;
        if o.library.people.iter().any(|k| k.id == id) {
            o.library.rename(id, &name);
        } else if !name.trim().is_empty() {
            let embeds: Vec<&[f32]> = p.faces.iter().map(|&(s, f)| o.shots[s].faces[f].embed.as_slice()).collect();
            o.library.learn(&name, &embeds);
        }
        o.library.save()?;
        o.regroup_people();
        Ok(o.view())
    })
}

/// Take one face out of its person (`person` 0) or give it to a named person.
#[tauri::command]
fn assign_face(state: State<'_, AppState>, file: String, face: usize, person: u32) -> Result<View, String> {
    with_open(&state, |o| {
        o.session.people.insert(format!("{file}:{face}"), person);
        o.session.save(&o.cache)?;
        o.regroup_people();
        Ok(o.view())
    })
}

/// Crop suggestions for `file`. The scene is built on first use (a second or
/// so) and cached; the folder lock is not held meanwhile, so calls may overlap.
#[tauri::command]
async fn reframe(app: AppHandle, file: String) -> Result<ReframeView, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<ReframeView, String> {
        let state = app.state::<AppState>();
        let cache = with_open(&state, |o| Ok(Cache { dir: o.cache.dir.clone(), folder: o.cache.folder.clone() }))?;
        let models = models(&state)?;
        let err = |e: anyhow::Error| format!("{file}: {e:#}");
        let shot = preview::read_exif(&[cache.folder.join(&file)]).map_err(err)?.pop().ok_or(format!("{file}: not found"))?;
        let scene = Scene::cached(models, &shot, &cache).map_err(err)?;
        let cfg = ReframeConfig::default();
        Ok(ReframeView {
            width: scene.width,
            height: scene.height,
            suggestions: reframe::suggest(&scene, &cfg),
            faces: scene
                .faces
                .iter()
                .map(|f| FaceBox { bbox: f.bbox, eyes: f.eyes, chin: f.chin, forehead: f.forehead })
                .collect(),
            margin: cfg.margin,
            chin_band: cfg.chin_band,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Replace `file`'s kept crops, saving the session. Returns them with their ids.
#[tauri::command]
fn set_crops(state: State<'_, AppState>, file: String, crops: Vec<(Crop, Ratio)>) -> Result<Vec<KeptCrop>, String> {
    with_open(&state, |o| {
        let kept: Vec<KeptCrop> = crops.into_iter().map(|(crop, ratio)| KeptCrop { id: crop.id(), crop, ratio }).collect();
        if kept.is_empty() {
            o.session.crops.remove(&file);
        } else {
            o.session.crops.insert(file, kept.clone());
        }
        o.session.save(&o.cache)?;
        Ok(kept)
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
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            initial_folder,
            pick_folder,
            open_folder,
            set_decisions,
            regroup,
            reframe,
            set_crops,
            name_person,
            assign_face,
            send_to_darktable,
            darktable_status
        ])
        .run(tauri::generate_context!())
        .expect("error while running sift");
}
