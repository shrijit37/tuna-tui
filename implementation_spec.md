# ViTune-Compatible Music Backend — Implementation Specification

> **Purpose:** Implement a server-side backend that reproduces the relevant backend behavior of the current `bartoostveen/ViTune` Android project: YouTube Music metadata/search/browse/playback discovery, lyrics, alternative Piped access, Kugou access, SponsorBlock, translation, caching, normalization, fallbacks, observability, and a stable API for clients.
>
> **Important accuracy rule:** This document distinguishes:
> - **VERIFIED FROM VITUNE SOURCE:** directly visible in the repository configuration/source available during this research.
> - **VERIFIED FROM LIVE CAPTURE:** directly visible in the user's uploaded YouTube Music `WEB_REMIX` response.
> - **VERIFIED FROM UPSTREAM DOCUMENTATION:** documented by the upstream service.
> - **IMPLEMENTATION RECOMMENDATION:** a backend design choice proposed here, not a claim that ViTune itself uses it.
>
> This is a compatibility-oriented engineering specification, not a claim that every internal implementation detail of ViTune can be reconstructed from public source alone.

---

## 1. Executive Summary

ViTune is structured as a provider-based Android music client. Its build configuration includes dedicated provider modules for:

```text
providers:github
providers:innertube
providers:kugou
providers:lrclib
providers:piped
providers:sponsorblock
providers:translate
```

and the app also bundles `yt-dlp` and `yt-dlp-ejs`.

The central architecture is therefore:

```text
                              ┌────────────────────────┐
                              │      Your Backend      │
                              │                        │
Client ──────────────────────►│ API / Orchestrator     │
                              │                        │
                              │ Provider Router        │
                              └───────┬───────┬────────┘
                                      │       │
                 ┌────────────────────┘       └───────────────────┐
                 │                                                │
                 ▼                                                ▼
       ┌─────────────────┐                               ┌─────────────────┐
       │   InnerTube     │                               │     Lyrics      │
       │ YouTube Music   │                               │     LRCLIB      │
       └────────┬────────┘                               └─────────────────┘
                │
       ┌────────┴────────┐
       │                 │
       ▼                 ▼
  YouTube metadata     Playback
                       discovery / extraction
                       via YouTube/yt-dlp/Piped

                 Additional providers:
                 ┌───────────────┐
                 │    Piped      │
                 ├───────────────┤
                 │    Kugou      │
                 ├───────────────┤
                 │ SponsorBlock  │
                 ├───────────────┤
                 │   Translate   │
                 └───────────────┘
```

The key design principle is **normalize upstream-specific JSON into a stable internal schema** and expose only your stable schema to clients.

---

# 2. Source Evidence and What Is Confirmed

## 2.1 ViTune project structure

The repository's `settings.gradle.kts` includes:

```text
:providers:common
:providers:github
:providers:innertube
:providers:kugou
:providers:lrclib
:providers:piped
:providers:sponsorblock
:providers:translate
```

This confirms those are deliberate provider modules, not incidental dependencies.

The Android application build also includes the same provider modules and separately installs:

```text
yt-dlp>=2026.07.04
yt-dlp-ejs>=0.8.0
```

through Chaquopy.

This confirms that ViTune has a distinct extraction/playback layer in addition to its metadata provider.

---

## 2.2 Live YouTube Music response captured from the test curl

The user's captured response shows:

```json
{
  "responseContext": {
    "visitorData": "...",
    "serviceTrackingParams": [
      {
        "service": "CSI",
        "params": [
          { "key": "c", "value": "WEB_REMIX" },
          { "key": "cver", "value": "1.20260821.01.00" }
        ]
      }
    ]
  }
}
```

This establishes the live client identity:

```text
clientName    = WEB_REMIX
clientVersion = 1.20260821.01.00
```

The response is a YouTube Music search response, not a generic YouTube video search response.

The same response exposes actual music metadata including:

```text
Daft Punk
Instant Crush (feat. Julian Casablancas)
videoId: khnokW3Mw24
duration: 5:38
plays: 1.2B
album browseId: MPREb_K8qWMWVqXGi
artist browseId: UCRr1xG_2WIDs18a6cIiCxeA
musicVideoType: MUSIC_VIDEO_TYPE_ATV
```

It also exposes other results such as:

```text
Giorgio by Moroder
videoId: ZFZM6jDTWd4

Get Lucky (feat. Pharrell Williams and Nile Rodgers)
videoId: 4D7u5KF7SP8
```

The search therefore returns a collection of result renderers, not a single result.

---

# 3. Goals

The backend should support:

1. Search.
2. Song metadata.
3. Artist metadata.
4. Album metadata.
5. Playlist metadata.
6. YouTube Music browse pages.
7. Continuation/pagination.
8. Playback/stream resolution.
9. Lyrics:
   - plain
   - line-synced
10. Sponsor segment lookup.
11. Optional alternative Piped playback.
12. Optional Kugou metadata/lyrics fallback.
13. Translation.
14. Thumbnail proxying/normalization if desired.
15. Provider health and failover.
16. Response caching.
17. Rate limiting.
18. Request tracing.
19. Stable normalized API independent of upstream renderer changes.

Non-goals:

- Do not reproduce every Android UI behavior.
- Do not expose raw upstream JSON as the primary public API.
- Do not store or redistribute copyrighted audio.
- Do not treat undocumented upstream interfaces as permanent contracts.

---

# 4. Recommended Backend Technology

A practical implementation:

```text
Language:       TypeScript
Runtime:        Node.js 22+
Framework:      Fastify
HTTP Client:    undici / native fetch
Validation:     Zod
Cache:          Redis
Primary DB:     PostgreSQL
Queue:          BullMQ or equivalent
Observability:  OpenTelemetry
Metrics:        Prometheus
Logs:           pino
Container:      Docker
Reverse Proxy:  Caddy / Nginx / Traefik
```

A Rust implementation is also appropriate if very high concurrency and low overhead are priorities.

---

# 5. System Architecture

## 5.1 Layers

```text
┌─────────────────────────────────────────────┐
│                  HTTP API                   │
├─────────────────────────────────────────────┤
│          Authentication / Rate Limit        │
├─────────────────────────────────────────────┤
│              Application Layer              │
│ SearchService / TrackService / Lyrics...    │
├─────────────────────────────────────────────┤
│               Provider Router               │
├──────────────┬──────────────┬───────────────┤
│ InnerTube    │ Piped        │ Kugou         │
│ LRCLIB       │ SponsorBlock │ Translate     │
├──────────────┴──────────────┴───────────────┤
│          HTTP / JSON / Retry Layer          │
├─────────────────────────────────────────────┤
│ Redis / PostgreSQL / Metrics / Tracing      │
└─────────────────────────────────────────────┘
```

## 5.2 Provider abstraction

Define one interface per capability rather than one giant interface.

```ts
export interface SearchProvider {
  search(query: string, options?: SearchOptions): Promise<SearchResultPage>;
}

export interface TrackProvider {
  getTrack(id: string, options?: TrackOptions): Promise<Track>;
}

export interface ArtistProvider {
  getArtist(id: string): Promise<Artist>;
}

export interface AlbumProvider {
  getAlbum(id: string): Promise<Album>;
}

export interface PlaylistProvider {
  getPlaylist(id: string, continuation?: string): Promise<PlaylistPage>;
}

export interface StreamProvider {
  resolveStream(id: string, options?: StreamOptions): Promise<PlaybackInfo>;
}

export interface LyricsProvider {
  getLyrics(query: LyricsQuery): Promise<Lyrics | null>;
}

export interface SegmentProvider {
  getSegments(videoId: string): Promise<SponsorSegments>;
}

export interface TranslationProvider {
  translate(text: string, targetLanguage: string): Promise<string>;
}
```

