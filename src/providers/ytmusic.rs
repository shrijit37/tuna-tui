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
        }
        _ => {}
    }
}

fn song_from_mrlir(item: &serde_json::Value) -> Option<Song> {
    let id = extract_video_id(item)?;
    let title = extract_title(item)?;
    let (artists_raw, album_raw, duration_ms) = parse_subtitle(item);
    let thumbnail_url = thumbnail_from_value(item);
    let artists = artists_raw
        .into_iter()
        .map(|name| ArtistRef { id: None, name })
        .collect::<Vec<_>>();
    let album = album_raw.map(|name| AlbumRef { id: None, name });
    let thumbnails = thumbnail_url
        .map(|url| Thumbnail {
            url,
            width: 544,
            height: 544,
        })
        .into_iter()
        .collect();
    Some(Song {
        id,
        title,
        subtitle: None,
        artists,
        album,
        duration_ms,
        thumbnails,
    })
}

fn ytv_from_radio(item: &serde_json::Value) -> Option<YtVideo> {
    let video_id = item.get("videoId").and_then(|v| v.as_str()).or_else(|| {
        item.get("navigationEndpoint")
            .and_then(|n| n.get("watchEndpoint"))
            .and_then(|w| w.get("videoId"))
            .and_then(|v| v.as_str())
    })?;
    if video_id.is_empty() {
        return None;
    }
    // title: title.runs[0].text or simpleText
    let title = item
        .get("title")
        .and_then(|t| {
            t.get("runs")
                .and_then(|r| r.as_array())
                .and_then(|a| a.first())
                .and_then(|r| r.get("text"))
                .and_then(|v| v.as_str())
                .or_else(|| t.get("simpleText").and_then(|v| v.as_str()))
        })
        .unwrap_or("")
        .to_string();
    if title.is_empty() {
        return None;
    }
    // artist: shortBylineText.runs[0].text  / longBylineText
    let artist = item
        .get("shortBylineText")
        .or_else(|| item.get("longBylineText"))
        .and_then(|t| {
            t.get("runs")
                .and_then(|r| r.as_array())
                .and_then(|a| a.first())
                .and_then(|r| r.get("text"))
                .and_then(|v| v.as_str())
        })
        .unwrap_or("")
        .to_string();
    // duration: lengthText.runs[0].text or lengthText.simpleText
    let duration_ms = item
        .get("lengthText")
        .and_then(|t| {
            t.get("runs")
                .and_then(|r| r.as_array())
                .and_then(|a| a.first())
                .and_then(|r| r.get("text"))
                .and_then(|v| v.as_str())
                .or_else(|| t.get("simpleText").and_then(|v| v.as_str()))
        })
        .and_then(parse_duration_to_ms);
    let thumbnail = thumbnail_from_value(item);
    Some(YtVideo {
        uri: format!("yt:video:{video_id}"),
        title,
        artist,
        album: None,
        duration_ms,
        thumbnail,
    })
}

// ---------------------------------------------------------------------------
// public API

/// YouTube Music song search. Returns `None` on transport failure (caller
/// falls back to yt-dlp); `Some(vec![])` on a valid but empty result.
pub fn search_songs(query: &str, limit: usize) -> Option<Vec<Song>> {
    if query.trim().is_empty() {
        return Some(Vec::new());
    }
    let body = serde_json::json!({
        "context": innertube_context(),
        "query": query,
        "params": "EgWKAQIIAQ=="
    });
    let root = post(SEARCH_URL, body)?;
    let songs = parse_search_value(&root, limit);
    Some(songs)
}

fn parse_search_value(root: &serde_json::Value, limit: usize) -> Vec<Song> {
    let mut items = Vec::new();
    collect_mrlir(root, &mut items);
    let mut out = Vec::new();
    for it in items {
        if let Some(s) = song_from_mrlir(it) {
            out.push(s);
            if out.len() >= limit {
                break;
            }
        }
    }
    out
}

/// Resolve a video id to (title, author, album, thumbnail_url) via the
/// YouTube Music player endpoint. `album` is `None` — the endpoint rarely
/// carries it; the caller keeps the yt-dlp fallback's album when absent.
pub fn track_meta(video_id: &str) -> Option<(String, String, Option<String>, Option<String>)> {
    if video_id.trim().is_empty() {
        return None;
    }
    let body = serde_json::json!({
        "context": innertube_context(),
        "videoId": video_id
    });
    let root = post(PLAYER_URL, body)?;
    parse_player_value(&root)
}

fn parse_player_value(
    root: &serde_json::Value,
) -> Option<(String, String, Option<String>, Option<String>)> {
    let details = root.get("videoDetails")?;
    let title = details.get("title").and_then(|v| v.as_str())?.to_string();
    if title.is_empty() {
        return None;
    }
    let author = details
        .get("author")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    // largest thumbnail
    let thumbnail = details
        .get("thumbnail")
        .and_then(|t| t.get("thumbnails"))
        .and_then(|a| a.as_array())
        .and_then(|a| a.last())
        .and_then(|e| e.get("url"))
        .and_then(|u| u.as_str())
        .map(normalize_thumbnail_url);
    Some((title, author, None, thumbnail))
}

