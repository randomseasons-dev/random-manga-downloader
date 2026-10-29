mod sources;

use futures::{stream, StreamExt};
use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter, State};

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0 Safari/537.36";
const PARALLEL_IMAGES: usize = 4;

static CANCEL: AtomicBool = AtomicBool::new(false);

pub fn make_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(UA)
        .timeout(Duration::from_secs(60))
        .connect_timeout(Duration::from_secs(15))
        .build()
        .expect("http client")
}

struct Http(reqwest::Client);

#[derive(Serialize)]
struct SourceResult {
    source: String,
    label: String,
    hits: Vec<sources::MangaHit>,
    error: Option<String>,
}

#[derive(Serialize, Clone)]
struct Progress {
    key: String,
    done: usize,
    total: usize,
}

/// Search every source at once; one entry per source (hits may be empty).
#[tauri::command]
async fn search(http: State<'_, Http>, query: String) -> Result<Vec<SourceResult>, String> {
    let q = query.trim().to_string();
    if q.is_empty() {
        return Ok(vec![]);
    }
    let c = &http.0;
    let jobs = sources::SOURCES.iter().map(|(key, label)| {
        let q = q.clone();
        async move {
            match sources::search(c, key, &q).await {
                Ok(hits) => SourceResult { source: key.to_string(), label: label.to_string(), hits, error: None },
                Err(e) => SourceResult { source: key.to_string(), label: label.to_string(), hits: vec![], error: Some(e) },
            }
        }
    });
    Ok(futures::future::join_all(jobs).await)
}

#[tauri::command]
async fn chapters(http: State<'_, Http>, source: String, id: String) -> Result<Vec<sources::Chapter>, String> {
    sources::chapters(&http.0, &source, &id).await
}

fn sanitize(s: &str, max: usize) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if "<>:\"/\\|?*".contains(c) || c.is_control() { '_' } else { c })
        .collect();
    let t: String = cleaned.trim().chars().take(max).collect();
    let t = t.trim_end_matches(['.', ' ']).to_string();
    if t.is_empty() { "untitled".into() } else { t }
}

fn manga_dir(dest: &str, manga: &str) -> PathBuf {
    Path::new(dest).join(sanitize(manga, 100))
}

/// "0001 - Chapter 1" : index keeps files sorted first -> last in any reader.
fn chapter_stem(index: usize, name: &str) -> String {
    format!("{:04} - {}", index + 1, sanitize(name, 110))
}

/// Save modes (the "Save manga as" dropdown):
///   book      -> <dest>/<manga>/<manga>.cbz, one CBZ for the whole book (packed after the queue ends)
///   chapters  -> <dest>/<manga>/<0001 - name>/001.jpg
///   onefolder -> <dest>/<manga>/CH_0001_001.jpg
/// "book" stages pages exactly like "onefolder" until pack_book() moves them into the CBZ.
fn ch_prefix(index: usize) -> String {
    format!("CH_{:04}_", index + 1)
}

fn book_path(dir: &Path, manga: &str) -> PathBuf {
    dir.join(format!("{}.cbz", sanitize(manga, 100)))
}

fn book_entries(book: &Path) -> Vec<String> {
    std::fs::File::open(book)
        .ok()
        .and_then(|f| zip::ZipArchive::new(f).ok())
        .map(|z| z.file_names().map(String::from).collect())
        .unwrap_or_default()
}

fn file_names(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.path().is_file())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| !n.ends_with(".part"))
                .collect()
        })
        .unwrap_or_default()
}

/// A chapter's pages satisfy the current settings: at least one page, and all JPG when `jpg` is on.
fn pages_ok<'a>(mut pages: impl Iterator<Item = &'a String>, jpg: bool) -> bool {
    let mut any = false;
    let ok = pages.all(|n| {
        any = true;
        !jpg || n.to_ascii_lowercase().ends_with(".jpg")
    });
    any && ok
}

