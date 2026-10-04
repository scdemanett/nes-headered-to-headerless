//! Fixdat matching: index the search folders by headerless (crc, size), then
//! write verified `.unh` payloads for every fixdat entry that has a source.
//!
//! Search folders are only ever opened for reading.

use md5::Md5;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::UNIX_EPOCH;
use walkdir::WalkDir;

const INES_MAGIC: &[u8] = b"NES\x1a";
const INES_HEADER_SIZE: usize = 16;
const ROM_EXTENSIONS: [&str; 2] = ["nes", "unh"];
const INDEX_VERSION: u32 = 1;

/// (crc32, size) of a headerless payload.
type Key = (u32, u64);

#[derive(Debug, Clone)]
pub struct RomEntry {
  pub rom_name: String,
  pub size: u64,
  pub crc: u32,
  pub sha1: Option<String>,
  pub md5: Option<String>,
}

/// One ROM inside an indexed file. `name` is the archive member, or `None`
/// when the file itself is a loose ROM. `size`/`crc` describe the payload
/// with the iNES header already removed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Member {
  pub name: Option<String>,
  pub size: u64,
  pub crc: u32,
}

#[derive(Debug, Serialize, Deserialize)]
struct CachedFile {
  len: u64,
  mtime: u64,
  members: Vec<Member>,
}

#[derive(Debug, Serialize, Deserialize)]
struct IndexCache {
  version: u32,
  files: HashMap<String, CachedFile>,
}

pub struct IndexedFile {
  pub path: PathBuf,
  pub members: Vec<Member>,
}