---

# 6. Provider Capability Matrix

| Capability | InnerTube | Piped | Kugou | LRCLIB | SponsorBlock | Translate |
|---|---:|---:|---:|---:|---:|---:|
| Search | YES | YES | YES | NO | NO | NO |
| Song metadata | YES | YES | YES | partial | NO | NO |
| Artist | YES | YES | YES/partial | NO | NO | NO |
| Album | YES | YES | YES | NO | NO | NO |
| Playlist | YES | YES | YES | NO | NO | NO |
| Playback information | YES | YES | YES/partial | NO | NO | NO |
| Lyrics | NO | NO | YES/partial | YES | NO | NO |
| Sponsor segments | NO | YES/related | NO | NO | YES | NO |
| Translation | NO | NO | NO | NO | NO | YES |

---

# 7. YouTube Music / InnerTube

## 7.1 Base URL

The core endpoint family is:

```text
https://music.youtube.com/youtubei/v1/
```

For direct metadata/search calls, use the endpoint under that path.

Primary operations:

```text
POST /youtubei/v1/search
POST /youtubei/v1/browse
POST /youtubei/v1/player
POST /youtubei/v1/next
```

In practice, append:

```text
?prettyPrint=false
```

for compact responses.

Example:

```text
https://music.youtube.com/youtubei/v1/search?prettyPrint=false
```

---

# 8. InnerTube Request Headers

A minimal working unauthenticated search request is:

```http
POST /youtubei/v1/search?prettyPrint=false HTTP/2
Host: music.youtube.com
Content-Type: application/json
User-Agent: Mozilla/5.0
Origin: https://music.youtube.com
Referer: https://music.youtube.com/
```

Recommended production headers:

```http
Content-Type: application/json
Accept: */*
Accept-Language: en-US,en;q=0.9
Origin: https://music.youtube.com
Referer: https://music.youtube.com/
User-Agent: <current browser-like UA>
```

Depending on the chosen client and request type, additional YouTube-specific headers may be required:

```text
x-youtube-client-name
x-youtube-client-version
x-goog-api-key
x-youtube-utc-offset
x-youtube-bootstrap-logged-in
```

**Do not hard-code these unless the exact upstream client configuration requires them.**

---

# 9. InnerTube Search Request

## 9.1 Minimal body

```json
{
  "context": {
    "client": {
      "clientName": "WEB_REMIX",
      "clientVersion": "1.20260821.01.00",
      "hl": "en",
      "gl": "US"
    }
  },
  "query": "Daft Punk"
}
```

The user's real captured response confirms:

```text
WEB_REMIX
1.20260821.01.00
```

as the current version used in that successful request.

## 9.2 Search curl

```bash
curl 'https://music.youtube.com/youtubei/v1/search?prettyPrint=false' \
  -H 'Content-Type: application/json' \
  -H 'User-Agent: Mozilla/5.0' \
  -H 'Origin: https://music.youtube.com' \
  -H 'Referer: https://music.youtube.com/' \
  --data-raw '{
    "context": {
      "client": {
        "clientName": "WEB_REMIX",
        "clientVersion": "1.20260821.01.00",
        "hl": "en",
        "gl": "US"
      }
    },
    "query": "Daft Punk"
  }'
```

---

# 10. Search Response Shape

The raw structure observed in the live response is:

```text
response
├── responseContext
│   ├── visitorData
│   ├── serviceTrackingParams[]
│   ├── maxAgeSeconds
│   └── responseId
└── contents
    └── tabbedSearchResultsRenderer
        └── tabs[]
            └── tabRenderer
                ├── title
                ├── selected
                └── content
                    └── sectionListRenderer
                        └── contents[]
                            └── musicCardShelfRenderer
                                ├── thumbnail
                                ├── title
                                ├── subtitle
                                └── contents[]
                                    └── musicResponsiveListItemRenderer[]
```

## 10.1 Song result fields

A typical song renderer can be normalized from:

```text
musicResponsiveListItemRenderer
├── thumbnail.musicThumbnailRenderer.thumbnail.thumbnails[]
├── overlay.musicItemThumbnailOverlayRenderer.content
│   └── musicPlayButtonRenderer
│       └── playNavigationEndpoint.watchEndpoint.videoId
├── flexColumns[]
├── menu
└── playlistItemData.videoId
```

The title is normally found under:

```text
flexColumns[0]
  .musicResponsiveListItemFlexColumnRenderer
  .text
  .runs[]
  .text
```

The type and duration are commonly under the second flex column.

The raw result can expose:

```text
title
duration
play count
videoId
thumbnail
album browseId
artist browseId
track credits browseId
musicVideoType
```

---

# 11. Canonical Internal Song Schema

Do not expose YouTube renderers publicly.

Use:

```ts
export interface Song {
  id: string;                 // YouTube videoId or provider-native ID
  provider: "youtube" | "piped" | "kugou";
  title: string;
  subtitle?: string;
  artists: ArtistRef[];
  album?: AlbumRef;
  durationMs?: number;
  thumbnails: Thumbnail[];
  explicit?: boolean;
  isVideo?: boolean;
  musicVideoType?: string;
  playCount?: number;
  url?: string;
  source?: SourceRef;
}
```

Artist:

```ts
export interface ArtistRef {
  id?: string;
  name: string;
}
```

Album:

```ts
export interface AlbumRef {
  id?: string;
  name: string;
}
```

Thumbnail:

```ts
export interface Thumbnail {
  url: string;
  width: number;
  height: number;
}
```

---

# 12. Search API Exposed By Your Backend

Recommended endpoint:

```http
GET /v1/search?q=Daft%20Punk&type=song&page=1
```

Response:

```json
{
  "items": [
    {
      "id": "khnokW3Mw24",
      "provider": "youtube",
      "title": "Instant Crush (feat. Julian Casablancas)",
      "artists": [
        {
          "id": "UCRr1xG_2WIDs18a6cIiCxeA",
          "name": "Daft Punk"
        }
      ],
      "album": {
        "id": "MPREb_K8qWMWVqXGi"
      },
      "durationMs": 338000,
      "thumbnails": [
        {
          "url": "https://...",
          "width": 120,
          "height": 120
        }
      ]
    }
  ],
  "continuation": null,
  "provider": "innertube"
}
```

Recommended public query parameters:

```text
q
type=song|album|artist|playlist|video
page
continuation
limit
region
language
```

---

# 13. Search Pagination / Continuations

InnerTube responses can contain continuation objects.

Never assume:

```text
one request = complete result set
```

Implement:

```ts
interface SearchResultPage {
  items: Song[];
  continuation?: string;
}
```

The public API should let the client send the opaque continuation token back:

```http
GET /v1/search?q=Daft%20Punk&continuation=<opaque>
```

Do not reinterpret an upstream continuation token.

Store no assumptions about its internal encoding.

---

# 14. Browse

Browse is used to load entities and pages such as:

```text
artist
album
playlist
charts
music pages
track credits
```

Endpoint:

```text
POST https://music.youtube.com/youtubei/v1/browse?prettyPrint=false
```

The request uses:

