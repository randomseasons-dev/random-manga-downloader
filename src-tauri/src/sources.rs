// Source adapters. Each source supports three steps:
//   search(query)      -> list of manga hits
//   chapters(manga_id) -> chapters ordered FIRST -> LAST
//   pages(chapter_id)  -> image URLs (+ the Referer the image host needs)
//
// To add a source: add an entry to SOURCES and a match arm in the three dispatch fns.

use regex::Regex;
use reqwest::Client;
use serde::Serialize;
use serde_json::Value;

pub const SOURCES: &[(&str, &str)] = &[
    ("mangadex", "MangaDex"),
    ("weebcentral", "WeebCentral"),
    ("mangapill", "MangaPill"),
    ("mangakatana", "MangaKatana"),
];

#[derive(Serialize, Clone, Debug)]
pub struct MangaHit {
    pub id: String,
    pub title: String,
    /// empty when the source does not publish it (MangaPill, MangaKatana)
    pub author: String,
    pub info: String,
}

#[derive(Serialize, Clone, Debug)]
pub struct Chapter {
    pub id: String,
    pub name: String,
}

pub struct Pages {
    pub urls: Vec<String>,
    /// optional second address per page, tried when the first one keeps failing
    pub fallback: Vec<String>,
    pub referer: Option<String>,
}

type R<T> = Result<T, String>;

pub async fn search(c: &Client, source: &str, q: &str) -> R<Vec<MangaHit>> {
    match source {
        "mangadex" => mangadex_search(c, q).await,
        "weebcentral" => weebcentral_search(c, q).await,
        "mangapill" => mangapill_search(c, q).await,
        "mangakatana" => mangakatana_search(c, q).await,
        _ => Err(format!("unknown source {source}")),
    }
}

pub async fn chapters(c: &Client, source: &str, id: &str) -> R<Vec<Chapter>> {
    let mut list = match source {
        "mangadex" => mangadex_chapters(c, id).await?,
        "weebcentral" => weebcentral_chapters(c, id).await?,
        "mangapill" => mangapill_chapters(c, id).await?,
        "mangakatana" => mangakatana_chapters(c, id).await?,
        _ => return Err(format!("unknown source {source}")),
    };
    // drop duplicate chapter ids, keep first occurrence
    let mut seen = std::collections::HashSet::new();
    list.retain(|ch| seen.insert(ch.id.clone()));
    Ok(list)
}

pub async fn pages(c: &Client, source: &str, chapter_id: &str) -> R<Pages> {
    let p = match source {
        "mangadex" => mangadex_pages(c, chapter_id).await?,
        "weebcentral" => weebcentral_pages(c, chapter_id).await?,
        "mangapill" => mangapill_pages(c, chapter_id).await?,
        "mangakatana" => mangakatana_pages(c, chapter_id).await?,
        _ => return Err(format!("unknown source {source}")),
    };
    if p.urls.is_empty() {
        return Err("no pages found for this chapter".into());
    }
    Ok(p)
}

// ---------- helpers ----------

async fn get_text(c: &Client, url: &str) -> R<String> {
    let r = c.get(url).send().await.map_err(|e| e.to_string())?;
    if !r.status().is_success() {
        return Err(format!("HTTP {} from {url}", r.status()));
    }
    r.text().await.map_err(|e| e.to_string())
}

async fn get_json(c: &Client, url: &str) -> R<Value> {
    let r = c.get(url).send().await.map_err(|e| e.to_string())?;
    if !r.status().is_success() {
        return Err(format!("HTTP {} from {url}", r.status()));
    }
    r.json().await.map_err(|e| e.to_string())
}

fn re(p: &str) -> Regex {
    Regex::new(p).unwrap()
}

fn enc(s: &str) -> String {
    urlencoding::encode(s).into_owned()
}

