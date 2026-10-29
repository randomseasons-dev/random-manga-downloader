# Random Manga Downloader

A Windows manga downloader that works. Type a name, pick a source, choose settings, **DOWNLOAD**. 

## How to use

1. Click **Browse…** and choose where manga should be saved.
2. Type a manga name and press Enter. Every source is searched at once; matches are listed under each source with author, year and status.
3. Click a title to list its chapters, first to last.
4. Click a chapter to download just that one, or **DOWNLOAD ALL** for every chapter not downloaded yet.
   - Yellow dot = not downloaded · spinner = downloading (pages done/total) · green ✓ = done · red ✕ = failed (reason shown, click to retry)
5. Right-click a chapter for **Redownload** (overwrites it) or **Open book folder**.
6. **Refresh all** re-checks the files on disk against the current settings and corrects every chapter's status.

## Settings

| Setting | What it does |
|---|---|
| **Save manga as → CBZ file (whole book)** | One `Manga Title\Manga Title.cbz` with every downloaded chapter, in order. Packed when the download queue finishes; later downloads are merged in. |
| **Save manga as → Chapter Folders** | `Manga Title\0001 - Chapter 1\001.jpg` — one folder per chapter. |
| **Save manga as → One folder only** | `Manga Title\CH_0001_001.jpg` — all pages in one folder, named in reading order. |
| **Always Convert Pages to jpg files** | On by default. Every page is saved as JPG (originals are not kept; pages that are already JPG are left untouched). |

## Sources

MangaDex · WeebCentral · MangaPill · MangaKatana

Sources are website adapters in `src-tauri/src/sources.rs` and are updated by hand when a site changes.

## Build

Requires Node.js and Rust.

```
npm install
npx tauri build
```

The app and installers are written to `src-tauri/target/release/`.

## Changelog

- **1.0** — first release.

made by RandomSeasons