```json
{
  "context": {
    "client": {
      "clientName": "WEB_REMIX",
      "clientVersion": "1.20260821.01.00",
      "hl": "en",
      "gl": "US"
    }
  },
  "browseId": "<browseId>"
}
```

Example artist browse ID observed in the live search result:

```text
UCRr1xG_2WIDs18a6cIiCxeA
```

Example album browse ID:

```text
MPREb_K8qWMWVqXGi
```

Track credits browse ID follows the observed pattern:

```text
MPTC<videoId>
```

Do not construct browse IDs manually if the upstream response already provides them. Treat the browse ID as opaque.

---

# 15. Player

The player endpoint is:

```text
POST https://music.youtube.com/youtubei/v1/player?prettyPrint=false
```

Purpose:

```text
videoId
  ↓
player
  ↓
playability status
metadata
streamingData
formats
adaptiveFormats
```

Canonical request concept:

```json
{
  "context": {
    "client": {
      "clientName": "WEB_REMIX",
      "clientVersion": "1.20260821.01.00",
      "hl": "en",
      "gl": "US"
    }
  },
  "videoId": "khnokW3Mw24"
}
```

Possible raw response areas:

```text
playabilityStatus
videoDetails
microformat
streamingData
  ├── formats[]
  └── adaptiveFormats[]
playerConfig
storyboards
```

Important fields in a stream object can include:

```text
itag
mimeType
bitrate
width
height
lastModified
contentLength
quality
qualityLabel
projectionType
averageBitrate
audioQuality
approxDurationMs
url
signatureCipher
cipher
```

**Do not assume a direct `url` will always be present.**

Some streams can require deciphering and/or additional player logic.

---

# 16. Playback Backend Strategy

Use a dedicated playback resolver:

```text
resolvePlayback(videoId)
       │
       ├── InnerTube player
       │
       ├── Piped /streams/:videoId
       │
       └── yt-dlp / equivalent extractor
```

Recommended preference:

```text
1. Cached still-valid playback URL
2. Primary InnerTube player
3. Piped
4. yt-dlp
5. Return structured failure
```

Each candidate should be health-checked before being exposed to the client.

---

# 17. Piped

Piped officially documents a public API with an instance-specific base URL.

Example documented instance:

```text
https://pipedapi.kavin.rocks
```

Piped explicitly warns that the instance list should be dynamically discovered instead of permanently assuming a single instance.

Documentation:

```text
https://docs.piped.video/docs/api-documentation/
```

## 17.1 Streams

Endpoint:

```text
GET /streams/:videoId
```

Full example:

```text
https://pipedapi.kavin.rocks/streams/khnokW3Mw24
```

The documented response contains:

```json
{
  "audioStreams": [
    {
      "bitrate": 0,
      "codec": "...",
      "format": "...",
      "mimeType": "audio/mp4",
      "quality": "...",
      "url": "https://...",
      "itag": 0
    }
  ],
  "videoStreams": [
    {
      "bitrate": 0,
      "codec": "avc1.64002a",
      "format": "MPEG_4",
      "fps": 30,
      "height": 720,
      "indexEnd": 0,
      "indexStart": 0,
      "initStart": 0,
      "initEnd": 0,
      "mimeType": "video/mp4",
      "quality": "720p",
      "url": "https://...",
      "videoOnly": false,
      "width": 1280
    }
  ],
  "views": 0
}
```

Piped's API documentation also exposes channel and other metadata endpoints.

---

# 18. Piped Base URL Selection

Do not hard-code one public Piped instance in production.

Create:

```ts
interface PipedInstance {
  apiBase: string;
  proxyBase?: string;
  healthy: boolean;
  latencyMs?: number;
}
```

Discover instances from:

```text
https://github.com/TeamPiped/Piped/wiki/Instances
```

At startup and periodically:

```text
discover instances
      ↓
probe /streams/<known-video-id>
      ↓
record:
  status
  latency
  TLS validity
  response correctness
      ↓
rank healthy instances
```

---

# 19. LRCLIB

The project's provider list confirms a dedicated LRCLIB provider.

The documented API base is:

```text
https://lrclib.net/api
```

The documented endpoints include:

```text
GET  /api/get
GET  /api/get/{id}
GET  /api/search
GET  /api/get-cached
POST /api/request-challenge
POST /api/publish
POST /api/flag
```

For a playback client, the two primary read operations are:

```text
GET /api/get
GET /api/search
```

## 19.1 Exact metadata lookup

Conceptual request:

```text
GET https://lrclib.net/api/get
  ?track_name=Instant%20Crush
  &artist_name=Daft%20Punk
  &album_name=Random%20Access%20Memories
  &duration=338
```

The metadata lookup should use:

```text
track_name
artist_name
album_name
duration
```

where available.

## 19.2 Search

```text
GET https://lrclib.net/api/search?q=Daft%20Punk
```

or the query fields supported by the current LRCLIB API version.

Canonical normalized model:

```ts
interface Lyrics {
  trackId?: number;
  trackName: string;
  artistName: string;
  albumName?: string;
  duration?: number;
  plainLyrics?: string | null;
  syncedLyrics?: string | null;
}
```

Do not publish lyrics data without considering the upstream service/license conditions.

---

# 20. Lyrics Matching Algorithm

Recommended order:

```text
1. Exact title + exact artist + exact duration
2. Exact title + artist + approximate duration
3. Search title + artist
4. Search normalized title + normalized artist
```

Normalize:

```text
case
Unicode punctuation
feat./ft./featuring
parentheses
hyphens
extra whitespace
```

Duration match:

```text
abs(candidateDuration - trackDuration) <= 3 seconds
```

Use a larger tolerance only as a fallback.

---

# 21. Kugou

The project contains a dedicated Kugou provider.

Public third-party documentation shows a song search endpoint:

```text
http://msearchcdn.kugou.com/api/v3/search/song
```

Required parameters documented by one API reference:

```text
plat=0
keyword=<query>
version=9108
```

Useful optional parameters:

```text
pagesize
page
highlight
tagtype
tag_aggr
```

Example:

```text
http://msearchcdn.kugou.com/api/v3/search/song?plat=0&keyword=Daft%20Punk&pagesize=20&version=9108
```

Other historically documented Kugou endpoints include:

```text
http://mobilecdn.kugou.com/api/v3/search/song
http://mobilecdn.kugou.com/api/v3/search/special
http://msearch.kugou.com/api/v3/search/mv
http://msearch.kugou.com/api/v3/search/album
http://mobileservice.kugou.com/api/v3/lyric/search
```

**Caution:** Kugou endpoint documentation available publicly is often reverse-engineered and version-dependent. Treat these as adapter endpoints and expect breakage.

---

# 22. Kugou Response Shape

A commonly documented search response is structurally:

```json
{
  "status": 1,
  "error": "",
  "data": {
    "aggregation": [],
    "timestamp": 0,
    "info": [
      {
        "songname": "...",
        "singername": "...",
        "album_name": "...",
        "hash": "...",
        "duration": 0,
        "album_id": 0,
        "singerid": 0,
        "trans_param": {}
      }
    ]
  },
  "errcode": 0
}
```

Because Kugou's response fields vary by endpoint/version, the adapter should parse defensively.

---

# 23. SponsorBlock

SponsorBlock is a separate crowd-sourced segment service.

Documented public API:

```text
https://sponsor.ajay.app
```

API documentation:

```text
https://github.com/ajayyy/SponsorBlock/wiki/API-Docs
```

A relevant operation is retrieving segments associated with a video.

