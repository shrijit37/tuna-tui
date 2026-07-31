//! YouTube Music InnerTube client — unauthenticated `WEB_REMIX` calls for
//! music search, radio and square album art. All network legs degrade to
//! `None`; callers fall back to the yt-dlp seam (graceful offline).

use crate::httpcache::blocking_client;
use crate::providers::contracts::{AlbumRef, ArtistRef, Song, Thumbnail};
use crate::yt::YtVideo;

const SEARCH_URL: &str = "https://music.youtube.com/youtubei/v1/search?prettyPrint=false";
const PLAYER_URL: &str = "https://music.youtube.com/youtubei/v1/player?prettyPrint=false";
const NEXT_URL: &str = "https://music.youtube.com/youtubei/v1/next?prettyPrint=false";
const BROWSE_URL: &str = "https://music.youtube.com/youtubei/v1/browse?prettyPrint=false";
const CLIENT_NAME: &str = "WEB_REMIX";
const CLIENT_VERSION: &str = "1.20260821.01.00";

// ---------------------------------------------------------------------------
// tiny helpers

/// `googleusercontent.com/...=w60-h60-l90-rj` → `...=w544-h544-l90-rj`
/// Non-YouTube-Music URLs (e.g. `i.ytimg.com`) pass through unchanged.
pub fn normalize_thumbnail_url(url: &str) -> String {
    if !url.contains("googleusercontent.com") {
        return url.to_string();
    }
    if let Some(eq) = url.rfind('=') {
        let suffix = &url[eq..];
        if suffix.starts_with("=w") || suffix.starts_with("=s") {
            return format!("{}=w544-h544-l90-rj", &url[..eq]);
        }
    }
    url.to_string()
}

fn innertube_context() -> serde_json::Value {
    serde_json::json!({
        "client": {
            "clientName": CLIENT_NAME,
            "clientVersion": CLIENT_VERSION,
            "hl": "en",
            "gl": "US"
        }
    })
}

fn post(url: &str, body: serde_json::Value) -> Option<serde_json::Value> {
    let resp = blocking_client().post(url).json(&body).send().ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json::<serde_json::Value>().ok()
}

fn parse_duration_to_ms(s: &str) -> Option<u32> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let parts: Vec<&str> = s.split(':').collect();
    if parts.is_empty()
        || parts
            .iter()
            .any(|p| p.is_empty() || !p.chars().all(|c| c.is_ascii_digit()))
    {
        return None;
    }
    let mut ms: u64 = 0;
    let mut mul: u64 = 1;
    for p in parts.iter().rev() {
        let v: u64 = p.parse().ok()?;
        ms = ms.checked_add(v * mul * 1000)?;
        mul = mul.checked_mul(60)?;
    }
    u32::try_from(ms).ok()
}