/// For each chapter (by position): is it on disk in the layout of the CURRENT save mode,
/// with pages that match the CURRENT jpg setting?
#[tauri::command]
fn downloaded_status(dest: String, manga: String, names: Vec<String>, format: String, jpg: bool) -> Vec<bool> {
    if dest.is_empty() {
        return vec![false; names.len()];
    }
    let dir = manga_dir(&dest, &manga);
    // CH_xxxx_ pages grouped by prefix: loose files (onefolder, book staging) + book CBZ entries
    let mut flat: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    let mut add = |n: String| {
        if n.len() > 8 && n.starts_with("CH_") {
            flat.entry(n[..8].to_string()).or_default().push(n);
        }
    };
    match format.as_str() {
        "onefolder" => file_names(&dir).into_iter().for_each(&mut add),
        "book" => {
            file_names(&dir).into_iter().for_each(&mut add);
            book_entries(&book_path(&dir, &manga)).into_iter().for_each(&mut add);
        }
        _ => {}
    }
    names
        .iter()
        .enumerate()
        .map(|(i, n)| match format.as_str() {
            "chapters" => pages_ok(file_names(&dir.join(chapter_stem(i, n))).iter(), jpg),
            _ => flat.get(&ch_prefix(i)).map_or(false, |p| pages_ok(p.iter(), jpg)),
        })
        .collect()
}

fn ext_for(url: &str, content_type: &str) -> &'static str {
    let path = url.split('?').next().unwrap_or("").to_ascii_lowercase();
    for e in ["jpg", "jpeg", "png", "webp", "gif", "avif"] {
        if path.ends_with(&format!(".{e}")) {
            return match e { "jpeg" => "jpg", "png" => "png", "webp" => "webp", "gif" => "gif", "avif" => "avif", _ => "jpg" };
        }
    }
    match content_type {
        t if t.contains("png") => "png",
        t if t.contains("webp") => "webp",
        t if t.contains("gif") => "gif",
        _ => "jpg",
    }
}

/// Re-encode a page as JPG (quality 90). Already-JPG bytes pass through untouched;
/// a format we cannot decode is kept as it came rather than losing the page.
fn to_jpg(bytes: Vec<u8>, ext: &'static str) -> (Vec<u8>, &'static str) {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return (bytes, "jpg");
    }
    let Ok(img) = image::load_from_memory(&bytes) else {
        return (bytes, ext);
    };
    // flatten transparency onto white
    let rgba = img.to_rgba8();
    let mut rgb = image::RgbImage::new(rgba.width(), rgba.height());
    for (d, s) in rgb.pixels_mut().zip(rgba.pixels()) {
        let a = s[3] as u32;
        for k in 0..3 {
            d[k] = ((s[k] as u32 * a + 255 * (255 - a)) / 255) as u8;
        }
    }
    let mut out = Vec::new();
    let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90);
    match rgb.write_with_encoder(enc) {
        Ok(()) => (out, "jpg"),
        Err(_) => (bytes, ext),
    }
}

/// Try each address in turn (primary first, then fallback), a few attempts each.
async fn fetch_image(c: &reqwest::Client, urls: &[String], referer: &Option<String>) -> Result<(Vec<u8>, &'static str), String> {
    let mut last = String::new();
    for url in urls {
        for attempt in 0..3u64 {
            if CANCEL.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(1000 * attempt)).await;
            }
            let mut req = c.get(url);
            if let Some(r) = referer {
                req = req.header("Referer", r);
            }
            match req.send().await {
                Ok(resp) if resp.status().is_success() => {
                    let ct = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                    match resp.bytes().await {
                        Ok(b) if !b.is_empty() => return Ok((b.to_vec(), ext_for(url, &ct))),
                        Ok(_) => last = "empty image".into(),
                        Err(e) => last = e.to_string(),
                    }
                }
                Ok(resp) => last = format!("HTTP {}", resp.status()),
                Err(e) => last = e.to_string(),
            }
        }
    }
    Err(format!("image failed ({last}): {}", urls.last().map(String::as_str).unwrap_or("")))
}

