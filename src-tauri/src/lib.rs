mod core;

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Instant;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_opener::OpenerExt;

const SETTINGS_FILE: &str = "settings.json";
const INDEX_FILE: &str = "index-cache.json";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
  search_dirs: Vec<String>,
  output_dir: String,
  dry_run: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SettingsInfo {
  settings: Settings,
  settings_path: String,
  portable: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Classified {
  files: Vec<String>,
  dirs: Vec<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Progress {
  done: usize,
  total: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RunResult {
  dats: Vec<core::DatResult>,
  files_indexed: usize,
  files_cached: usize,
  dry_run: bool,
  elapsed_ms: u128,
  warnings: Vec<String>,
}

/// Fixdats passed on the command line (e.g. dropped onto the exe or a
/// shortcut) before the window was ready to receive them.
struct PendingDats(Mutex<Vec<String>>);
struct Running(AtomicBool);

/// A `settings.json` beside the exe switches the app to portable mode:
/// settings and the index cache live next to it instead of in AppData.
fn portable_dir() -> Option<PathBuf> {
  let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
  dir.join(SETTINGS_FILE).is_file().then_some(dir)
}

fn data_dir(app: &AppHandle) -> Result<PathBuf, String> {
  match portable_dir() {
    Some(dir) => Ok(dir),
    None => app.path().app_config_dir().map_err(|e| e.to_string()),
  }
}

fn load_settings(app: &AppHandle) -> Settings {
  data_dir(app)
    .ok()
    .and_then(|dir| fs::read(dir.join(SETTINGS_FILE)).ok())
    .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    .unwrap_or_default()
}

fn existing_files<I: IntoIterator<Item = String>>(args: I, cwd: &Path) -> Vec<String> {
  args
    .into_iter()
    .map(|arg| cwd.join(arg))
    .filter(|path| path.is_file())
    .map(|path| path.to_string_lossy().into_owned())
    .collect()
}

#[tauri::command]
fn get_settings(app: AppHandle) -> Result<SettingsInfo, String> {
  Ok(SettingsInfo {
    settings: load_settings(&app),
    settings_path: data_dir(&app)?.join(SETTINGS_FILE).to_string_lossy().into_owned(),
    portable: portable_dir().is_some(),
  })
}

#[tauri::command]
fn save_settings(app: AppHandle, settings: Settings) -> Result<(), String> {
  let dir = data_dir(&app)?;
  fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
  let json = serde_json::to_vec_pretty(&settings).map_err(|e| e.to_string())?;
  fs::write(dir.join(SETTINGS_FILE), json).map_err(|e| e.to_string())
}

#[tauri::command]
fn take_pending_dats(pending: State<PendingDats>) -> Vec<String> {
  std::mem::take(&mut *pending.0.lock().unwrap())
}

#[tauri::command]
fn classify_paths(paths: Vec<String>) -> Classified {
  let (dirs, files): (Vec<String>, Vec<String>) =
    paths.into_iter().partition(|p| Path::new(p).is_dir());
  Classified { files, dirs }
}

#[tauri::command]
fn clear_index(app: AppHandle) -> Result<(), String> {
  match fs::remove_file(data_dir(&app)?.join(INDEX_FILE)) {
    Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
    _ => Ok(()),
  }
}

#[tauri::command]
fn open_folder(app: AppHandle, path: String) -> Result<(), String> {
  app.opener().open_path(path, None::<&str>).map_err(|e| e.to_string())
}

#[tauri::command]
async fn run_fixdats(
  app: AppHandle,
  running: State<'_, Running>,
  paths: Vec<String>,
) -> Result<RunResult, String> {
  let settings = load_settings(&app);
  if settings.search_dirs.is_empty() {
    return Err("Add at least one search folder first.".into());
  }
  if settings.output_dir.trim().is_empty() {
    return Err("Choose an output folder first.".into());
  }
  if running.0.swap(true, Ordering::SeqCst) {
    return Err("A run is already in progress.".into());
  }

  let cache_path = data_dir(&app)?.join(INDEX_FILE);
  let handle = app.clone();
  let outcome = tauri::async_runtime::spawn_blocking(move || {
    let started = Instant::now();
    let search_dirs: Vec<PathBuf> = settings.search_dirs.iter().map(PathBuf::from).collect();
    let output_dir = PathBuf::from(&settings.output_dir);

    let index = core::build_index(&search_dirs, &cache_path, &|done, total| {
      let _ = handle.emit("fix-progress", Progress { done, total });
    });
    let dats = paths
      .iter()
      .map(|path| core::process_fixdat(Path::new(path), &index, &output_dir, settings.dry_run))
      .collect();

    RunResult {
      dats,
      files_indexed: index.files.len(),
      files_cached: index.cached,
      dry_run: settings.dry_run,
      elapsed_ms: started.elapsed().as_millis(),
      warnings: index.warnings,
    }
  })
  .await;

  running.0.store(false, Ordering::SeqCst);
  outcome.map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
  let cwd = std::env::current_dir().unwrap_or_default();
  let pending = existing_files(std::env::args().skip(1), &cwd);

  tauri::Builder::default()
    // A second launch (e.g. another fixdat dropped on the exe) hands its
    // files to the running window instead of opening a new one.
    .plugin(tauri_plugin_single_instance::init(|app, argv, cwd| {
      let dats = existing_files(argv.into_iter().skip(1), Path::new(&cwd));
      if !dats.is_empty() {
        let _ = app.emit("dats-opened", dats);
      }
      if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
      }
    }))
    .plugin(tauri_plugin_dialog::init())
    .plugin(tauri_plugin_opener::init())
    .manage(PendingDats(Mutex::new(pending)))
    .manage(Running(AtomicBool::new(false)))
    .invoke_handler(tauri::generate_handler![
      get_settings,
      save_settings,
      take_pending_dats,
      classify_paths,
      clear_index,
      open_folder,
      run_fixdats,
    ])
    .run(tauri::generate_context!())
    .expect("error while running tauri application");
}