/// Fast radio recommendations via the YouTube Music `next` endpoint.
pub fn radio(video_id: &str) -> Option<Vec<YtVideo>> {
    if video_id.trim().is_empty() {
        return None;
    }
    let body = serde_json::json!({
        "context": innertube_context(),
        "videoId": video_id,
        "playlistId": format!("RDAMVM{video_id}"),
        "params": "wAEB"
    });
    let root = post(NEXT_URL, body)?;
    let mut items = Vec::new();
    collect_radio_items(&root, &mut items);
    if items.is_empty() {
        return None;
    }
    let out: Vec<YtVideo> = items.into_iter().filter_map(ytv_from_radio).collect();
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Fetch official lyrics for a video id from YouTube Music InnerTube (`next` -> `browse` endpoint).
pub fn lyrics(video_id: &str) -> Option<String> {
    if video_id.trim().is_empty() {
        return None;
    }
    let body = serde_json::json!({
        "context": innertube_context(),
        "videoId": video_id,
    });
    let next_root = post(NEXT_URL, body)?;
    let tabs = next_root
        .pointer("/contents/singleColumnMusicWatchNextResultsRenderer/tabbedRenderer/watchNextTabbedResultsRenderer/tabs")?
        .as_array()?;
    let lyrics_tab = tabs.iter().find(|t| {
        t.pointer("/tabRenderer/title")
            .and_then(|v| v.as_str())
            .is_some_and(|title| title.eq_ignore_ascii_case("lyrics"))
    })?;
    let browse_id = lyrics_tab
        .pointer("/tabRenderer/endpoint/browseEndpoint/browseId")?
        .as_str()?;

    let browse_body = serde_json::json!({
        "context": innertube_context(),
        "browseId": browse_id,
    });
    let browse_root = post(BROWSE_URL, browse_body)?;
    let runs = browse_root
        .pointer("/contents/sectionListRenderer/contents/0/musicDescriptionShelfRenderer/description/runs")?
        .as_array()?;
    let mut out = String::new();
    for r in runs {
        if let Some(text) = r["text"].as_str() {
            out.push_str(text);
        }
    }
    if out.trim().is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Query YouTube Music for the official 1:1 square album art (`544x544`)
/// for a video ID, falling back to a YouTube Music search by title if needed.
pub fn square_album_art(video_id: &str, title_hint: &str) -> Option<String> {
    if video_id.trim().is_empty() {
        return None;
    }
    // 1. Try /youtubei/v1/next with videoId (fastest: ~100ms)
    let body = serde_json::json!({
        "context": innertube_context(),
        "videoId": video_id
    });
    if let Some(root) = post(NEXT_URL, body) {
        if let Some(url) = deep_googleusercontent_thumb(&root) {
            return Some(normalize_thumbnail_url(&url));
        }
    }
    // 2. Try searching YouTube Music songs with the title/artist hint
    let query = title_hint.trim();
    if !query.is_empty() {
        let search_body = serde_json::json!({
            "context": innertube_context(),
            "query": query,
            "params": "EgWKAQIIAQ=="
        });
        if let Some(root) = post(SEARCH_URL, search_body) {
            if let Some(url) = deep_googleusercontent_thumb(&root) {
                return Some(normalize_thumbnail_url(&url));
            }
        }
    }
    None
}

fn deep_googleusercontent_thumb(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::Object(map) => {
            if let Some(arr) = map.get("thumbnails").and_then(|a| a.as_array()) {
                for t in arr.iter().rev() {
                    if let Some(u) = t.get("url").and_then(|u| u.as_str()) {
                        if u.contains("googleusercontent.com") {
                            return Some(u.to_string());
                        }
                    }
                }
            }
            for child in map.values() {
                if let Some(u) = deep_googleusercontent_thumb(child) {
                    return Some(u);
                }
            }
            None
        }
        serde_json::Value::Array(arr) => {
            for e in arr {
                if let Some(u) = deep_googleusercontent_thumb(e) {
                    return Some(u);
                }
            }
            None
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// offline-tested parsers (exposed for unit tests)

#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn parse_search_json_for_test(root: &serde_json::Value, limit: usize) -> Vec<Song> {
    parse_search_value(root, limit)
}
#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn parse_player_json_for_test(
    root: &serde_json::Value,
) -> Option<(String, String, Option<String>, Option<String>)> {
    parse_player_value(root)
}
#[cfg(test)]
pub(crate) fn parse_next_json_for_test(root: &serde_json::Value) -> Vec<YtVideo> {
    let mut items = Vec::new();
    collect_radio_items(root, &mut items);
    items.into_iter().filter_map(ytv_from_radio).collect()
}

// ---------------------------------------------------------------------------
// tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_rewrites_googleusercontent_and_passes_others() {
        assert_eq!(
            normalize_thumbnail_url("https://lh3.googleusercontent.com/abc=w60-h60-l90-rj"),
            "https://lh3.googleusercontent.com/abc=w544-h544-l90-rj"
        );
        assert_eq!(
            normalize_thumbnail_url("https://lh3.googleusercontent.com/abc=s100"),
            "https://lh3.googleusercontent.com/abc=w544-h544-l90-rj"
        );
        assert_eq!(
            normalize_thumbnail_url("https://lh3.googleusercontent.com/abc=w120-h120-l90-rj"),
            "https://lh3.googleusercontent.com/abc=w544-h544-l90-rj"
        );
        // i.ytimg.com untouched
        assert_eq!(
            normalize_thumbnail_url("https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg"),
            "https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg"
        );