fn with_suffix(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

fn page_names(images: &[(Vec<u8>, &str)], prefix: &str) -> Vec<String> {
    let width = images.len().to_string().len().max(3);
    images
        .iter()
        .enumerate()
        .map(|(i, (_, ext))| format!("{prefix}{:0width$}.{ext}", i + 1))
        .collect()
}

fn write_folder(path: &Path, images: &[(Vec<u8>, &str)]) -> Result<(), String> {
    let tmp = with_suffix(path, ".part");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    for ((bytes, _), n) in images.iter().zip(page_names(images, "")) {
        std::fs::write(tmp.join(n), bytes).map_err(|e| e.to_string())?;
    }
    let _ = std::fs::remove_dir_all(path);
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// Loose pages CH_xxxx_NNN.ext straight into the manga folder.
fn write_flat(dir: &Path, index: usize, images: &[(Vec<u8>, &str)]) -> Result<(), String> {
    let prefix = ch_prefix(index);
    // re-download replaces this chapter's old pages
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.filter_map(|e| e.ok()) {
            if e.file_name().to_string_lossy().starts_with(&prefix) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let names = page_names(images, &prefix);
    for ((bytes, _), n) in images.iter().zip(&names) {
        std::fs::write(dir.join(format!("{n}.part")), bytes).map_err(|e| e.to_string())?;
    }
    for n in &names {
        std::fs::rename(dir.join(format!("{n}.part")), dir.join(n)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Download one chapter in the chosen save mode.
/// Emits "progress" events {key, done, total} while pages arrive.
#[tauri::command]
async fn download_chapter(
    app: AppHandle,
    http: State<'_, Http>,
    key: String,
    source: String,
    chapter_id: String,
    manga: String,
    index: usize,
    name: String,
    dest: String,
    format: String,
    jpg: bool,
) -> Result<usize, String> {
    if dest.trim().is_empty() {
        return Err("choose a destination folder first".into());
    }
    CANCEL.store(false, Ordering::Relaxed);
    let progress = |done, total| {
        let _ = app.emit("progress", Progress { key: key.clone(), done, total });
    };
    let first = save_chapter(&http.0, &source, &chapter_id, &manga, index, &name, &dest, &format, jpg, &progress).await;
    match first {
        Err(e) if !CANCEL.load(Ordering::Relaxed) && e != "cancelled" => {
            // any source: one more try with a freshly fetched page list (links can expire, servers hiccup)
            tokio::time::sleep(Duration::from_secs(3)).await;
            save_chapter(&http.0, &source, &chapter_id, &manga, index, &name, &dest, &format, jpg, &progress).await
        }
        r => r,
    }
}

async fn save_chapter(
    c: &reqwest::Client,
    source: &str,
    chapter_id: &str,
    manga: &str,
    index: usize,
    name: &str,
    dest: &str,
    format: &str,
    jpg: bool,
    progress: &impl Fn(usize, usize),
) -> Result<usize, String> {
    let pages = sources::pages(c, source, chapter_id).await?;
    let total = pages.urls.len();
    progress(0, total);

    let referer = pages.referer.clone();
    let candidates: Vec<Vec<String>> = pages
        .urls
        .iter()
        .enumerate()
        .map(|(i, u)| std::iter::once(u.clone()).chain(pages.fallback.get(i).cloned()).collect())
        .collect();
    let mut images: Vec<(Vec<u8>, &'static str)> = Vec::with_capacity(total);
    let mut s = stream::iter(candidates)
        .map(|urls| {
            let (c, referer) = (c.clone(), referer.clone());
            async move {
                let (bytes, ext) = fetch_image(&c, &urls, &referer).await?;
                if !jpg {
                    return Ok((bytes, ext));
                }
                tauri::async_runtime::spawn_blocking(move || to_jpg(bytes, ext)).await.map_err(|e| e.to_string())
            }
        })
        .buffered(PARALLEL_IMAGES);
    while let Some(r) = s.next().await {
        images.push(r?);
        progress(images.len(), total);
    }

    let dir = manga_dir(dest, manga);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    match format {
        "chapters" => write_folder(&dir.join(chapter_stem(index, name)), &images)?,
        _ => write_flat(&dir, index, &images)?, // "onefolder", and "book" staging
    }
    Ok(total)
}

/// Move the staged CH_xxxx_ pages of a manga into <manga>.cbz (merging with what the book
/// already holds; a re-downloaded chapter replaces its old pages), then delete the loose files.
#[tauri::command]
fn pack_book(dest: String, manga: String) -> Result<usize, String> {
    let dir = manga_dir(&dest, &manga);
    let book = book_path(&dir, &manga);
    let staged: Vec<String> = std::fs::read_dir(&dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("CH_") && n.len() >= 8 && !n.ends_with(".part"))
        .collect();
    if staged.is_empty() {
        return Ok(0);
    }
    let restaged: std::collections::HashSet<&str> = staged.iter().map(|n| &n[..8]).collect();

    // (name, comes from the existing book)
    let mut old = std::fs::File::open(&book).ok().and_then(|f| zip::ZipArchive::new(f).ok());
    let mut all: Vec<(String, bool)> = old
        .as_ref()
        .map(|z| {
            z.file_names()
                .filter(|n| !(n.len() >= 8 && restaged.contains(&n[..8])))
                .map(|n| (n.to_string(), true))
                .collect()
        })
        .unwrap_or_default();
    all.extend(staged.iter().map(|n| (n.clone(), false)));
    all.sort();

    let tmp = with_suffix(&book, ".part");
    let f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
    let mut z = zip::ZipWriter::new(f);
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (n, from_book) in &all {
        if *from_book {
            let entry = old.as_mut().unwrap().by_name(n).map_err(|e| e.to_string())?;
            z.raw_copy_file(entry).map_err(|e| e.to_string())?;
        } else {
            z.start_file(n.as_str(), opts).map_err(|e| e.to_string())?;
            z.write_all(&std::fs::read(dir.join(n)).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        }
    }
    z.finish().map_err(|e| e.to_string())?;
    drop(old);
    std::fs::rename(&tmp, &book).map_err(|e| e.to_string())?;
    for n in &staged {
        let _ = std::fs::remove_file(dir.join(n));
    }
    Ok(staged.len())
}

#[tauri::command]
fn cancel_downloads() {
    CANCEL.store(true, Ordering::Relaxed);
}

/// Open the manga's folder (created if missing) in Explorer.
/// Done from Rust: the JS openPath() is blocked by the opener plugin's path scope.
#[tauri::command]
fn open_manga_folder(app: AppHandle, dest: String, manga: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let dir = manga_dir(&dest, &manga);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    app.opener().open_path(dir.to_string_lossy(), None::<&str>).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Same path the app uses: first 3 chapters (last one first) via save_chapter, then pack_book for "book".
    // MD_TEST_DIR=<folder> MD_SRC=mangadex MD_Q=phoenix MD_FMT=book MD_JPG=1 cargo test live_save -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn live_save_chapters() {
        let dest = std::env::var("MD_TEST_DIR").expect("MD_TEST_DIR");
        let src = std::env::var("MD_SRC").unwrap_or("mangadex".into());
        let q = std::env::var("MD_Q").unwrap_or("phoenix".into());
        let fmt = std::env::var("MD_FMT").unwrap_or("book".into());
        let jpg = std::env::var("MD_JPG").map(|v| v == "1").unwrap_or(true);
        let c = make_client();
        let hits = sources::search(&c, &src, &q).await.unwrap();
        println!("hit: '{}' author='{}' info='{}'", hits[0].title, hits[0].author, hits[0].info);
        let chs = sources::chapters(&c, &src, &hits[0].id).await.unwrap();
        for (i, ch) in chs.iter().take(3).enumerate().rev() {
            let r = save_chapter(&c, &src, &ch.id, &hits[0].title, i, &ch.name, &dest, &fmt, jpg, &|_, _| {}).await;
            println!("{} -> {:?}", ch.name, r);
        }
        if fmt == "book" {
            println!("packed {:?}", pack_book(dest.clone(), hits[0].title.clone()));
        }
        let names: Vec<String> = chs.iter().take(4).map(|c| c.name.clone()).collect();
        assert_eq!(downloaded_status(dest, hits[0].title.clone(), names, fmt, jpg), vec![true, true, true, false]);
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(Http(make_client()))
        .invoke_handler(tauri::generate_handler![
            search,
            chapters,
            downloaded_status,
            download_chapter,
            cancel_downloads,
            open_manga_folder,
            pack_book
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