Conceptual request:

```text
GET https://sponsor.ajay.app/api/skipSegments/<hash>
```

or the current API-equivalent query documented by SponsorBlock.

The response contains segment objects with fields such as:

```json
{
  "segment": [0, 10],
  "category": "sponsor",
  "actionType": "skip",
  "videoDuration": 0,
  "UUID": "..."
}
```

Do not hard-code the precise path from old third-party examples; verify against the current SponsorBlock API documentation at implementation time.

---

# 24. SponsorBlock Normalized Schema

```ts
interface SponsorSegment {
  id: string;
  startMs: number;
  endMs: number;
  category:
    | "sponsor"
    | "intro"
    | "outro"
    | "selfpromo"
    | "interaction"
    | "music_offtopic"
    | "preview"
    | "filler"
    | string;
  action: "skip" | string;
}
```

---

# 25. Translation Provider

The repository contains:

```text
providers:translate
```

but the publicly surfaced repository configuration does not expose enough evidence here to assert a single exact external translation endpoint.

Therefore:

**Do not claim an exact Google Translate/LibreTranslate/etc. URL for ViTune without inspecting the provider source itself.**

For your backend, use an abstraction:

```ts
interface TranslationProvider {
  translate(
    text: string,
    sourceLanguage: string | undefined,
    targetLanguage: string
  ): Promise<{
    text: string;
    sourceLanguage?: string;
    targetLanguage: string;
  }>;
}
```

Implementation options:

```text
LibreTranslate
Google Cloud Translation
DeepL
self-hosted NLLB/Marian
```

Choose one explicitly rather than assuming the ViTune provider implementation.

---

# 26. GitHub Provider

The repository also contains:

```text
providers:github
```

but the available evidence does not justify claiming a specific GitHub endpoint or runtime dependency for music metadata.

Treat GitHub integration as a separate provider capability.

If your backend needs release metadata:

```text
GET https://api.github.com/repos/<owner>/<repo>/releases/latest
```

is an example of the public GitHub API, but this is an implementation choice, not a verified ViTune runtime contract.

---

# 27. Your Stable Public API

Recommended public API surface:

```text
GET /v1/search
GET /v1/songs/:id
GET /v1/artists/:id
GET /v1/albums/:id
GET /v1/playlists/:id
GET /v1/playlists/:id/continuation
GET /v1/streams/:id
GET /v1/lyrics
GET /v1/suggestions
GET /v1/segments/:id
GET /v1/health
GET /v1/providers
```

Optional:

```text
GET /v1/home
GET /v1/charts
GET /v1/related/:id
GET /v1/next/:id
GET /v1/artist/:id/top
GET /v1/artist/:id/albums
GET /v1/album/:id/tracks
```

---

# 28. `/v1/songs/:id`

Response:

```json
{
  "id": "khnokW3Mw24",
  "provider": "youtube",
  "title": "Instant Crush (feat. Julian Casablancas)",
  "artists": [
    {
      "id": "UCRr1xG_2WIDs18a6cIiCxeA",
      "name": "Daft Punk"
    }
  ],
  "album": {
    "id": "MPREb_K8qWMWVqXGi",
    "name": "Random Access Memories"
  },
  "durationMs": 338000,
  "thumbnails": [],
  "type": "song"
}
```

---

# 29. `/v1/artists/:id`

Response:

```json
{
  "id": "UCRr1xG_2WIDs18a6cIiCxeA",
  "name": "Daft Punk",
  "description": null,
  "thumbnails": [],
  "followers": null,
  "monthlyAudience": 80500000,
  "albums": [],
  "singles": [],
  "topSongs": [],
  "continuation": null
}
```

Use `null` rather than fabricating values when upstream does not expose a field.

---

# 30. `/v1/albums/:id`

```json
{
  "id": "MPREb_K8qWMWVqXGi",
  "title": "Random Access Memories",
  "artists": [
    {
      "id": "UCRr1xG_2WIDs18a6cIiCxeA",
      "name": "Daft Punk"
    }
  ],
  "year": 2013,
  "thumbnails": [],
  "tracks": [
    {
      "id": "khnokW3Mw24",
      "title": "Instant Crush (feat. Julian Casablancas)",
      "durationMs": 338000,
      "trackNumber": 7
    }
  ],
  "continuation": null
}
```

Exact album year/track number must only be emitted if present in the upstream data.

---

# 31. `/v1/playlists/:id`

```json
{
  "id": "...",
  "title": "...",
  "description": null,
  "owner": null,
  "thumbnails": [],
  "items": [
    {
      "id": "...",
      "title": "...",
      "artists": [],
      "durationMs": null
    }
  ],
  "continuation": null
}
```

---

# 32. `/v1/lyrics`

Request:

```http
GET /v1/lyrics?title=Instant%20Crush&artist=Daft%20Punk&album=Random%20Access%20Memories&duration=338
```

Response:

```json
{
  "found": true,
  "source": "lrclib",
  "track": {
    "title": "Instant Crush",
    "artist": "Daft Punk",
    "album": "Random Access Memories"
  },
  "durationMs": 338000,
  "plainLyrics": "...",
  "syncedLyrics": "[00:12.00]..."
}
```

No-found response:

```json
{
  "found": false,
  "source": null,
  "track": {
    "title": "Unknown",
    "artist": "Unknown"
  },
  "plainLyrics": null,
  "syncedLyrics": null
}
```

---

# 33. `/v1/streams/:id`

Use a provider-neutral stream schema.

```json
{
  "id": "khnokW3Mw24",
  "provider": "piped",
  "expiresAt": "2026-08-22T18:00:00Z",
  "audio": [
    {
      "url": "https://...",
      "mimeType": "audio/mp4",
      "codec": "mp4a.40.2",
      "bitrate": 128000,
      "sampleRate": 48000,
      "channels": 2,
      "contentLength": null
    }
  ],
  "video": []
}
```

Never assume a signed URL is permanent.

---

# 34. URL Expiration and Playback Caching

Do not cache a stream URL for the same duration as metadata.

Use:

```text
metadata TTL: 6h–24h
artist TTL: 6h–24h
album TTL: 6h
playlist TTL: 5m–1h
search TTL: 30s–5m
lyrics TTL: 7d
stream URL TTL: short, provider-dependent
```

Prefer:

```text
Cache-Control / upstream expiry timestamp
```

over hard-coded stream lifetimes.

---

# 35. Cache Keys

Recommended:

```text
yt:search:{region}:{language}:{normalizedQuery}:{type}:{continuationHash}

yt:song:{videoId}

yt:artist:{browseId}

yt:album:{browseId}

yt:playlist:{browseId}

stream:{videoId}:{clientProfile}

lyrics:{sha256(title|artist|album|duration)}

sponsor:{videoId}
```

---

# 36. Cache Stampede Protection

For hot tracks:

```text
GET cache miss
      ↓
distributed lock
      ↓
one request fetches upstream
      ↓
other requests wait briefly
      ↓
result stored
```

Use Redis:

```text
SET lock:key value NX PX 5000
```

with a unique lock owner/token.

---

# 37. Database

You do not need to persist every upstream object.

Recommended tables:

```text
songs
artists
albums
playlists
playlist_items
song_aliases
provider_ids
lyrics_cache
provider_health
request_log
```

## 37.1 songs