fn split_artists(s: &str) -> Vec<String> {
    // "A, B & C" → ["A","B","C"]
    let norm = s.replace(" & ", ", ").replace(" , ", ", ");
    norm.split(',')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

// ---------------------------------------------------------------------------
// thumbnail extractors

fn thumbnail_from_value(v: &serde_json::Value) -> Option<String> {
    // Known InnerTube shapes:
    //  search:  thumbnail.musicThumbnailRenderer.thumbnail.thumbnails[]
    //  radio:   thumbnail.thumbnails[]  /  thumbnail.musicThumbnailRenderer.thumbnail.thumbnails[]
    let candidates: Vec<Option<&serde_json::Value>> = vec![
        v.get("thumbnail")
            .and_then(|t| t.get("musicThumbnailRenderer"))
            .and_then(|m| m.get("thumbnail"))
            .and_then(|t| t.get("thumbnails")),
        v.get("thumbnail").and_then(|t| t.get("thumbnails")),
        v.get("thumbnailRenderer")
            .and_then(|t| t.get("musicThumbnailRenderer"))
            .and_then(|m| m.get("thumbnail"))
            .and_then(|t| t.get("thumbnails")),
    ];
    for cand in candidates.into_iter().flatten() {
        if let Some(arr) = cand.as_array() {
            // Prefer googleusercontent URLs (square YTM art) over generic ones
            if let Some(url) = arr
                .iter()
                .rev()
                .filter_map(|e| e.get("url").and_then(|u| u.as_str()))
                .find(|u| u.contains("googleusercontent.com"))
                .or_else(|| {
                    arr.last()
                        .and_then(|e| e.get("url").and_then(|u| u.as_str()))
                })
            {
                return Some(normalize_thumbnail_url(url));
            }
        }
    }
    // fallback deep-scan for any "thumbnails" array
    deep_thumbnail(v).map(|u| normalize_thumbnail_url(&u))
}

fn deep_thumbnail(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::Object(map) => {
            if let Some(arr) = map.get("thumbnails").and_then(|a| a.as_array()) {
                if let Some(last) = arr
                    .last()
                    .and_then(|e| e.get("url"))
                    .and_then(|u| u.as_str())
                {
                    return Some(last.to_string());
                }
            }
            for child in map.values() {
                if let Some(u) = deep_thumbnail(child) {
                    return Some(u);
                }
            }
            None
        }
        serde_json::Value::Array(arr) => {
            for e in arr {
                if let Some(u) = deep_thumbnail(e) {
                    return Some(u);
                }
            }
            None
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// videoId / title extraction for search results

fn extract_video_id(item: &serde_json::Value) -> Option<String> {
    if let Some(id) = item
        .get("playlistItemData")
        .and_then(|p| p.get("videoId"))
        .and_then(|v| v.as_str())
    {
        if !id.is_empty() {
            return Some(id.to_string());
        }
    }
    if let Some(id) = item
        .get("overlay")
        .and_then(|o| o.get("musicItemThumbnailOverlayRenderer"))
        .and_then(|c| c.get("content"))
        .and_then(|c| c.get("musicPlayButtonRenderer"))
        .and_then(|p| p.get("playNavigationEndpoint"))
        .and_then(|e| e.get("watchEndpoint"))
        .and_then(|w| w.get("videoId"))
        .and_then(|v| v.as_str())
    {
        if !id.is_empty() {
            return Some(id.to_string());
        }
    }
    if let Some(id) = item
        .get("navigationEndpoint")
        .and_then(|n| n.get("watchEndpoint"))
        .and_then(|w| w.get("videoId"))
        .and_then(|v| v.as_str())
    {
        if !id.is_empty() {
            return Some(id.to_string());
        }
    }
    if let Some(id) = item
        .get("onTap")
        .and_then(|n| n.get("watchEndpoint"))
        .and_then(|w| w.get("videoId"))
        .and_then(|v| v.as_str())
    {
        if !id.is_empty() {
            return Some(id.to_string());
        }
    }
    // deep fallback: first videoId string that looks like an 11-char YouTube id
    deep_video_id(item)
}

fn deep_video_id(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::Object(map) => {
            if let Some(id) = map.get("videoId").and_then(|x| x.as_str()) {
                if id.len() == 11 && !id.is_empty() {
                    return Some(id.to_string());
                }
            }
            for child in map.values() {
                if let Some(id) = deep_video_id(child) {
                    return Some(id);
                }
            }
            None
        }
        serde_json::Value::Array(arr) => {
            for e in arr {
                if let Some(id) = deep_video_id(e) {
                    return Some(id);
                }
            }
            None
        }
        _ => None,
    }
}

fn extract_title(item: &serde_json::Value) -> Option<String> {
    // flexColumns[0].musicResponsiveListItemFlexColumnRenderer.text.runs[0].text
    let flex = item.get("flexColumns")?.as_array()?;
    let col = flex
        .first()?
        .get("musicResponsiveListItemFlexColumnRenderer")?;
    let runs = col.get("text")?.get("runs")?.as_array()?;
    let t = runs.first()?.get("text")?.as_str()?;
    if t.trim().is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

fn parse_subtitle(item: &serde_json::Value) -> (Vec<String>, Option<String>, Option<u32>) {
    let Some(flex) = item.get("flexColumns").and_then(|v| v.as_array()) else {
        return (Vec::new(), None, None);
    };
    if flex.len() < 2 {
        return (Vec::new(), None, None);
    }
    let Some(col) = flex[1].get("musicResponsiveListItemFlexColumnRenderer") else {
        return (Vec::new(), None, None);
    };
    let Some(runs) = col
        .get("text")
        .and_then(|t| t.get("runs"))
        .and_then(|r| r.as_array())
    else {
        return (Vec::new(), None, None);
    };
    // split runs by " • "
    let mut chunks: Vec<Vec<String>> = Vec::new();
    let mut cur: Vec<String> = Vec::new();
    for r in runs {
        let t = r.get("text").and_then(|v| v.as_str()).unwrap_or("");
        if t == " • " {
            if !cur.is_empty() {
                chunks.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(t.to_string());
        }
    }
    if !cur.is_empty() {
        chunks.push(cur);
    }
    if chunks.is_empty() {
        return (Vec::new(), None, None);
    }
    // strip leading badge "Song"/"Video"
    if chunks[0].len() == 1 && matches!(chunks[0][0].as_str(), "Song" | "Video" | "Episode") {
        chunks.remove(0);
        if chunks.is_empty() {
            return (Vec::new(), None, None);
        }
    }
    // last chunk may be duration
    let mut duration: Option<u32> = None;
    if let Some(last) = chunks.last() {
        let joined = last.join("");
        if let Some(ms) = parse_duration_to_ms(&joined) {
            duration = Some(ms);
            chunks.pop();
        }
    }
    let mut artists: Vec<String> = Vec::new();
    let mut album: Option<String> = None;
    if !chunks.is_empty() {
        let joined = chunks.remove(0).join("");
        artists = split_artists(&joined);
    }
    if !chunks.is_empty() {
        let joined = chunks.remove(0).join("");
        if !joined.trim().is_empty() {
            album = Some(joined);
        }
    }
    (artists, album, duration)
}

// ---------------------------------------------------------------------------
// deep collection helpers for search / radio

fn collect_mrlir<'a>(v: &'a serde_json::Value, out: &mut Vec<&'a serde_json::Value>) {
    match v {
        serde_json::Value::Object(map) => {
            if let Some(item) = map.get("musicResponsiveListItemRenderer") {
                out.push(item);
            }
            for child in map.values() {
                collect_mrlir(child, out);
            }
        }
        serde_json::Value::Array(arr) => {
            for e in arr {
                collect_mrlir(e, out);
            }
        }
        _ => {}
    }
}

fn collect_radio_items<'a>(v: &'a serde_json::Value, out: &mut Vec<&'a serde_json::Value>) {
    match v {
        serde_json::Value::Object(map) => {
            if let Some(item) = map.get("playlistPanelVideoRenderer") {
                out.push(item);
            }
            for child in map.values() {
                collect_radio_items(child, out);
            }
        }
        serde_json::Value::Array(arr) => {
            for e in arr {
                collect_radio_items(e, out);
            }
