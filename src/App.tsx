import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { getVersion } from "@tauri-apps/api/app";
import "./App.css";

type Hit = { id: string; title: string; author: string; info: string };
type SourceResult = { source: string; label: string; hits: Hit[]; error: string | null };
type Chapter = { id: string; name: string };
type Selected = { source: string; label: string; hit: Hit };

type ChState = "idle" | "queued" | "downloading" | "done" | "error";
type Status = { state: ChState; done?: number; total?: number; err?: string };
type Job = {
  key: string;
  source: string;
  chapterId: string;
  manga: string;
  index: number;
  name: string;
  format: string;
  jpg: boolean;
};

// older builds stored per-chapter "cbz" / "folder"
const MODES: Record<string, string> = { book: "book", chapters: "chapters", onefolder: "onefolder", cbz: "book", folder: "chapters" };

const keyOf = (source: string, chapterId: string) => `${source}|${chapterId}`;

function load(k: string, d: string) {
  try {
    return localStorage.getItem(k) ?? d;
  } catch {
    return d;
  }
}
function save(k: string, v: string) {
  try {
    localStorage.setItem(k, v);
  } catch {
    /* ignore */
  }
}

export default function App() {
  const [query, setQuery] = useState("");
  const [searching, setSearching] = useState(false);
  const [results, setResults] = useState<SourceResult[] | null>(null);

  const [selected, setSelected] = useState<Selected | null>(null);
  const [chapters, setChapters] = useState<Chapter[]>([]);
  const [loadingCh, setLoadingCh] = useState(false);
  const [chError, setChError] = useState("");

  const [status, setStatus] = useState<Record<string, Status>>({});
  const [dest, setDest] = useState(() => load("dest", ""));
  const [format, setFormat] = useState(() => MODES[load("format", "book")] ?? "book");
  const [jpg, setJpg] = useState(() => load("jpg", "1") === "1");
  const [version, setVersion] = useState("");
  // right-click menu; index = the chapter row that was clicked (null = empty area)
  const [menu, setMenu] = useState<{ x: number; y: number; index: number | null } | null>(null);
  const [running, setRunning] = useState(false);
  const [notice, setNotice] = useState("");

  const queue = useRef<Job[]>([]);
  const runningRef = useRef(false);
  const destRef = useRef(dest);
  destRef.current = dest;

  const setOne = (key: string, s: Status) => setStatus((prev) => ({ ...prev, [key]: s }));

  useEffect(() => {
    getVersion().then(setVersion);
    // no browser context menu (Print etc.); text boxes keep theirs for copy/paste
    const noMenu = (e: MouseEvent) => {
      const t = e.target as HTMLElement;
      if (!t.closest("input, textarea")) e.preventDefault();
    };
    const close = () => setMenu(null);
    document.addEventListener("contextmenu", noMenu);
    document.addEventListener("click", close);
    window.addEventListener("blur", close);
    return () => {
      document.removeEventListener("contextmenu", noMenu);
      document.removeEventListener("click", close);
      window.removeEventListener("blur", close);
    };
  }, []);

  // live page progress from the backend
  useEffect(() => {
    const un = listen<{ key: string; done: number; total: number }>("progress", (e) => {
      const { key, done, total } = e.payload;
      setStatus((prev) =>
        prev[key]?.state === "downloading" ? { ...prev, [key]: { state: "downloading", done, total } } : prev,
      );
    });
    return () => {
      un.then((f) => f());
    };
  }, []);

  // Check what is on disk against the CURRENT save mode + jpg setting and set every
  // chapter that is not queued/downloading to done or not-downloaded accordingly.
  async function refreshStatus() {
    if (!selected || chapters.length === 0) return;
    const flags = await invoke<boolean[]>("downloaded_status", {
      dest,
      manga: selected.hit.title,
      names: chapters.map((c) => c.name),
      format,
      jpg,
    });
    setStatus((prev) => {
      const next = { ...prev };
      chapters.forEach((c, i) => {
        const k = keyOf(selected.source, c.id);
        const cur = next[k]?.state;
        if (cur === "queued" || cur === "downloading") return;
        next[k] = { state: flags[i] ? "done" : "idle" };
      });
      return next;
    });
  }

  // mark chapters already on disk when a manga opens or the destination changes
  useEffect(() => {
    refreshStatus();
  }, [selected, chapters, dest]);

  async function doSearch() {
    const q = query.trim();
    if (!q) return;
    setSearching(true);
    setResults(null);
    try {
      const r = await invoke<SourceResult[]>("search", { query: q });
      // sources with matches first, keep the original order otherwise
      r.sort((a, b) => Number(b.hits.length > 0) - Number(a.hits.length > 0));
      setResults(r);
    } catch (e) {
      setNotice(String(e));
    } finally {
      setSearching(false);
    }
  }

  async function pick(src: SourceResult, hit: Hit) {
    setSelected({ source: src.source, label: src.label, hit });
    setChapters([]);
    setChError("");
    setLoadingCh(true);
    try {
      setChapters(await invoke<Chapter[]>("chapters", { source: src.source, id: hit.id }));
    } catch (e) {
      setChError(String(e));
    } finally {
      setLoadingCh(false);
    }
  }

  async function browse() {
    const dir = await open({ directory: true, defaultPath: dest || undefined, title: "Choose download folder" });
    if (typeof dir === "string") {
      setDest(dir);
      save("dest", dir);
    }
  }

  async function runQueue() {
    if (runningRef.current) return;
    runningRef.current = true;
    setRunning(true);
    const books = new Set<string>();
    while (queue.current.length > 0) {
      const job = queue.current.shift()!;
      setOne(job.key, { state: "downloading" });
      try {
        await invoke<number>("download_chapter", {
          key: job.key,
          source: job.source,
          chapterId: job.chapterId,
          manga: job.manga,
          index: job.index,
          name: job.name,
          dest: destRef.current,
          format: job.format,
          jpg: job.jpg,
        });
        setOne(job.key, { state: "done" });
        if (job.format === "book") books.add(job.manga);
      } catch (e) {
        const msg = String(e);
        setOne(job.key, msg.includes("cancelled") ? { state: "idle" } : { state: "error", err: msg });
      }
    }
    // whole-book CBZ: pack the downloaded chapters once the queue is finished
    for (const manga of books) {
      setNotice(`Packing ${manga}.cbz…`);
      try {
        await invoke<number>("pack_book", { dest: destRef.current, manga });
        setNotice("");
      } catch (e) {
        setNotice(`Packing ${manga}.cbz failed: ${e}`);
      }
    }
    runningRef.current = false;
    setRunning(false);
  }

  // force = Redownload: also takes chapters that are already downloaded (files get overwritten)
  function enqueue(idxs: number[], force = false) {
    if (!selected) return;
    if (!dest) {
      setNotice("Choose a destination folder first.");
      return;
    }
    setNotice("");
    for (const i of idxs) {
      const c = chapters[i];
      const k = keyOf(selected.source, c.id);
      const st = status[k]?.state ?? "idle";
      if (st !== "idle" && st !== "error" && !(force && st === "done")) continue;
      if (queue.current.some((j) => j.key === k)) continue;
      queue.current.push({
        key: k,
        source: selected.source,
        chapterId: c.id,
        manga: selected.hit.title,
        index: i,
        name: c.name,
        format,
        jpg,
      });
      setOne(k, { state: "queued" });
    }
    runQueue();
  }

  function stopAll() {
    const pending = queue.current;
    queue.current = [];
    setStatus((prev) => {
      const next = { ...prev };
      pending.forEach((j) => (next[j.key] = { state: "idle" }));
      return next;
    });
    invoke("cancel_downloads");
  }

  async function openFolder() {
    setMenu(null);
    if (!selected || !dest) return;
    try {
      await invoke("open_manga_folder", { dest, manga: selected.hit.title });
    } catch (e) {
      setNotice(`Could not open the folder: ${e}`);
    }
  }

  const menuState =
    menu && menu.index !== null && selected ? (status[keyOf(selected.source, chapters[menu.index].id)]?.state ?? "idle") : null;

  const doneCount = selected
    ? chapters.filter((c) => status[keyOf(selected.source, c.id)]?.state === "done").length
    : 0;

  return (
    <div className="app">
      {/* ---------- column 1: search + settings ---------- */}
      <section className="col col1">
        <h1>Random Manga Downloader{version && ` v${version.replace(/\.0$/, "")}`}</h1>
        <form
          className="search"
          onSubmit={(e) => {
            e.preventDefault();
            doSearch();
          }}
        >
          <input
            autoFocus
            placeholder="Type a manga name…"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
          <button type="submit" disabled={searching || !query.trim()}>
            {searching ? "…" : "Search"}
          </button>
        </form>

        <div className="field">
          <label>Destination folder</label>
          <div className="dest" title={dest}>
            {dest || <span className="muted">not set</span>}
          </div>
          <button onClick={browse}>Browse…</button>
        </div>

        <div className="field">
          <label>Save manga as</label>
          <select
            value={format}
            onChange={(e) => {
              setFormat(e.target.value);
              save("format", e.target.value);
            }}
          >
            <option value="book">CBZ file (whole book)</option>
            <option value="chapters">Chapter Folders</option>
            <option value="onefolder">One folder only</option>
          </select>
        </div>

        <label className="check">
          <input
            type="checkbox"
            checked={jpg}
            onChange={(e) => {
              setJpg(e.target.checked);
              save("jpg", e.target.checked ? "1" : "0");
            }}
          />
          Always Convert Pages to jpg files
        </label>
        {notice && <div className="notice">{notice}</div>}
        <div className="foot muted">made by RandomSeasons</div>
      </section>

      {/* ---------- column 2: sources ---------- */}
      <section className="col col2">
        <h2>Sources</h2>
        <div className="scroll">
          {searching && <div className="muted pad">Searching all sources…</div>}
          {!searching && !results && <div className="muted pad">Search for a manga to see which sources have it.</div>}
          {results?.map((r) => (
            <div key={r.source} className="src">
              <div className="src-head">
                <span>{r.label}</span>
                <span className="count">{r.error ? "error" : r.hits.length}</span>
              </div>
              {r.error && <div className="src-err" title={r.error}>Source unavailable</div>}
              {!r.error && r.hits.length === 0 && <div className="src-none">no match</div>}
              {r.hits.map((h) => {
                const on = selected?.source === r.source && selected.hit.id === h.id;
                return (
                  <div key={h.id} className={"hit" + (on ? " on" : "")} onClick={() => pick(r, h)}>
                    <div className="hit-title">{h.title}</div>
                    {(h.author || h.info) && (
                      <div className="hit-info">{[h.author, h.info].filter(Boolean).join(" - ")}</div>
                    )}
                  </div>
                );
              })}
            </div>
          ))}
        </div>
      </section>

      {/* ---------- column 3: chapters ---------- */}
      <section
        className="col col3"
        onContextMenu={(e) => {
          e.preventDefault();
          setMenu({ x: e.clientX, y: e.clientY, index: null });
        }}
      >
        <h2>
          {selected ? (
            <>
              {selected.hit.title} <span className="muted">· {selected.label}</span>
            </>
          ) : (
            "Chapters"
          )}
          {chapters.length > 0 && (
            <span className="count">
              {doneCount}/{chapters.length}
            </span>
          )}
        </h2>
        <div className="scroll">
          {!selected && <div className="muted pad">Pick a source to list its chapters.</div>}
          {loadingCh && <div className="muted pad">Loading chapters…</div>}
          {chError && <div className="notice pad">{chError}</div>}
          {selected &&
            chapters.map((c, i) => {
              const st = status[keyOf(selected.source, c.id)] ?? { state: "idle" };
              return (
                <div
                  key={c.id}
                  className={"ch " + st.state}
                  onClick={() => enqueue([i])}
                  onContextMenu={(e) => {
                    e.preventDefault();
                    e.stopPropagation();
                    setMenu({ x: e.clientX, y: e.clientY, index: i });
                  }}
                  title={st.state === "done" ? "Downloaded" : st.err ?? "Click to download this chapter"}
                >
                  <StatusIcon s={st} />
                  <span className="ch-name">
                    {c.name}
                    {st.state === "error" && <span className="ch-err">{st.err} — click to retry</span>}
                  </span>
                  {st.state === "downloading" && st.total ? (
                    <span className="ch-prog">
                      {st.done}/{st.total}
                    </span>
                  ) : null}
                </div>
              );
            })}
        </div>
        <div className="bar">
          <button
            className="refresh"
            disabled={!selected || chapters.length === 0}
            onClick={refreshStatus}
            title="Re-check the chapters on disk against the current settings"
          >
            Refresh all
          </button>
          <button
            className="primary"
            disabled={!selected || chapters.length === 0 || doneCount === chapters.length}
            onClick={() => enqueue(chapters.map((_, i) => i))}
          >
            DOWNLOAD ALL
          </button>
          {running && (
            <button className="stop" onClick={stopAll}>
              STOP
            </button>
          )}
        </div>
      </section>

      {menu && (
        <div className="menu" style={{ left: menu.x, top: menu.y }} onClick={(e) => e.stopPropagation()}>
          {menu.index !== null && (
            <button
              disabled={menuState === "queued" || menuState === "downloading"}
              onClick={() => {
                const i = menu.index!;
                setMenu(null);
                enqueue([i], true);
              }}
            >
              Redownload
            </button>
          )}
          <button disabled={!selected || !dest} onClick={openFolder}>
            Open book folder
          </button>
        </div>
      )}
    </div>
  );
}

function StatusIcon({ s }: { s: Status }) {
  switch (s.state) {
    case "done":
      return <span className="st st-done">✓</span>;
    case "downloading":
      return <span className="st st-dl" />;
    case "queued":
      return <span className="st st-q">•</span>;
    case "error":
      return <span className="st st-err">✕</span>;
    default:
      return <span className="st st-idle" />;
  }
}