```sql
CREATE TABLE songs (
  id UUID PRIMARY KEY,
  canonical_provider TEXT NOT NULL,
  provider_song_id TEXT NOT NULL,
  title TEXT NOT NULL,
  duration_ms INTEGER,
  explicit BOOLEAN,
  created_at TIMESTAMPTZ NOT NULL,
  updated_at TIMESTAMPTZ NOT NULL,
  UNIQUE(canonical_provider, provider_song_id)
);
```

## 37.2 provider_ids

```sql
CREATE TABLE provider_ids (
  song_id UUID NOT NULL,
  provider TEXT NOT NULL,
  provider_id TEXT NOT NULL,
  metadata JSONB,
  PRIMARY KEY(song_id, provider)
);
```

This lets one canonical song map to:

```text
YouTube videoId
Kugou hash
Piped video ID
other provider IDs
```

---

# 38. Search Normalization

Input:

```text
"  Daft  Punk  "
```

Normalize to:

```text
"daft punk"
```

Rules:

```text
Unicode NFKC
trim
collapse whitespace
lowercase for cache key
preserve original query for upstream
```

Do not remove punctuation before sending upstream unless required.

---

# 39. Metadata Merge

When multiple providers return the same track:

```text
YouTube:
  title
  duration
  artist
  album

Kugou:
  hash
  alternate lyrics metadata

LRCLIB:
  lyrics

Piped:
  playback

SponsorBlock:
  segments
```

Do not overwrite values blindly.

Use field precedence:

```text
canonical title:
  YouTube Music > Kugou

duration:
  YouTube > Piped > Kugou

artist:
  YouTube Music > Kugou

album:
  YouTube Music > Kugou

lyrics:
  LRCLIB > Kugou fallback

stream:
  healthy primary resolver > fallback
```

---

# 40. Provider Failure Taxonomy

Every provider error should map into:

```ts
type ProviderErrorCode =
  | "TIMEOUT"
  | "RATE_LIMITED"
  | "UNAUTHORIZED"
  | "FORBIDDEN"
  | "NOT_FOUND"
  | "UNPLAYABLE"
  | "BAD_RESPONSE"
  | "UPSTREAM_5XX"
  | "UPSTREAM_4XX"
  | "PARSER_ERROR"
  | "INSTANCE_UNHEALTHY"
  | "UNKNOWN";
```

Never leak raw upstream stack traces to clients.

---

# 41. Retry Policy

Retry only safe/idempotent requests.

Recommended:

```text
Timeout:
  250ms → 750ms → 1500ms

HTTP 429:
  respect Retry-After

HTTP 5xx:
  250ms → 750ms → 1500ms

HTTP 4xx:
  generally no retry
```

Limit total upstream attempts:

```text
search: 2
metadata: 2
lyrics: 2
playback: 3+
```

---

# 42. Circuit Breakers

Maintain per-provider:

```text
state:
  CLOSED
  OPEN
  HALF_OPEN
```

Trip when:

```text
5xx rate > 30%
or timeout rate > 30%
or 5 consecutive failures
```

Reset after:

```text
15–30 seconds
```

Tune using real production telemetry.

---

# 43. Piped Instance Health

For each Piped instance record:

```text
instance URL
last success
last failure
latency p50
latency p95
success rate
TLS status
last tested endpoint
```

Probe:

```text
/streams/<known-video-id>
```

If the instance has an unhealthy proxy domain, mark the instance degraded.

---

# 44. Observability

Every request gets:

```text
request_id
trace_id
provider
operation
upstream_url
status
latency_ms
retry_count
cache_hit
response_bytes
```

Metrics:

```text
http_requests_total
http_request_duration_ms
provider_requests_total
provider_request_duration_ms
provider_errors_total
cache_hits_total
cache_misses_total
stream_resolution_success_total
stream_resolution_failure_total
lyrics_hits_total
lyrics_misses_total
```

---

# 45. OpenTelemetry

Create spans:

```text
HTTP GET /v1/search
└── provider.search
    └── http POST music.youtube.com/youtubei/v1/search
```

For playback:

```text
GET /v1/streams/:id
└── stream.resolve
    ├── cache.get
    ├── innertube.player
    ├── piped.streams
    └── extractor.resolve
```

Do not record cookies, authorization tokens, or other secrets as span attributes.

---

# 46. Security

## Required

- TLS.
- Request size limits.
- Query length limits.
- SSRF protection.
- URL allowlisting for server-side fetches.
- Rate limiting.
- Redis authentication.
- DB credentials via secrets.
- No provider credentials in logs.
- No user cookies in logs.

## SSRF protection

Never allow clients to send arbitrary:

```text
url=https://anything
```

to a generic backend fetch endpoint.

Provider URLs must be constructed from allowlisted base URLs.

---

# 47. Rate Limiting

Suggested initial limits:

```text
unauthenticated search:
  30 requests/min/IP

metadata:
  120 requests/min/IP

lyrics:
  60 requests/min/IP

playback:
  60 requests/min/IP
```

These are backend recommendations, not ViTune values.

Use Redis token bucket/sliding window.

---

# 48. Concurrency Limits

Per provider:

```text
InnerTube: 20 concurrent
Piped:     20 per instance
Kugou:     10
LRCLIB:    10
SponsorBlock: 20
Translate: 5
```

Tune based on provider response and rate limits.

---

# 49. Provider Router

Example:

```ts
class ProviderRouter {
  constructor(
    private readonly innerTube: InnerTubeProvider,
    private readonly piped: PipedProvider,
    private readonly kugou: KugouProvider,
    private readonly lrclib: LrclibProvider,
    private readonly sponsorBlock: SponsorBlockProvider,
    private readonly translate: TranslationProvider,
  ) {}

  async search(query: string) {
    try {
      return await this.innerTube.search(query);
    } catch {
      try {
        return await this.piped.search(query);
      } catch {
        return await this.kugou.search(query);
      }
    }
  }
}
```

Production implementation should add:

```text
timeouts
metrics
circuit breaker
cache
structured errors
```

---

# 50. Search Fallback Strategy

Preferred:

```text
InnerTube
    │
    ├── success → return
    │
    └── fail
          ↓
        Piped
          │
          ├── success → return
          │
          └── fail
                ↓
              Kugou
```

Do not merge unrelated result sets unless explicitly requested.

The default behavior should be:

```text
one canonical provider result set
```

with provider fallback only on error.

---

# 51. Stream Fallback Strategy

```text
1. InnerTube player
2. Piped /streams
3. yt-dlp
4. fail with structured error
```

For each resolver:

```ts
interface PlaybackResolver {
  resolve(videoId: string): Promise<PlaybackInfo>;
}
```

The final resolver should return the same normalized schema.

---

# 52. `yt-dlp` Integration

ViTune's Android build configuration explicitly installs:

```text
yt-dlp>=2026.07.04
yt-dlp-ejs>=0.8.0
```

For a backend implementation, isolate yt-dlp:

```text
Node API
  ↓
worker / subprocess
  ↓
yt-dlp
```

Recommended worker invocation:

```bash
yt-dlp \
  --dump-single-json \
  --no-playlist \
  'https://www.youtube.com/watch?v=<videoId>'
```

Use output parsing, not shell text scraping.

Important:

- pin a known yt-dlp version;
- monitor upstream breakages;
- never accept arbitrary URLs from public clients;
- allowlist YouTube domains;
- apply process timeouts.

---

# 53. Suggested Internal Service Boundaries

```text
services/
  search/
  metadata/
  playback/
  lyrics/
  sponsors/
  translation/
  providers/
    innertube/
    piped/
    kugou/
    lrclib/
    sponsorblock/
  cache/
  normalization/
  observability/
```