pub struct Index {
  pub files: Vec<IndexedFile>,
  /// Files whose hashes came from the cache instead of being re-read.
  pub cached: usize,
  pub warnings: Vec<String>,
  lookup: HashMap<Key, Vec<(usize, usize)>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Written {
  pub name: String,
  pub source: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Missing {
  pub name: String,
  pub crc: String,
  pub size: u64,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatResult {
  pub fixdat: String,
  pub needed: usize,
  pub written: Vec<Written>,
  pub present: Vec<String>,
  pub missing: Vec<Missing>,
  pub problems: Vec<String>,
  pub error: Option<String>,
}

pub fn strip_ines_header(data: &[u8]) -> &[u8] {
  if data.len() >= INES_HEADER_SIZE && data.starts_with(INES_MAGIC) {
    &data[INES_HEADER_SIZE..]
  } else {
    data
  }
}

fn extension_of(name: &str) -> String {
  Path::new(name)
    .extension()
    .map(|e| e.to_string_lossy().to_ascii_lowercase())
    .unwrap_or_default()
}

fn is_rom_name(name: &str) -> bool {
  ROM_EXTENSIONS.contains(&extension_of(name).as_str())
}

fn hex(bytes: &[u8]) -> String {
  bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn source_label(path: &Path, member: &Option<String>) -> String {
  match member {
    Some(name) => format!("{}::{}", path.display(), name),
    None => path.display().to_string(),
  }
}

pub fn parse_fixdat(path: &Path) -> Result<Vec<RomEntry>, String> {
  let bytes = fs::read(path).map_err(|e| format!("Could not read fixdat: {e}"))?;
  let text = String::from_utf8_lossy(&bytes);
  let text = text.trim_start_matches('\u{feff}');
  let options = roxmltree::ParsingOptions {
    allow_dtd: true,
    ..Default::default()
  };
  let doc = roxmltree::Document::parse_with_options(text, options)
    .map_err(|e| format!("Not a valid XML dat: {e}"))?;

  let mut entries = Vec::new();
  for rom in doc.descendants().filter(|n| n.has_tag_name("rom")) {
    let (Some(name), Some(crc), Some(size)) =
      (rom.attribute("name"), rom.attribute("crc"), rom.attribute("size"))
    else {
      continue;
    };
    let (Ok(crc), Ok(size)) = (u32::from_str_radix(crc.trim(), 16), size.trim().parse::<u64>())
    else {
      continue;
    };
    let hash = |attr: &str| {
      rom
        .attribute(attr)
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty())
    };
    entries.push(RomEntry {
      rom_name: name.to_string(),
      size,
      crc,
      sha1: hash("sha1"),
      md5: hash("md5"),
    });
  }
  Ok(entries)
}

fn verify_payload(data: &[u8], entry: &RomEntry) -> bool {
  if data.len() as u64 != entry.size || crc32fast::hash(data) != entry.crc {
    return false;
  }
  if let Some(sha1) = &entry.sha1 {
    if hex(&Sha1::digest(data)) != *sha1 {
      return false;
    }
  }
  if let Some(md5) = &entry.md5 {
    if hex(&Md5::digest(data)) != *md5 {
      return false;
    }
  }
  true
}

fn member_from(name: Option<String>, raw: &[u8]) -> Option<Member> {
  let payload = strip_ines_header(raw);
  if payload.is_empty() {
    return None;
  }
  Some(Member {
    name,
    size: payload.len() as u64,
    crc: crc32fast::hash(payload),
  })
}

/// Hash every ROM in a loose file or archive.
fn scan_file(path: &Path) -> Result<Vec<Member>, String> {
  let mut members = Vec::new();
  match extension_of(&path.to_string_lossy()).as_str() {
    "zip" => {
      let file = File::open(path).map_err(|e| e.to_string())?;
      let mut archive = zip::ZipArchive::new(BufReader::new(file)).map_err(|e| e.to_string())?;
      for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        if !entry.is_file() || !is_rom_name(entry.name()) {
          continue;
        }
        let name = entry.name().to_string();
        let mut raw = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut raw).map_err(|e| e.to_string())?;
        members.extend(member_from(Some(name), &raw));
      }
    }
    "7z" => {
      let mut archive = sevenz_rust2::ArchiveReader::open(path, sevenz_rust2::Password::empty())
        .map_err(|e| e.to_string())?;
      archive
        .for_each_entries(|entry, reader| {
          // Solid archives must be read in order, so drain entries we skip.
          if entry.is_directory() || !is_rom_name(entry.name()) {
            io::copy(reader, &mut io::sink())?;
            return Ok(true);
          }
          let mut raw = Vec::with_capacity(entry.size() as usize);
          reader.read_to_end(&mut raw)?;
          members.extend(member_from(Some(entry.name().to_string()), &raw));
          Ok(true)
        })
        .map_err(|e| e.to_string())?;
    }
    _ => {
      let raw = fs::read(path).map_err(|e| e.to_string())?;
      members.extend(member_from(None, &raw));
    }
  }
  Ok(members)
}

/// Read one ROM and return its headerless payload.
fn load_payload(path: &Path, member: &Option<String>) -> Result<Vec<u8>, String> {
  let raw = match (extension_of(&path.to_string_lossy()).as_str(), member) {
    ("zip", Some(name)) => {
      let file = File::open(path).map_err(|e| e.to_string())?;
      let mut archive = zip::ZipArchive::new(BufReader::new(file)).map_err(|e| e.to_string())?;
      let mut entry = archive.by_name(name).map_err(|e| e.to_string())?;
      let mut raw = Vec::with_capacity(entry.size() as usize);
      entry.read_to_end(&mut raw).map_err(|e| e.to_string())?;
      raw
    }
    ("7z", Some(name)) => {
      let mut archive = sevenz_rust2::ArchiveReader::open(path, sevenz_rust2::Password::empty())
        .map_err(|e| e.to_string())?;
      archive.read_file(name).map_err(|e| e.to_string())?
    }
    _ => fs::read(path).map_err(|e| e.to_string())?,
  };
  Ok(strip_ines_header(&raw).to_vec())
}

fn mtime_millis(meta: &fs::Metadata) -> u64 {
  meta
    .modified()
    .ok()
    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
    .map(|d| d.as_millis() as u64)
    .unwrap_or(0)
}

fn load_cache(path: &Path) -> HashMap<String, CachedFile> {
  fs::read(path)
    .ok()
    .and_then(|bytes| serde_json::from_slice::<IndexCache>(&bytes).ok())
    .filter(|cache| cache.version == INDEX_VERSION)
    .map(|cache| cache.files)
    .unwrap_or_default()
}

fn atomic_write(path: &Path, data: &[u8]) -> io::Result<()> {
  if let Some(parent) = path.parent() {
    fs::create_dir_all(parent)?;
  }
  let file_name = path.file_name().unwrap_or_default().to_string_lossy();
  let tmp = path.with_file_name(format!(".{file_name}.tmp"));
  let mut file = File::create(&tmp)?;
  file.write_all(data)?;
  file.sync_all()?;
  drop(file);
  fs::rename(&tmp, path).inspect_err(|_| {
    let _ = fs::remove_file(&tmp);
  })
}

/// Walk the search folders and hash every ROM, reusing cached hashes for
/// files whose size and modified time are unchanged. `progress(done, total)`
/// is called from worker threads.
pub fn build_index(
  search_dirs: &[PathBuf],
  cache_path: &Path,
  progress: &(dyn Fn(usize, usize) + Sync),
) -> Index {
  let mut warnings = Vec::new();
  let mut paths: Vec<(PathBuf, u64, u64)> = Vec::new();
  for dir in search_dirs {
    if !dir.is_dir() {
      warnings.push(format!("Search folder not found: {}", dir.display()));
      continue;
    }
    for entry in WalkDir::new(dir).into_iter().filter_map(Result::ok) {
      if !entry.file_type().is_file() {
        continue;
      }
      let ext = extension_of(&entry.file_name().to_string_lossy());
      if !(ROM_EXTENSIONS.contains(&ext.as_str()) || ext == "zip" || ext == "7z") {
        continue;
      }
      if let Ok(meta) = entry.metadata() {
        paths.push((entry.into_path(), meta.len(), mtime_millis(&meta)));
      }
    }
  }
  paths.sort();
  paths.dedup_by(|a, b| a.0 == b.0);

  let cache = load_cache(cache_path);
  let total = paths.len();
  let done = AtomicUsize::new(0);
  progress(0, total);

  let scanned: Vec<(PathBuf, u64, u64, Result<Vec<Member>, String>, bool)> = paths
    .into_par_iter()
    .map(|(path, len, mtime)| {
      let hit = cache
        .get(path.to_string_lossy().as_ref())
        .filter(|c| c.len == len && c.mtime == mtime);
      let (members, cached) = match hit {
        Some(c) => (Ok(c.members.clone()), true),
        None => (scan_file(&path), false),
      };
      let n = done.fetch_add(1, Ordering::Relaxed) + 1;
      if n % 25 == 0 || n == total {
        progress(n, total);
      }
      (path, len, mtime, members, cached)
    })
    .collect();

  let mut files = Vec::with_capacity(scanned.len());
  let mut new_cache = HashMap::with_capacity(scanned.len());
  let mut cached_count = 0;
  for (path, len, mtime, members, cached) in scanned {
    match members {
      Ok(members) => {
        cached_count += cached as usize;
        new_cache.insert(
          path.to_string_lossy().into_owned(),
          CachedFile { len, mtime, members: members.clone() },
        );
        files.push(IndexedFile { path, members });
      }
      Err(e) => warnings.push(format!("Could not read {}: {e}", path.display())),
    }
  }

  let cache = IndexCache { version: INDEX_VERSION, files: new_cache };
  if let Ok(bytes) = serde_json::to_vec(&cache) {
    if let Err(e) = atomic_write(cache_path, &bytes) {
      warnings.push(format!("Could not save index cache: {e}"));
    }
  }

  let mut lookup: HashMap<Key, Vec<(usize, usize)>> = HashMap::new();
  for (fi, file) in files.iter().enumerate() {
    for (mi, member) in file.members.iter().enumerate() {
      lookup.entry((member.crc, member.size)).or_default().push((fi, mi));
    }
  }

  Index { files, cached: cached_count, warnings, lookup }
}

/// Fixdat ROM names are written flat into the output folder; refuse anything
/// that would land outside it.
fn safe_dest(output_dir: &Path, rom_name: &str) -> Option<PathBuf> {
  let rel = Path::new(rom_name);
  if rom_name.is_empty() || !rel.components().all(|c| matches!(c, Component::Normal(_))) {
    return None;
  }
  Some(output_dir.join(rel))
}

enum Outcome {
  Written(Written),
  Present(String),
  Missing(Missing),
}

fn resolve_entry(
  entry: &RomEntry,
  index: &Index,
  output_dir: &Path,
  dry_run: bool,
) -> (Outcome, Vec<String>) {
  let mut problems = Vec::new();
  let missing = || {
    Outcome::Missing(Missing {
      name: entry.rom_name.clone(),
      crc: format!("{:08x}", entry.crc),
      size: entry.size,
    })
  };

  let Some(dest) = safe_dest(output_dir, &entry.rom_name) else {
    problems.push(format!("Unsafe ROM name in fixdat, skipped: {}", entry.rom_name));
    return (missing(), problems);
  };

  if fs::metadata(&dest).is_ok_and(|m| m.len() == entry.size) {
    if let Ok(existing) = fs::read(&dest) {
      if verify_payload(&existing, entry) {
        return (Outcome::Present(entry.rom_name.clone()), problems);
      }
    }
  }

  let candidates = index.lookup.get(&(entry.crc, entry.size)).map(Vec::as_slice).unwrap_or(&[]);
  for &(fi, mi) in candidates {
    let file = &index.files[fi];
    let member = &file.members[mi].name;
    let source = source_label(&file.path, member);
    let payload = match load_payload(&file.path, member) {
      Ok(payload) => payload,
      Err(e) => {
        problems.push(format!("{source} (read failed: {e})"));
        continue;
      }
    };
    if !verify_payload(&payload, entry) {
      problems.push(format!("{source} (CRC matched, SHA1/MD5 did not)"));
      continue;
    }
    if !dry_run {
      if let Err(e) = atomic_write(&dest, &payload) {
        problems.push(format!("Could not write {}: {e}", dest.display()));
        return (missing(), problems);
      }
    }
    return (Outcome::Written(Written { name: entry.rom_name.clone(), source }), problems);
  }

  (missing(), problems)
}

pub fn process_fixdat(fixdat: &Path, index: &Index, output_dir: &Path, dry_run: bool) -> DatResult {
  let mut result = DatResult {
    fixdat: fixdat.display().to_string(),
    ..Default::default()
  };

  let mut entries = match parse_fixdat(fixdat) {
    Ok(entries) => entries,
    Err(e) => {
      result.error = Some(e);
      return result;
    }
  };
  // One output file per name, so parallel workers never share a destination.
  entries.sort_by(|a, b| a.rom_name.cmp(&b.rom_name));
  entries.dedup_by(|a, b| a.rom_name == b.rom_name);
  if entries.is_empty() {
    result.error = Some("No ROM entries found in this file.".into());
    return result;
  }
  result.needed = entries.len();

  if !dry_run {
    if let Err(e) = fs::create_dir_all(output_dir) {
      result.error = Some(format!("Could not create output folder: {e}"));
      return result;
    }
  }

  let outcomes: Vec<(Outcome, Vec<String>)> = entries
    .par_iter()
    .map(|entry| resolve_entry(entry, index, output_dir, dry_run))
    .collect();

  for (outcome, problems) in outcomes {
    result.problems.extend(problems);
    match outcome {
      Outcome::Written(w) => result.written.push(w),
      Outcome::Present(name) => result.present.push(name),
      Outcome::Missing(m) => result.missing.push(m),
    }
  }
  result
}

#[cfg(test)]
mod tests {
  use super::*;

  fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("nes-fixer-test-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
  }

  fn headered(payload: &[u8]) -> Vec<u8> {
    let mut rom = b"NES\x1a".to_vec();
    rom.extend_from_slice(&[0u8; 12]);
    rom.extend_from_slice(payload);
    rom
  }

  fn rom_xml(name: &str, payload: &[u8]) -> String {
    format!(
      r#"<game name="{name}"><rom name="{name}.unh" size="{}" crc="{:08X}" sha1="{}" md5="{}"/></game>"#,
      payload.len(),
      crc32fast::hash(payload),
      hex(&Sha1::digest(payload)),
      hex(&Md5::digest(payload)),
    )
  }

  #[test]
  fn strips_only_real_headers() {
    assert_eq!(strip_ines_header(&headered(b"abc")), b"abc");
    assert_eq!(strip_ines_header(b"plain payload, no magic"), b"plain payload, no magic");
  }

  #[test]
  fn converts_from_loose_zip_and_7z_sources() {
    let root = scratch("e2e");
    let (search_a, search_b, out) = (root.join("a"), root.join("b"), root.join("out"));
    fs::create_dir_all(search_a.join("nested")).unwrap();
    fs::create_dir_all(&search_b).unwrap();

    let loose = vec![1u8; 4096];
    let zipped = vec![2u8; 8192];
    let sevenz = vec![3u8; 2048];
    let absent = vec![4u8; 1024];

    fs::write(search_a.join("nested").join("Loose Game.nes"), headered(&loose)).unwrap();

    let mut zip = zip::ZipWriter::new(File::create(search_b.join("whatever.zip")).unwrap());
    zip
      .start_file("Wrong Name.nes", zip::write::SimpleFileOptions::default())
      .unwrap();
    zip.write_all(&headered(&zipped)).unwrap();
    zip.finish().unwrap();

    let staging = root.join("staging");
    fs::create_dir_all(&staging).unwrap();
    fs::write(staging.join("Seven.nes"), headered(&sevenz)).unwrap();
    sevenz_rust2::compress_to_path(&staging, search_b.join("seven.7z")).unwrap();

    let fixdat = root.join("fix.dat");
    fs::write(
      &fixdat,
      format!(
        "<?xml version=\"1.0\"?>\n<datafile><header><name>t</name></header>{}{}{}{}</datafile>",
        rom_xml("Loose", &loose),
        rom_xml("Zipped", &zipped),
        rom_xml("Seven", &sevenz),
        rom_xml("Absent", &absent),
      ),
    )
    .unwrap();

    let cache = root.join("index.json");
    let dirs = [search_a.clone(), search_b.clone()];
    let index = build_index(&dirs, &cache, &|_, _| {});
    assert_eq!(index.files.len(), 3);
    assert_eq!(index.cached, 0);
    assert!(index.warnings.is_empty(), "{:?}", index.warnings);

    let dry = process_fixdat(&fixdat, &index, &out, true);
    assert_eq!(dry.written.len(), 3);
    assert!(!out.exists(), "dry run must not write");

    let result = process_fixdat(&fixdat, &index, &out, false);
    assert_eq!(result.needed, 4);
    assert_eq!(result.written.len(), 3);
    assert_eq!(result.missing.len(), 1);
    assert_eq!(result.missing[0].name, "Absent.unh");
    assert!(result.problems.is_empty(), "{:?}", result.problems);
    assert_eq!(fs::read(out.join("Loose.unh")).unwrap(), loose);
    assert_eq!(fs::read(out.join("Zipped.unh")).unwrap(), zipped);
    assert_eq!(fs::read(out.join("Seven.unh")).unwrap(), sevenz);

    // Second pass: everything comes from the cache and is already present.
    let index = build_index(&dirs, &cache, &|_, _| {});
    assert_eq!(index.cached, 3);
    let again = process_fixdat(&fixdat, &index, &out, false);
    assert_eq!(again.present.len(), 3);
    assert!(again.written.is_empty());

    let _ = fs::remove_dir_all(&root);
  }

  #[test]
  fn rejects_rom_names_that_escape_the_output_folder() {
    let out = Path::new("out");
    assert!(safe_dest(out, "Game.unh").is_some());
    assert!(safe_dest(out, "../Game.unh").is_none());
    assert!(safe_dest(out, "C:\\Windows\\Game.unh").is_none());
  }
}