pub fn decode_html(s: &str) -> String {
    let s = s
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#039;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ");
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---------- MangaDex (official JSON API) ----------

const MD_API: &str = "https://api.mangadex.org";
const MD_RATINGS: &str =
    "contentRating[]=safe&contentRating[]=suggestive&contentRating[]=erotica";

fn md_title(attr: &Value) -> String {
    let t = &attr["title"];
    if let Some(s) = t["en"].as_str() {
        return s.to_string();
    }
    if let Some(alts) = attr["altTitles"].as_array() {
        if let Some(s) = alts.iter().find_map(|a| a["en"].as_str()) {
            return s.to_string();
        }
    }
    t.as_object()
        .and_then(|o| o.values().next())
        .and_then(|v| v.as_str())
        .unwrap_or("Untitled")
        .to_string()
}

async fn mangadex_search(c: &Client, q: &str) -> R<Vec<MangaHit>> {
    let url = format!(
        "{MD_API}/manga?title={}&limit=10&order[relevance]=desc&availableTranslatedLanguage[]=en&hasAvailableChapters=true&includes[]=author&{MD_RATINGS}",
        enc(q)
    );
    let v = get_json(c, &url).await?;
    let hits = v["data"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|m| {
                    let at = &m["attributes"];
                    let year = at["year"].as_i64().map(|y| y.to_string()).unwrap_or_default();
                    let status = at["status"].as_str().unwrap_or("");
                    let author = m["relationships"]
                        .as_array()
                        .map(|rs| {
                            rs.iter()
                                .filter(|r| r["type"] == "author")
                                .filter_map(|r| r["attributes"]["name"].as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        })
                        .unwrap_or_default();
                    MangaHit {
                        id: m["id"].as_str().unwrap_or("").to_string(),
                        title: md_title(at),
                        author,
                        info: [year.as_str(), status].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" · "),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(hits)
}

async fn mangadex_chapters(c: &Client, id: &str) -> R<Vec<Chapter>> {
    let mut rows: Vec<(f64, Chapter)> = Vec::new();
    let mut seen_num = std::collections::HashSet::new();
    let mut offset = 0;
    loop {
        let url = format!(
            "{MD_API}/manga/{id}/feed?translatedLanguage[]=en&order[chapter]=asc&limit=500&offset={offset}&includeExternalUrl=0&{MD_RATINGS}"
        );
        let v = get_json(c, &url).await?;
        let data = v["data"].as_array().cloned().unwrap_or_default();
        for ch in &data {
            let at = &ch["attributes"];
            if at["pages"].as_i64().unwrap_or(0) == 0 {
                continue;
            }
            let num = at["chapter"].as_str().unwrap_or("").to_string();
            // several scanlation groups often upload the same chapter: keep the first
            if !seen_num.insert(num.clone()) {
                continue;
            }
            let title = at["title"].as_str().unwrap_or("").trim();
            let mut name = if num.is_empty() { "Oneshot".to_string() } else { format!("Chapter {num}") };
            if !title.is_empty() {
                name = format!("{name}: {title}");
            }
            let key = num.parse::<f64>().unwrap_or(f64::MAX);
            rows.push((key, Chapter { id: ch["id"].as_str().unwrap_or("").to_string(), name }));
        }
        offset += data.len();
        let total = v["total"].as_u64().unwrap_or(0) as usize;
        if data.is_empty() || offset >= total {
            break;
        }
    }
    rows.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    Ok(rows.into_iter().map(|r| r.1).collect())
}

async fn mangadex_pages(c: &Client, chapter_id: &str) -> R<Pages> {
    let v = get_json(c, &format!("{MD_API}/at-home/server/{chapter_id}")).await?;
    let base = v["baseUrl"].as_str().ok_or("MangaDex: no image server")?;
    let hash = v["chapter"]["hash"].as_str().unwrap_or("");
    let files: Vec<&str> = v["chapter"]["data"]
        .as_array()
        .map(|a| a.iter().filter_map(|f| f.as_str()).collect())
        .unwrap_or_default();
    // MangaDex@Home nodes are caches and can 404 on chapters they have not fetched yet;
    // the main uploads server always has the original
    let urls = files.iter().map(|f| format!("{base}/data/{hash}/{f}")).collect();
    let fallback = files.iter().map(|f| format!("https://uploads.mangadex.org/data/{hash}/{f}")).collect();
    Ok(Pages { urls, fallback, referer: None })
}

// ---------- WeebCentral (HTML) ----------

const WC: &str = "https://weebcentral.com";

async fn weebcentral_search(c: &Client, q: &str) -> R<Vec<MangaHit>> {
    let url = format!(
        "{WC}/search/data?text={}&sort=Best%20Match&order=Descending&official=Any&anime=Any&adult=Any&display_mode=Full%20Display",
        enc(q)
    );
    let html = get_text(c, &url).await?;
    let r = re(r#"<a href="https://weebcentral\.com/series/([A-Z0-9]+)/[^"]*" class="line-clamp-1 link link-hover">([^<]+)</a>"#);
    let year = re(r#"(?s)<strong>Year:</strong>\s*<span>(\d+)</span>"#);
    let author = re(r#"search\?author=[^"]*" class="link link-info link-hover">([^<]+)</a>"#);
    // one <article> per result; read title, year and authors inside the same block
    Ok(html
        .split(r#"<article class="bg-base-300 flex gap-4 p-4">"#)
        .skip(1)
        .filter_map(|block| {
            let m = r.captures(block)?;
            Some(MangaHit {
                id: m[1].to_string(),
                title: decode_html(&m[2]),
                author: author.captures_iter(block).map(|a| decode_html(&a[1])).collect::<Vec<_>>().join(", "),
                info: year.captures(block).map(|y| y[1].to_string()).unwrap_or_default(),
            })
        })
        .collect())
}

async fn weebcentral_chapters(c: &Client, id: &str) -> R<Vec<Chapter>> {
    let html = get_text(c, &format!("{WC}/series/{id}/full-chapter-list")).await?;
    let r = re(r#"(?s)href="/chapters/([A-Z0-9]+)".*?<span class="">([^<]+)</span>"#);
    let mut list: Vec<Chapter> = r
        .captures_iter(&html)
        .map(|m| Chapter { id: m[1].to_string(), name: decode_html(&m[2]) })
        .collect();
    list.reverse(); // site lists newest first
    Ok(list)
}

async fn weebcentral_pages(c: &Client, chapter_id: &str) -> R<Pages> {
    let html = get_text(
        c,
        &format!("{WC}/chapters/{chapter_id}/images?is_prev=False&current_page=1&reading_style=long_strip"),
    )
    .await?;
    let r = re(r#"<img\b[^>]*?\bsrc="(https://[^"]+)""#);
    let urls = r
        .captures_iter(&html)
        .map(|m| m[1].to_string())
        .filter(|u| !u.contains("/static/") && !u.contains("/cover/"))
        .collect();
    Ok(Pages { urls, fallback: vec![], referer: Some(format!("{WC}/")) })
}

// ---------- MangaPill (HTML) ----------

const MP: &str = "https://mangapill.com";

async fn mangapill_search(c: &Client, q: &str) -> R<Vec<MangaHit>> {
    let html = get_text(c, &format!("{MP}/search?q={}", enc(q))).await?;
    let r = re(r#"(?s)<a href="(/manga/\d+/[^"]+)" class="mb-2">\s*<div class="mt-3 font-black leading-tight line-clamp-2">([^<]+)</div>(.*?)</a>\s*<div class="flex flex-wrap gap-1 mt-1">\s*<div[^>]*>([^<]*)</div>\s*<div[^>]*>([^<]*)</div>"#);
    Ok(r.captures_iter(&html)
        .map(|m| MangaHit {
            id: m[1].to_string(),
            title: decode_html(&m[2]),
            author: String::new(),
            info: format!("{} · {}", m[4].trim(), m[5].trim()),
        })
        .collect())
}

async fn mangapill_chapters(c: &Client, id: &str) -> R<Vec<Chapter>> {
    let html = get_text(c, &format!("{MP}{id}")).await?;
    let r = re(r#"href="(/chapters/[^"]+)"[^>]*>([^<]+)</a>"#);
    let mut list: Vec<Chapter> = r
        .captures_iter(&html)
        .map(|m| Chapter { id: m[1].to_string(), name: decode_html(&m[2]) })
        .collect();
    list.reverse();
    Ok(list)
}

async fn mangapill_pages(c: &Client, chapter_id: &str) -> R<Pages> {
    let html = get_text(c, &format!("{MP}{chapter_id}")).await?;
    let r = re(r#"<img class="js-page" data-src="([^"]+)""#);
    let urls = r.captures_iter(&html).map(|m| m[1].to_string()).collect();
    Ok(Pages { urls, fallback: vec![], referer: Some(format!("{MP}/")) })
}

// ---------- MangaKatana (HTML) ----------

const MK: &str = "https://mangakatana.com";

async fn mangakatana_search(c: &Client, q: &str) -> R<Vec<MangaHit>> {
    let url = format!("{MK}/?search={}&search_by=book_name", enc(q));
    let resp = c.get(&url).send().await.map_err(|e| e.to_string())?;
    let final_url = resp.url().to_string();
    let html = resp.text().await.map_err(|e| e.to_string())?;
    // a search with exactly one match redirects straight to the manga page
    if final_url.contains("/manga/") {
        let h = re(r#"<h1 class="heading">([^<]+)"#);
        let title = h.captures(&html).map(|m| decode_html(&m[1])).unwrap_or_else(|| q.to_string());
        return Ok(vec![MangaHit { id: final_url, title, author: String::new(), info: String::new() }]);
    }
    let r = re(r#"(?s)<h3 class="title">\s*<a href="(https://mangakatana\.com/manga/[^"/]+)"[^>]*>([^<]+)</a>(?:<span>([^<]*)</span>)?"#);
    Ok(r.captures_iter(&html)
        .map(|m| MangaHit {
            id: m[1].to_string(),
            title: decode_html(&m[2]),
            author: String::new(),
            info: m.get(3).map(|s| s.as_str().trim_start_matches(" - ").trim().to_string()).unwrap_or_default(),
        })
        .collect())
}

async fn mangakatana_chapters(c: &Client, id: &str) -> R<Vec<Chapter>> {
    let html = get_text(c, id).await?;
    // only rows that link into THIS manga (the page also shows related titles)
    let r = re(&format!(
        r#"(?s)<div class="chapter">\s*<a href="({}/c[^"]+)">([^<]+)</a>"#,
        regex::escape(id)
    ));
    let mut list: Vec<Chapter> = r
        .captures_iter(&html)
        .map(|m| Chapter { id: m[1].to_string(), name: decode_html(&m[2]) })
        .collect();
    list.reverse();
    Ok(list)
}

async fn mangakatana_pages(c: &Client, chapter_id: &str) -> R<Pages> {
    let html = get_text(c, chapter_id).await?;
    // the page holds a decoy array with 1 image; the real one is the longest
    let arr = re(r#"var \w+=\[([^\]]*)\]"#);
    let item = re(r#"'(https?://[^']+)'"#);
    let urls = arr
        .captures_iter(&html)
        .map(|m| item.captures_iter(&m[1]).map(|u| u[1].to_string()).collect::<Vec<_>>())
        .max_by_key(|v| v.len())
        .unwrap_or_default();
    Ok(Pages { urls, fallback: vec![], referer: Some(format!("{MK}/")) })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Live smoke test of every source: search -> chapters -> pages -> first image.
    // cargo test -- --nocapture --ignored
    #[tokio::test]
    #[ignore]
    async fn live_all_sources() {
        let c = crate::make_client();
        for (key, _) in SOURCES {
            let q = if *key == "mangadex" { "oshi no ko" } else { "one piece" };
            let hits = search(&c, key, q).await.expect("search");
            assert!(!hits.is_empty(), "{key}: no hits");
            let h = &hits[0];
            let chs = chapters(&c, key, &h.id).await.expect("chapters");
            println!("{key}: '{}' by '{}' [{}] {} chapters, first='{}' last='{}'", h.title, h.author, h.info, chs.len(), chs[0].name, chs[chs.len() - 1].name);
            let p = pages(&c, key, &chs[0].id).await.expect("pages");
            let mut req = c.get(p.fallback.first().unwrap_or(&p.urls[0]));
            if let Some(r) = &p.referer {
                req = req.header("Referer", r);
            }
            let resp = req.send().await.unwrap();
            println!("    {} pages, first image HTTP {}", p.urls.len(), resp.status());
            assert!(resp.status().is_success());
        }
    }
}