---

# 54. Recommended Repository Layout

```text
vitune-backend/
├── apps/
│   └── api/
│       └── src/
│           ├── routes/
│           ├── services/
│           ├── providers/
│           ├── schemas/
│           ├── middleware/
│           └── server.ts
│
├── packages/
│   ├── domain/
│   ├── provider-contracts/
│   ├── normalization/
│   ├── observability/
│   └── config/
│
├── workers/
│   └── extractor/
│
├── infra/
│   ├── docker/
│   ├── postgres/
│   └── redis/
│
└── tests/
```

---

# 55. Environment Variables

```env
NODE_ENV=production
PORT=8080

DATABASE_URL=postgresql://...

REDIS_URL=redis://...

YT_CLIENT_NAME=WEB_REMIX
YT_CLIENT_VERSION=1.20260821.01.00
YT_REGION=US
YT_LANGUAGE=en

PIPED_INSTANCE_DISCOVERY_URL=https://github.com/TeamPiped/Piped/wiki/Instances

LRCLIB_BASE_URL=https://lrclib.net/api

SPONSORBLOCK_BASE_URL=https://sponsor.ajay.app

KUGOU_SEARCH_BASE_URL=http://msearchcdn.kugou.com

REQUEST_TIMEOUT_MS=10000

YT_SEARCH_CACHE_TTL=60
YT_METADATA_CACHE_TTL=21600
LYRICS_CACHE_TTL=604800
```

Do not put secret cookies/API keys directly in source.

---

# 56. API Versioning

Always version your own API:

```text
/v1/search
/v1/songs/:id
/v1/streams/:id
```

Do not expose:

```text
/youtubei/v1/...
```

as your public contract.

That protects clients when YouTube changes renderer structures.

---

# 57. Response Envelope

Choose one consistent envelope.

Recommended:

```json
{
  "data": {},
  "meta": {
    "provider": "innertube",
    "requestId": "..."
  }
}
```

Errors:

```json
{
  "error": {
    "code": "UPSTREAM_UNAVAILABLE",
    "message": "No playback provider is currently available.",
    "retryable": true,
    "requestId": "..."
  }
}
```

---

# 58. Versioning Provider Parsers

Treat raw upstream schemas as versions:

```text
innertube-search-v1
innertube-search-v2
piped-stream-v1
kugou-search-v1
```

Parser:

```ts
parseInnerTubeSearch(payload): SearchResultPage
```

Test parser against saved fixtures.

Never unit-test only against live endpoints.

---

# 59. Fixture Strategy

Save sanitized fixtures:

```text
tests/fixtures/innertube/
  search-daft-punk.json
  browse-artist.json
  browse-album.json
  player-track.json

tests/fixtures/piped/
  streams-track.json

tests/fixtures/lrclib/
  lyrics-track.json

tests/fixtures/kugou/
  search-track.json

tests/fixtures/sponsorblock/
  segments-track.json
```

Fixtures should be sanitized for:

```text
cookies
visitor IDs
personal data
tokens
temporary signed URLs
```

---

# 60. Contract Tests

For every provider:

```text
test parse minimal response
test parse empty response
test parse malformed response
test parse continuation
test parse unknown renderer
test parse missing optional field
test upstream error mapping
```

For InnerTube especially:

```text
test musicCardShelfRenderer
test musicResponsiveListItemRenderer
test plain musicResponsiveShelfRenderer
test continuationItemRenderer
test sectionListRenderer
test tabbedSearchResultsRenderer
```

---

# 61. Unknown Renderers

Do not crash because YouTube adds:

```text
someNewRenderer
```

Parser policy:

```text
known renderer → parse
unknown renderer → ignore + metric
```

Metric:

```text
innertube_unknown_renderer_total{
  renderer="..."
}
```

This is critical for long-term stability.

---

# 62. Field Extraction Helpers

Example:

```ts
function textFromRuns(value: unknown): string | undefined {
  if (!isObject(value)) return undefined;

  const runs = value.runs;
  if (!Array.isArray(runs)) return undefined;

  return runs
    .map((run) => isObject(run) && typeof run.text === "string" ? run.text : "")
    .join("");
}
```

Never assume:

```text runs[0].text
```

exists.

---

# 63. Duration Parsing

YouTube Music can return duration as text:

```text
5:38
9:05
6:10
```

Parse:

```text
5:38 = 338 seconds
9:05 = 545 seconds
6:10 = 370 seconds
```

Implementation:

```ts
function parseDuration(value: string): number | null {
  const parts = value.split(":").map(Number);

  if (parts.some(Number.isNaN)) return null;

  if (parts.length === 2) {
    return (parts[0] * 60 + parts[1]) * 1000;
  }

  if (parts.length === 3) {
    return (parts[0] * 3600 + parts[1] * 60 + parts[2]) * 1000;
  }

  return null;
}
```

---

# 64. Play Count Parsing

Observed strings can look like:

```text
1.2B plays
94M plays
1.8B plays
```

Normalize:

```text
1.2B → 1_200_000_000
94M  → 94_000_000
1.8B → 1_800_000_000
```

Never assume play count is always present.

---

# 65. Thumbnail Normalization

Preserve the complete thumbnail array:

```json
[
  {
    "url": "...",
    "width": 60,
    "height": 60
  },
  {
    "url": "...",
    "width": 120,
    "height": 120
  }
]
```

Do not strip query parameters from YouTube image URLs.

---

# 66. ID Semantics

You will encounter several identifier classes.

```text
YouTube videoId
YouTube Music browseId
YouTube playlistId
Kugou hash
Kugou album ID
LRCLIB track ID
SponsorBlock video ID
```

Never place them all in the same untyped field.

Use:

```ts
type ProviderId =
  | {
      provider: "youtube";
      kind: "video";
      value: string;
    }
  | {
      provider: "youtube";
      kind: "browse";
      value: string;
    }
  | {
      provider: "kugou";
      kind: "hash";
      value: string;
    };
```

---

# 67. Canonical Track Identity

Do not assume:

```text
same title + same artist = same recording
```

A safer fingerprint:

```text
normalized title
normalized primary artist
album
duration ± tolerance
```

Use fuzzy matching only when provider mappings are being created.

---

# 68. Provider Linking

When a YouTube result is obtained:

```text
YouTube title + artist + album + duration
               ↓
             search
               ↓
            Kugou
               ↓
            score
               ↓
       create provider mapping
```

Never automatically link a low-confidence match.

Suggested score:

```text
title similarity       40%
artist similarity      30%
album similarity       15%
duration                15%
```

---

# 69. Request Flow: Search

```text
Client
  │
  ▼
GET /v1/search?q=Daft Punk
  │
  ▼
API validation
  │
  ▼
Redis cache
  │
  ├── HIT ───────────────► normalized response
  │
  └── MISS
        │
        ▼
   InnerTube provider
        │
        ▼
  raw renderer response
        │
        ▼
  parser / normalizer
        │
        ▼
  cache
        │
        ▼
      client
```

---

# 70. Request Flow: Song Details

```text
GET /v1/songs/khnokW3Mw24
       │
       ▼
cache lookup
       │
       ▼
InnerTube browse/player as required
       │
       ▼
normalize
       │
       ▼
return
```

---

# 71. Request Flow: Lyrics

```text
GET /v1/lyrics
       │
       ▼
normalize metadata
       │
       ▼
LRCLIB exact lookup
       │
       ├── hit → return
       │
       └── miss
             ▼
           LRCLIB search
             │
             └── match → return
```

Optional:

```text
Kugou lyrics fallback
```

---

# 72. Request Flow: Playback

```text
GET /v1/streams/khnokW3Mw24
       │
       ▼
cache
       │
       ├── valid → return
       │
       └── expired
             │
             ▼
        InnerTube player
             │
             ├── playable → normalize
             │
             └── unavailable
                   ▼
                 Piped
                   │
                   ├── playable
                   │
                   └── fail
                         ▼
                       yt-dlp
```

---

# 73. Health Endpoint

```http
GET /v1/health
```

Response:

```json
{
  "status": "ok",
  "providers": {
    "innertube": {
      "status": "healthy",
      "latencyMs": 120
    },
    "piped": {
      "status": "healthy",
      "latencyMs": 240
    },
    "lrclib": {
      "status": "healthy",
      "latencyMs": 95
    }
  }
}
```

---

# 74. Provider Endpoint

Useful internal endpoint:

```http
GET /v1/providers
```

Response:

```json
{
  "providers": [
    {
      "name": "innertube",
      "capabilities": [
        "search",
        "browse",
        "player"
      ],
      "status": "healthy"
    },
    {
      "name": "piped",
      "capabilities": [
        "search",
        "streams"
      ],
      "status": "healthy"
    },
    {
      "name": "lrclib",
      "capabilities": [
        "lyrics"
      ],
      "status": "healthy"
    }
  ]
}
```

Keep this endpoint admin-only if you do not want to reveal infrastructure details publicly.

---

# 75. Logging Format

Use structured JSON.

Example:

```json
{
  "level": "info",
  "time": "2026-08-22T12:00:00.000Z",
  "requestId": "req_123",
  "route": "/v1/search",
  "provider": "innertube",
  "operation": "search",
  "latencyMs": 184,
  "cacheHit": false,
  "status": 200
}
```

Never log:

```text
Cookie
Authorization
session tokens
signed stream URLs
visitor secrets
```

---

# 76. Timeouts

Recommended:

```text
HTTP request timeout:
  10 seconds

InnerTube:
  8 seconds

Piped:
  8 seconds

LRCLIB:
  5 seconds

Kugou:
  5 seconds

SponsorBlock:
  5 seconds

Translation:
  10 seconds

yt-dlp process:
  30 seconds
```

These values are recommendations, not ViTune constants.

---

# 77. Compression

Enable:

```text
gzip
brotli
```

for JSON responses.

Do not compress already-compressed media streams.

---

# 78. CORS

For browser clients:

```http
Access-Control-Allow-Origin: <configured-origin>
Access-Control-Allow-Methods: GET, OPTIONS
Access-Control-Allow-Headers: Content-Type, Authorization
```

Avoid:

```text
Access-Control-Allow-Origin: *
```

if you later introduce authentication.

---

# 79. Client Compatibility

The normalized API should not require the Android client to understand:

```text
musicResponsiveListItemRenderer
musicCardShelfRenderer
sectionListRenderer
```

Only provider adapters should know those renderer names.

This is the most important separation in the architecture.

---

# 80. What Happens When YouTube Changes

Example:

```text
Today:
musicResponsiveListItemRenderer
```

Tomorrow:

```text
musicResponsiveListItemRenderer2
```

Only the parser changes:

```text
providers/innertube/parsers/search.ts
```

The public API remains:

```json
{
  "id": "...",
  "title": "...",
  "artists": []
}
```

This prevents upstream schema churn from breaking clients.

---

# 81. Caching Raw vs Normalized Data

Prefer storing both for short periods:

```text
raw:<hash>
normalized:<hash>
```

Raw:

```text
TTL 5–30 minutes
```

Normalized:

```text
TTL according to entity
```

Do not permanently store every raw upstream response.

---

# 82. Privacy

For anonymous users, do not persist:

```text
IP
query history
visitorData
cookies
```

unless necessary and explicitly disclosed.

YouTube `visitorData` seen in responses should be treated as request/session metadata rather than music metadata.

---

# 83. Legal / Operational Considerations

InnerTube is an internal/undocumented YouTube interface.

Piped and other projects provide alternative access paths but are also subject to their own operational and legal constraints.

Before production deployment:

1. Review YouTube's current terms and policies.
2. Review each provider's license/terms.
3. Respect upstream rate limits.
4. Avoid redistribution of audio unless you have the rights/authority to do so.
5. Attribute providers where required.
6. Do not disguise or misrepresent the service as official YouTube/Google.

This document intentionally does not claim that use of an undocumented endpoint is authorized merely because it technically works.

---

# 84. Exact Endpoint Reference

## YouTube Music InnerTube

```text
Base:
https://music.youtube.com/youtubei/v1/

Search:
POST https://music.youtube.com/youtubei/v1/search?prettyPrint=false

Browse:
POST https://music.youtube.com/youtubei/v1/browse?prettyPrint=false

Player:
POST https://music.youtube.com/youtubei/v1/player?prettyPrint=false

Next:
POST https://music.youtube.com/youtubei/v1/next?prettyPrint=false
```

Client profile from the live user capture:

```text
WEB_REMIX
1.20260821.01.00
```

## Piped

Example documented base:

```text
https://pipedapi.kavin.rocks
```

Streams:

```text
GET https://pipedapi.kavin.rocks/streams/<videoId>
```

Instance list:

```text
https://github.com/TeamPiped/Piped/wiki/Instances
```

## LRCLIB

```text
Base:
https://lrclib.net/api

Lookup:
GET https://lrclib.net/api/get

Track ID:
GET https://lrclib.net/api/get/<id>

Search:
GET https://lrclib.net/api/search
```

## SponsorBlock

```text
Base:
https://sponsor.ajay.app

API documentation:
https://github.com/ajayyy/SponsorBlock/wiki/API-Docs
```

## Kugou

Common documented/reverse-engineered endpoints:

```text
http://msearchcdn.kugou.com/api/v3/search/song

http://mobilecdn.kugou.com/api/v3/search/song

http://mobilecdn.kugou.com/api/v3/search/special

http://msearch.kugou.com/api/v3/search/mv

http://msearch.kugou.com/api/v3/search/album

http://mobileservice.kugou.com/api/v3/lyric/search
```

These Kugou URLs should be treated as version-sensitive and verified before production use.

---

# 85. Example Complete Backend Response

Request:

```http
GET /v1/search?q=Daft%20Punk&type=song
```

Response:

```json
{
  "data": {
    "items": [
      {
        "id": "khnokW3Mw24",
        "provider": "youtube",
        "title": "Instant Crush (feat. Julian Casablancas)",
        "artists": [
          {
            "id": "UCRr1xG_2WIDs18a6cIiCxeA",
            "name": "Daft Punk"
          }
        ],
        "album": {
          "id": "MPREb_K8qWMWVqXGi",
          "name": "Random Access Memories"
        },
        "durationMs": 338000,
        "thumbnails": [
          {
            "url": "https://...",
            "width": 120,
            "height": 120
          }
        ],
        "type": "song",
        "playCount": 1200000000
      },
      {
        "id": "ZFZM6jDTWd4",
        "provider": "youtube",
        "title": "Giorgio by Moroder",
        "artists": [
          {
            "id": "UCRr1xG_2WIDs18a6cIiCxeA",
            "name": "Daft Punk"
          }
        ],
        "durationMs": 545000,
        "type": "song",
        "playCount": 94000000
      },
      {
        "id": "4D7u5KF7SP8",
        "provider": "youtube",
        "title": "Get Lucky (feat. Pharrell Williams and Nile Rodgers)",
        "artists": [
          {
            "id": "UCRr1xG_2WIDs18a6cIiCxeA",
            "name": "Daft Punk"
          }
        ],
        "durationMs": 370000,
        "type": "song",
        "playCount": 1800000000
      }
    ]
  },
  "meta": {
    "provider": "innertube",
    "requestId": "req_123",
    "continuation": null
  }
}
```

---

# 86. Minimal MVP

Build in this order:

```text
Phase 1
  InnerTube search
  InnerTube browse
  normalization
  Redis cache

Phase 2
  InnerTube player
  playback schema
  stream validation

Phase 3
  LRCLIB
  lyrics normalization

Phase 4
  Piped fallback
  Piped instance health

Phase 5
  Kugou fallback
  provider matching

Phase 6
  SponsorBlock
  translation

Phase 7
  observability
  circuit breakers
  background health checks
```

---

# 87. Production-Grade Version

```text
                ┌──────────────┐
                │ CDN / Proxy  │
                └──────┬───────┘
                       │
                ┌──────▼───────┐
                │ API Gateway  │
                └──────┬───────┘
                       │
       ┌───────────────┼────────────────┐
       │               │                │
       ▼               ▼                ▼
   Search API      Metadata API      Playback API
       │               │                │
       └───────────────┼────────────────┘
                       ▼
                 Provider Router
                       │
        ┌──────────────┼───────────────────┐
        ▼              ▼                   ▼
    InnerTube         Piped               Kugou
        │              │                   │
        ├──────────────┴──────────────┐    │
        │                             │    │
        ▼                             ▼    ▼
     Redis                         LRCLIB SponsorBlock
        │
        ▼
   PostgreSQL
        │
        ▼
  OpenTelemetry
```

---

# 88. Critical Failure Modes to Test

## YouTube/InnerTube

```text
client version rejected
401/403
429
HTML instead of JSON
empty search
unknown renderer
continuation missing
browse page layout changed
player unavailable
region restriction
login/premium restriction
```

## Piped

```text
instance offline
proxy offline
stream URL expired
upstream 403
bad JSON
empty audioStreams
```

## LRCLIB

```text
404
no synchronized lyrics
no lyrics
duration mismatch
rate limiting
```

## Kugou

```text
endpoint version changed
Chinese metadata encoding
empty result
region restriction
schema variation
```

## SponsorBlock

```text
no segments
rate limiting
new category
invalid video ID
```

---

# 89. Testing Checklist

```text
[ ] Search "Daft Punk"
[ ] Search "Blinding Lights"
[ ] Search non-English query
[ ] Search typo
[ ] Search empty query
[ ] Search pagination
[ ] Fetch artist
[ ] Fetch album
[ ] Fetch playlist
[ ] Fetch song
[ ] Resolve stream
[ ] Expire stream cache
[ ] Piped fallback
[ ] yt-dlp fallback
[ ] Fetch lyrics
[ ] Fetch synced lyrics
[ ] Lyrics miss
[ ] SponsorBlock lookup
[ ] Translation
[ ] Provider outage
[ ] Redis outage
[ ] PostgreSQL outage
[ ] YouTube renderer unknown
[ ] Rate limiting
[ ] Concurrent requests
```

---

# 90. Final Implementation Principle

The most important implementation decision is:

```text
Do NOT build:
Your API = YouTube API
```

Build:

```text
Your API
   ↓
Stable domain models
   ↓
Provider adapters
   ↓
External upstreams
```

That makes the backend resilient to upstream changes.

For example:

```text
GET /v1/search
        │
        ▼
SearchService
        │
        ▼
ProviderRouter
        │
        ├── InnerTube
        ├── Piped
        └── Kugou
        │
        ▼
Normalized SearchResult[]
```

Similarly:

```text
GET /v1/lyrics
        ↓
LyricsService
        ↓
LRCLIB
        ↓
Kugou fallback
        ↓
Normalized Lyrics
```

and:

```text
GET /v1/streams/:videoId
        ↓
PlaybackService
        ↓
InnerTube
        ↓
Piped
        ↓
yt-dlp
        ↓
Normalized PlaybackInfo
```

---

# 91. Source Register

## ViTune repository

Repository:

```text
https://github.com/bartoostveen/ViTune
```

Relevant source/configuration:

```text
settings.gradle.kts
app/build.gradle.kts
providers/*
```

The repository configuration confirms the provider modules and the embedded yt-dlp/yt-dlp-ejs integration.

## Piped API

```text
https://docs.piped.video/docs/api-documentation/
```

## LRCLIB

```text
https://lrclib.net/api
```

Current route/documentation examples:

```text
https://github.com/junago15/lrclib-edit
https://github.com/Dr-Blank/lrclibapi
```

## SponsorBlock

```text
https://sponsor.ajay.app
https://github.com/ajayyy/SponsorBlock/wiki/API-Docs
```

## Kugou reverse-engineered API references

```text
https://github.com/keyule/KuGou-API
```

## InnerTube ecosystem reference

```text
https://github.com/LuanRT/YouTube.js
```

---

# 92. Accuracy Notes

The following are **directly supported** by the material examined:

- ViTune has an InnerTube provider.
- ViTune has Piped, Kugou, LRCLIB, SponsorBlock, Translate, and GitHub provider modules.
- ViTune bundles yt-dlp and yt-dlp-ejs.
- The supplied successful YouTube Music search capture uses `WEB_REMIX`.
- The supplied capture reports client version `1.20260821.01.00`.
- The supplied response contains multiple music results with video IDs and music metadata.
- Piped documents `/streams/:videoId` and instance-specific API base URLs.
- LRCLIB documents `/api/get`, `/api/get/:track_id`, and `/api/search`.
- SponsorBlock exposes a public API at `sponsor.ajay.app`.
- Kugou has publicly documented/reverse-engineered search endpoints.

The following are **not asserted as exact ViTune internals here** because the surfaced repository evidence did not expose enough source detail:

- the exact current InnerTube request header set used by ViTune itself;
- the exact `providers:translate` upstream;
- every exact Kugou request payload used by the current ViTune version;
- every exact fallback priority used at runtime for every capability;
- every exact HTTP timeout/cache value used by ViTune.

Those should be obtained by inspecting the corresponding provider source files or tracing a running ViTune APK if byte-for-byte/runtime parity is required.

---

# 93. Practical Next Step

For an actual implementation, start with these exact modules:

```text
src/providers/innertube/
  client.ts
  context.ts
  search.ts
  browse.ts
  player.ts
  next.ts
  parsers/
    search.ts
    browse.ts
    player.ts

src/providers/piped/
  client.ts
  instance-discovery.ts
  streams.ts

src/providers/lrclib/
  client.ts
  lyrics.ts

src/providers/kugou/
  client.ts
  search.ts
  lyrics.ts

src/providers/sponsorblock/
  client.ts
  segments.ts

src/providers/translate/
  client.ts

src/services/
  search.ts
  metadata.ts
  playback.ts
  lyrics.ts
  sponsors.ts

src/cache/
  redis.ts

src/http/
  routes.ts
  errors.ts
  rate-limit.ts
```

The implementation should then be validated against the live `Daft Punk` capture before adding additional providers.
