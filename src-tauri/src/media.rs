//! 媒体层：封面解密 + HLS 代理。
//!
//! 站点封面是 AES-128-CBC 密文，m3u8 内部分片/key 又可能被 WebView 的 CORS 拦住，
//! 所以图片与视频统一走本地自定义协议（Windows/Android 为 `http://hg.localhost`，
//! macOS 为 `hg://localhost`，见 [`LOCAL_ORIGIN`]）。

use crate::net::fetch_bytes;
use aes::cipher::{BlockDecrypt, KeyInit};
use aes::Aes128;
use once_cell::sync::Lazy;
use percent_encoding::{percent_decode_str, utf8_percent_encode, NON_ALPHANUMERIC};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, serde::Deserialize, serde::Serialize)]
pub struct ByteRange { pub start: u64, pub end: u64 }

static OBSERVED_PLAYLISTS: Lazy<Mutex<HashMap<String, std::time::Instant>>> = Lazy::new(|| Mutex::new(HashMap::new()));
fn playlist_id(url: &str) -> String { format!("{:x}", Sha256::digest(url.as_bytes())) }
fn observe_playlist(url: &str) {
    if let Ok(mut entries) = OBSERVED_PLAYLISTS.lock() {
        entries.retain(|_, at| at.elapsed() < std::time::Duration::from_secs(300));
        if entries.len() >= 256 { entries.clear(); }
        entries.insert(playlist_id(url), std::time::Instant::now());
    }
}
pub fn latest_playlist(urls: &[String]) -> Option<usize> {
    let entries = OBSERVED_PLAYLISTS.lock().ok()?;
    urls.iter().enumerate().filter_map(|(i, url)| entries.get(&playlist_id(url)).filter(|at| at.elapsed() < std::time::Duration::from_secs(300)).map(|at| (i, at)))
        .max_by_key(|(_, at)| **at).map(|(i, _)| i)
}

const DIRECT_BLOCK: u64 = 1024 * 1024;
#[derive(Clone)]
struct DirectInfo { total: u64, cursor: u64, request_id: u64, etag: String, tag: String }
static DIRECT_MEDIA: Lazy<Mutex<HashMap<String, DirectInfo>>> = Lazy::new(|| Mutex::new(HashMap::new()));
static DIRECT_PROBE_FAILURES: Lazy<Mutex<HashMap<String, std::time::Instant>>> = Lazy::new(|| Mutex::new(HashMap::new()));
static DIRECT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

pub async fn probe_direct(url: &str, urgent: bool) -> Result<(u64, String), String> {
    let target = url.to_string();
    let object = crate::media_download::shared(format!("probe:{}:{url}", MEMORY_REVISION.load(Ordering::SeqCst)), urgent, move |transfer| async move {
        let referer = format!("{}/", crate::net::base());
        let mut response = transfer.run(crate::net::get_with_range(&target, std::time::Duration::from_secs(20), None, Some(&referer), Some("bytes=0-0"))).await?;
        if response.status() != reqwest::StatusCode::PARTIAL_CONTENT { return Err("服务器不支持范围请求".into()); }
        let (start, end, total) = response.headers().get(reqwest::header::CONTENT_RANGE).and_then(|value| value.to_str().ok())
            .and_then(parse_content_range).ok_or("无效的 Content-Range")?;
        if start != 0 || end != 0 || total > 9_007_199_254_740_991 { return Err("服务器返回的字节范围不匹配".into()); }
        let etag = response.headers().get(reqwest::header::ETAG).and_then(|value| value.to_str().ok())
            .filter(|tag| tag.starts_with('"') && tag.ends_with('"')).ok_or("服务器未提供强 ETag，无法安全复用文件块")?.to_string();
        let mut bytes = Vec::new();
        while let Some(chunk) = transfer.run(response.chunk()).await.map_err(|_| "范围探测失败")? {
            if chunk.len() > 1 - bytes.len() { return Err("范围探测响应长度错误".into()); }
            bytes.extend_from_slice(&chunk);
        }
        if bytes.len() != 1 { return Err("范围探测响应长度错误".into()); }
        let tag = format!("{:x}", Sha256::digest(etag.as_bytes()));
        let mut direct = DIRECT_MEDIA.lock().map_err(|_| "范围状态锁失败")?;
        if direct.len() >= 128 && !direct.contains_key(&target) { direct.clear(); }
        let (cursor, request_id) = direct.get(&target).filter(|entry| entry.tag == tag && entry.total == total)
            .map_or((0, 0), |entry| (entry.cursor, entry.request_id));
        direct.insert(target, DirectInfo { total, cursor, request_id, etag: etag.clone(), tag });
        Ok(crate::media_download::Object { mime: None, total: Some(total), etag: Some(etag), range: None, bytes: Arc::new(bytes) })
    }).await?;
    let tag = format!("{:x}", Sha256::digest(object.etag.ok_or("缺少 ETag")?.as_bytes()));
    Ok((object.total.ok_or("缺少文件大小")?, tag))
}

pub fn direct_cursor(url: &str) -> u64 {
    DIRECT_MEDIA.lock().ok().and_then(|map| map.get(url).map(|entry| entry.cursor)).unwrap_or(0)
}
fn advance_direct_cursor(target: &str, request_id: u64, next_byte: u64) {
    // A response from before a seek can finish after the latest range request.
    if let Ok(mut map) = DIRECT_MEDIA.lock() {
        if let Some(info) = map.get_mut(target).filter(|info| info.request_id == request_id) {
            info.cursor = next_byte;
        }
    }
}
fn direct_cache_key(target: &str, range: ByteRange, tag: &str) -> String {
    format!("{}\nvalidator={tag}", range_key(target, range))
}
fn direct_cache_read(target: &str, requested: ByteRange, total: u64, tag: &str) -> Option<(Option<String>, Vec<u8>)> {
    if requested.start >= total { return None; }
    let end = requested.end.min(total - 1).min(requested.start.saturating_add(8 * 1024 * 1024 - 1));
    if end < requested.start { return None; }
    let mut offset = requested.start;
    let mut result = Vec::new(); let mut content_type = None;
    while offset <= end {
        let block_start = offset / DIRECT_BLOCK * DIRECT_BLOCK;
        let block_end = (block_start + DIRECT_BLOCK - 1).min(total - 1);
        let key = direct_cache_key(target, ByteRange { start: block_start, end: block_end }, tag);
        let entry = crate::media_cache::read_range(&key, offset - block_start, end.min(block_end) - block_start);
        let Some((mime, saved_total, bytes)) = entry else { break };
        if saved_total != total || bytes.is_empty() { break; }
        if content_type.is_none() { content_type = mime; }
        offset += bytes.len() as u64;
        result.extend_from_slice(&bytes);
    }
    if result.is_empty() { None } else { Some((content_type, result)) }
}

fn direct_cache_group(target: &str, info: &DirectInfo) -> String {
    format!("{target}\nvalidator={}\ntotal={}", info.tag, info.total)
}
pub fn direct_pending(target: &str) -> Vec<ByteRange> {
    let info = DIRECT_MEDIA.lock().ok().and_then(|map| map.get(target).cloned());
    info.map_or_else(Vec::new, |info| crate::media_cache::partial_blocks(&direct_cache_group(target, &info))
        .into_iter().map(|(start, end)| ByteRange { start, end }).collect())
}

// Disk identity stays at 1 MiB. Small shared transfers bound the work preceding an urgent seek.
const DIRECT_TRANSFER: u64 = 64 * 1024;
async fn direct_bytes(target: &str, info: &DirectInfo, requested: ByteRange, urgent: bool, generation: u64, protected: &[String]) -> Result<(Option<String>, Vec<u8>), String> {
    let block_start = requested.start / DIRECT_BLOCK * DIRECT_BLOCK;
    let block = ByteRange { start: block_start, end: (block_start + DIRECT_BLOCK - 1).min(info.total.saturating_sub(1)) };
    if requested.start > requested.end || requested.end > block.end { return Err("无效的文件块范围".into()); }
    let key = direct_cache_key(target, block, &info.tag);
    let mut offset = requested.start;
    let mut bytes = Vec::new();
    let mut mime = None;
    while offset <= requested.end {
        if let Some((cached_mime, total, cached)) = crate::media_cache::read_range(&key, offset - block_start, requested.end - block_start) {
            if total == info.total && !cached.is_empty() {
                mime = cached_mime.or(mime); offset += cached.len() as u64; bytes.extend_from_slice(&cached); continue;
            }
        }
        let end = requested.end.min((offset / DIRECT_TRANSFER + 1) * DIRECT_TRANSFER - 1);
        let target = target.to_string(); let key = key.clone(); let info = info.clone(); let protected = protected.to_vec();
        let transfer_key = format!("direct:{generation}:{key}:{}", offset / DIRECT_TRANSFER);
        let object = crate::media_download::shared(transfer_key, urgent, move |transfer| async move {
            let start = offset;
            let mut cursor = start;
            let mut received = Vec::new();
            let mut mime = None;
            while cursor <= end {
                if let Some((cached_mime, total, cached)) = crate::media_cache::read_range(&key, cursor - block_start, end - block_start) {
                    if total == info.total && !cached.is_empty() {
                        mime = cached_mime.or(mime); cursor += cached.len() as u64; received.extend_from_slice(&cached); continue;
                    }
                }
                let gap_end = block_start + crate::media_cache::gap_end(&key, cursor - block_start, end - block_start);
                let header = format!("bytes={cursor}-{gap_end}");
                let referer = format!("{}/", crate::net::base());
                let mut response = transfer.run(crate::net::get_with_range(&target, std::time::Duration::from_secs(20), None, Some(&referer), Some(&header))).await?;
                if response.status() != reqwest::StatusCode::PARTIAL_CONTENT { return Err("服务器不支持字节范围".into()); }
                let actual = response.headers().get(reqwest::header::CONTENT_RANGE).and_then(|value| value.to_str().ok()).and_then(parse_content_range);
                if actual != Some((cursor, gap_end, info.total)) || response.headers().get(reqwest::header::ETAG).and_then(|value| value.to_str().ok()) != Some(info.etag.as_str()) {
                    if let Ok(mut map) = DIRECT_MEDIA.lock() {
                        if map.get(&target).is_some_and(|entry| entry.tag == info.tag) { map.remove(&target); }
                    }
                    return Err("媒体文件已改变或字节范围不匹配".into());
                }
                mime = Some(media_mime(&target, response.headers().get(reqwest::header::CONTENT_TYPE).and_then(|value| value.to_str().ok()).map(str::to_string)));
                let gap_start = cursor;
                let mut fragment = Vec::new();
                while let Some(chunk) = transfer.run(response.chunk()).await.map_err(|_| "文件块下载失败")? {
                    if chunk.len() as u64 > gap_end + 1 - cursor { return Err("字节范围响应长度错误".into()); }
                    cursor += chunk.len() as u64;
                    fragment.extend_from_slice(&chunk);
                }
                if cursor != gap_end + 1 { return Err("字节范围响应长度错误".into()); }
                crate::media_cache::write_range(&key, &direct_cache_group(&target, &info), block_start, block.end - block_start + 1,
                    gap_start - block_start, &fragment, mime.clone(), info.total, generation, &protected);
                received.extend_from_slice(&fragment);
            }
            Ok(crate::media_download::Object { mime, total: Some(info.total), etag: None, range: Some(ByteRange { start, end }), bytes: Arc::new(received) })
        }).await?;
        let range = object.range.ok_or("缺少文件块范围")?;
        if range.start <= offset && range.end >= offset {
            let first = (offset - range.start) as usize;
            let last = (requested.end.min(range.end) - range.start + 1) as usize;
            bytes.extend_from_slice(&object.bytes[first..last]);
            offset += (last - first) as u64;
            mime = object.mime.or(mime);
        }
    }
    Ok((mime, bytes))
}

fn direct_cached_response(target: &str, info: &DirectInfo, start: u64, bounded: Option<ByteRange>) -> Option<Response<Vec<u8>>> {
    let requested = ByteRange { start, end: bounded.map_or(info.total.saturating_sub(1), |range| range.end) };
    let (mime, bytes) = direct_cache_read(target, requested, info.total, &info.tag)?;
    let end = start + bytes.len() as u64 - 1;
    advance_direct_cursor(target, info.request_id, end + 1);
    Some(byte_response(206, mime.as_deref(), Some(&format!("bytes {start}-{end}/{}", info.total)), bytes))
}

fn range_key(url: &str, range: ByteRange) -> String {
    format!("{url}\nrange={}-{}", range.start, range.end)
}

fn parse_content_range(raw: &str) -> Option<(u64, u64, u64)> {
    let (range, total) = raw.strip_prefix("bytes ")?.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let (start, end, total) = (start.parse().ok()?, end.parse().ok()?, total.parse().ok()?);
    if start > end || end >= total { return None; }
    Some((start, end, total))
}

fn valid_range_header(raw: &str) -> bool {
    if raw.len() > 100 { return false; }
    let Some(ranges) = raw.strip_prefix("bytes=") else { return false };
    ranges.split(',').all(|range| {
        let Some((start, end)) = range.split_once('-') else { return false };
        if start.is_empty() { return end.parse::<u64>().is_ok_and(|n| n > 0); }
        let Ok(start) = start.parse::<u64>() else { return false };
        end.is_empty() || end.parse::<u64>().is_ok_and(|end| start <= end)
    })
}

fn parse_bounded_range(raw: &str) -> Option<ByteRange> {
    let (start, end) = raw.strip_prefix("bytes=")?.split_once('-')?;
    let (start, end) = (start.parse().ok()?, end.parse().ok()?);
    if start > end { return None; }
    Some(ByteRange { start, end })
}

fn media_mime(url: &str, upstream: Option<String>) -> String {
    if let Some(mime) = upstream.filter(|mime| !mime.eq_ignore_ascii_case("application/octet-stream")) { return mime; }
    let path = url.split(['?', '#']).next().unwrap_or(url).to_ascii_lowercase();
    let mime = if path.ends_with(".vtt") { "text/vtt" }
        else if path.ends_with(".aac") { "audio/aac" }
        else if path.ends_with(".m4a") { "audio/mp4" }
        else if path.ends_with(".mp3") { "audio/mpeg" }
        else if path.ends_with(".ts") { "video/mp2t" }
        else if path.ends_with(".m4s") || path.ends_with(".mp4") { "video/mp4" }
        else { "application/octet-stream" };
    mime.to_string()
}

async fn shared_segment(url: &str, range: Option<ByteRange>, urgent: bool) -> Result<crate::media_download::Object, String> {
    let revision = MEMORY_REVISION.load(Ordering::SeqCst);
    let cache_key = range.map(|range| range_key(url, range)).unwrap_or_else(|| url.to_string());
    let url = url.to_string();
    crate::media_download::shared(format!("{revision}:{cache_key}"), urgent, move |transfer| async move {
        if let Some((mime, total, bytes)) = crate::media_cache::get_entry(&cache_key) {
            return Ok(crate::media_download::Object { mime, total, etag: None, range, bytes: Arc::new(bytes) });
        }
        if range.is_none() {
            let memory = KEY_CACHE.lock().ok().and_then(|cache| cache.get(&url).cloned())
                .or_else(|| SEG_CACHE.lock().ok().and_then(|cache| cache.get(&url).cloned()));
            if let Some((mime, bytes)) = memory {
                return Ok(crate::media_download::Object { mime: Some(mime), total: None, etag: None, range, bytes });
            }
        }
        let (mime, total, bytes) = if range.is_none() && urgent {
            let (mime, bytes) = transfer.run(fetch_bytes(&url)).await?;
            (mime, None, bytes)
        } else {
            let referer = format!("{}/", crate::net::base());
            let header = range.map(|range| format!("bytes={}-{}", range.start, range.end));
            let mut response = transfer.run(crate::net::get_with_range(&url, std::time::Duration::from_secs(20), None, Some(&referer), header.as_deref())).await?;
            if !response.status().is_success() { return Err(format!("HTTP {}", response.status())); }
            let total = if let Some(range) = range {
                if response.status() != reqwest::StatusCode::PARTIAL_CONTENT { return Err("服务器不支持字节范围".into()); }
                let (start, end, total) = response.headers().get(reqwest::header::CONTENT_RANGE).and_then(|value| value.to_str().ok())
                    .and_then(parse_content_range).ok_or("无效的 Content-Range")?;
                if start != range.start || end != range.end { return Err("服务器返回的字节范围不匹配".into()); }
                Some(total)
            } else { None };
            let mime = response.headers().get(reqwest::header::CONTENT_TYPE).and_then(|value| value.to_str().ok()).map(str::to_string);
            let mut bytes = Vec::new();
            while let Some(chunk) = transfer.run(response.chunk()).await.map_err(|_| "分片下载失败")? {
                if !transfer.is_urgent() && bytes.len() + chunk.len() > 16 * 1024 * 1024 { return Err("缓存块超过 16 MiB".into()); }
                bytes.extend_from_slice(&chunk);
            }
            if range.is_some_and(|range| bytes.len() as u64 != range.end - range.start + 1) { return Err("字节范围响应长度不匹配".into()); }
            (mime, total, bytes)
        };
        let mime = media_mime(&url, mime);
        let bytes = Arc::new(bytes);
        let playlist = mime.to_ascii_lowercase().contains("mpegurl") || bytes.starts_with(b"#EXTM3U") || bytes.starts_with(b"\xef\xbb\xbf#EXTM3U");
        if range.is_none() && !playlist && MEMORY_REVISION.load(Ordering::SeqCst) == revision {
            if bytes.len() <= 65536 { cache_put(&KEY_CACHE, url.clone(), (mime.clone(), bytes.clone()), KEY_CACHE_MAX); }
            else if bytes.len() <= SEG_CACHE_MAX_BYTES { cache_put(&SEG_CACHE, url.clone(), (mime.clone(), bytes.clone()), SEG_CACHE_MAX); }
        }
        Ok(crate::media_download::Object { mime: Some(mime), total, etag: None, range, bytes })
    }).await
}

pub async fn prefetch_segment(url: &str, range: Option<ByteRange>, validator: Option<&str>, protected_urls: &[String], focus: Option<u64>) -> Result<bool, String> {
    crate::network_policy::parse_target(url)?;
    let Some(generation) = crate::media_cache::generation() else { return Ok(false) };
    if range.is_some_and(|range| range.start > range.end || range.end.saturating_sub(range.start) >= 16 * 1024 * 1024) { return Err("无效的缓存范围".into()); }
    if let Some(validator) = validator {
        let info = DIRECT_MEDIA.lock().ok().and_then(|map| map.get(url).cloned()).ok_or("媒体文件需要重新探测")?;
        if info.tag != validator { return Err("媒体文件已改变，请重新加载".into()); }
        let range = range.ok_or("缺少文件块范围")?;
        if range.start % DIRECT_BLOCK != 0 || range.end != (range.start + DIRECT_BLOCK - 1).min(info.total.saturating_sub(1)) { return Err("文件块边界不匹配".into()); }
        let start = focus.filter(|&focus| focus >= range.start && focus <= range.end).unwrap_or(range.start);
        if crate::media_cache::contains(&direct_cache_key(url, range, &info.tag)) { return Ok(true); }
        direct_bytes(url, &info, ByteRange { start, end: range.end }, false, generation, protected_urls).await?;
        let key = direct_cache_key(url, range, &info.tag);
        return Ok(crate::media_cache::read_range(&key, start - range.start, range.end - range.start)
            .is_some_and(|(_, _, bytes)| bytes.len() as u64 == range.end - start + 1));
    }
    let key = range.map(|range| range_key(url, range)).unwrap_or_else(|| url.to_string());
    if crate::media_cache::contains(&key) { return Ok(true); }
    let object = shared_segment(url, range, false).await?;
    if object.bytes.len() > 16 * 1024 * 1024 { return Err("缓存块超过 16 MiB".into()); }
    Ok(match object.total {
        Some(total) => crate::media_cache::put_entry(&key, object.mime, Some(total), &object.bytes, generation, protected_urls),
        None => crate::media_cache::put(&key, object.mime, &object.bytes, generation, protected_urls),
    })
}

use tauri::http::{Response, StatusCode};

/// 本平台自定义协议的完整基址。
///
/// Tauri 的自定义协议地址随平台不同：Windows / Android 是 `http://<scheme>.localhost`，
/// macOS / iOS / Linux 是 `<scheme>://localhost`。m3u8 里的分片与 key 地址按它改写。
/// 前端 `src/api.ts` 的 `LOCAL` 必须与此一致，否则封面与视频全部加载失败。
#[cfg(any(target_os = "windows", target_os = "android"))]
pub const LOCAL_ORIGIN: &str = "http://hg.localhost";
#[cfg(not(any(target_os = "windows", target_os = "android")))]
pub const LOCAL_ORIGIN: &str = "hg://localhost";

const IMG_KEY: &[u8] = b"f5d965df75336270";
const IMG_IV: &[u8] = b"97b60394abc2fbe1";

static MEMORY_REVISION: AtomicU64 = AtomicU64::new(0);
static IMG_CACHE: Lazy<Mutex<HashMap<String, Arc<Vec<u8>>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
static PL_CACHE: Lazy<Mutex<HashMap<String, (std::time::Instant, Arc<String>)>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
static KEY_CACHE: Lazy<Mutex<HashMap<String, (String, Arc<Vec<u8>>)>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
static SEG_CACHE: Lazy<Mutex<HashMap<String, (String, Arc<Vec<u8>>)>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

const IMG_CACHE_MAX: usize = 240;
const PL_CACHE_MAX: usize = 60;
const KEY_CACHE_MAX: usize = 120;
const SEG_CACHE_MAX: usize = 36;
const SEG_CACHE_MAX_BYTES: usize = 4 * 1024 * 1024;

fn cache_put<V: Clone>(map: &Mutex<HashMap<String, V>>, key: String, val: V, max: usize) {
    let mut m = map.lock().unwrap();
    if m.len() >= max {
        if let Some(k) = m.keys().next().cloned() {
            m.remove(&k);
        }
    }
    m.insert(key, val);
}

/* ───────────────────────── AES ───────────────────────── */

fn aes_cbc_decrypt(data: &[u8]) -> Option<Vec<u8>> {
    if data.is_empty() || data.len() % 16 != 0 {
        return None;
    }
    let cipher = Aes128::new_from_slice(IMG_KEY).ok()?;
    let mut prev = [0u8; 16];
    prev.copy_from_slice(IMG_IV);
    let mut out = Vec::with_capacity(data.len());
    for chunk in data.chunks(16) {
        let mut ct = [0u8; 16];
        ct.copy_from_slice(chunk);
        let mut block = ct;
        cipher.decrypt_block((&mut block).into());
        for i in 0..16 {
            out.push(block[i] ^ prev[i]);
        }
        prev = ct;
    }
    Some(out)
}

fn strip_padding(mut buf: Vec<u8>) -> Vec<u8> {
    let pad = *buf.last().unwrap_or(&0) as usize;
    if pad > 0 && pad <= 16 && buf.len() >= pad {
        if buf[buf.len() - pad..].iter().all(|b| *b as usize == pad) {
            buf.truncate(buf.len() - pad);
        }
    }
    buf
}

fn image_kind(buf: &[u8]) -> Option<&'static str> {
    if buf.len() < 12 {
        return None;
    }
    if buf[0] == 0xFF && buf[1] == 0xD8 {
        return Some("image/jpeg");
    }
    if buf.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
        return Some("image/png");
    }
    if buf.starts_with(b"GIF8") {
        return Some("image/gif");
    }
    if buf.starts_with(b"RIFF") && &buf[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
}

/// 取封面：命中缓存直接返回，否则下载并按需解密。
pub async fn load_image(url: &str) -> Result<(String, Arc<Vec<u8>>), String> {
    let revision = MEMORY_REVISION.load(Ordering::SeqCst);
    if let Some(hit) = IMG_CACHE.lock().unwrap().get(url).cloned() {
        let kind = image_kind(&hit).unwrap_or("image/jpeg");
        return Ok((kind.to_string(), hit));
    }
    let (_, bytes) = fetch_bytes(url).await?;
    let plain = if image_kind(&bytes).is_some() {
        bytes
    } else {
        let dec = aes_cbc_decrypt(&bytes).ok_or_else(|| "封面解密失败".to_string())?;
        strip_padding(dec)
    };
    let kind = image_kind(&plain).unwrap_or("image/jpeg").to_string();
    let arc = Arc::new(plain);
    if MEMORY_REVISION.load(Ordering::SeqCst) == revision { cache_put(&IMG_CACHE, url.to_string(), arc.clone(), IMG_CACHE_MAX); }
    Ok((kind, arc))
}

/* ───────────────────────── HLS ───────────────────────── */

fn enc(s: &str) -> String {
    utf8_percent_encode(s, NON_ALPHANUMERIC).to_string()
}

fn dec(s: &str) -> String {
    percent_decode_str(s).decode_utf8_lossy().to_string()
}

fn absolutize(base: &str, target: &str) -> String {
    if target.starts_with("http://") || target.starts_with("https://") {
        return target.to_string();
    }
    if let Ok(b) = url::Url::parse(base) {
        if let Ok(j) = b.join(target) {
            return j.to_string();
        }
    }
    target.to_string()
}

/// 属性列表中的引号内可含逗号，不能直接 split(',')。
fn hls_attributes(line: &str) -> Option<(&str, Vec<&str>)> {
    let (tag, rest) = line.split_once(':')?;
    let mut attributes = Vec::new();
    let mut quoted = false;
    let mut start = 0;
    for (i, ch) in rest.char_indices() {
        if ch == '"' {
            quoted = !quoted;
        }
        if ch == ',' && !quoted {
            attributes.push(&rest[start..i]);
            start = i + 1;
        }
    }
    attributes.push(&rest[start..]);
    Some((tag, attributes))
}

fn map_uri_attributes(line: &str, mut rewrite: impl FnMut(&str) -> String) -> String {
    let Some((tag, attributes)) = hls_attributes(line) else {
        return line.to_string();
    };
    let mapped: Vec<String> = attributes
        .into_iter()
        .map(|attribute| {
            if let Some((key, value)) = attribute.split_once('=') {
                if key.trim() == "URI" {
                    if let Some(uri) = value
                        .trim()
                        .strip_prefix('"')
                        .and_then(|v| v.strip_suffix('"'))
                    {
                        return format!("{key}=\"{}\"", rewrite(uri));
                    }
                }
            }
            attribute.to_string()
        })
        .collect();
    format!("{tag}:{}", mapped.join(","))
}

fn rewrite_uri_attr(line: &str, prefix: &str) -> String {
    map_uri_attributes(line, |uri| format!("{prefix}/seg?u={}", enc(uri)))
}

/// 取 m3u8 并把 key / 子列表 / 分片地址改写到本地协议。
pub async fn load_playlist(url: &str, prefix: &str) -> Result<Arc<String>, String> {
    let _foreground = crate::media_download::foreground();
    let cache_key = format!("{prefix}|{url}");
    let revision = MEMORY_REVISION.load(Ordering::SeqCst);
    if let Some((expires, hit)) = PL_CACHE.lock().unwrap().get(&cache_key).cloned() {
        if std::time::Instant::now() < expires { return Ok(hit); }
    }
    let (_, bytes) = fetch_bytes(url).await?;
    let text = std::str::from_utf8(&bytes).map_err(|_| "播放列表不是 UTF-8 文本")?;
    if !text.trim_start_matches('\u{feff}').starts_with("#EXTM3U") {
        return Err("响应不是 HLS 播放列表".into());
    }
    let arc = Arc::new(rewrite_playlist(url, text, prefix));
    let ttl = if text.lines().any(|line| line.trim() == "#EXT-X-ENDLIST") { 300 } else { 2 };
    if MEMORY_REVISION.load(Ordering::SeqCst) == revision {
        cache_put(&PL_CACHE, cache_key, (std::time::Instant::now() + std::time::Duration::from_secs(ttl), arc.clone()), PL_CACHE_MAX);
    }
    Ok(arc)
}

fn rewrite_playlist(url: &str, text: &str, prefix: &str) -> String {
    let mut out = String::with_capacity(text.len() + 512);
    for raw in text.trim_start_matches('\u{feff}').lines() {
        let line = raw.trim_end();
        if line.is_empty() {
            out.push('\n');
            continue;
        }
        if line.starts_with('#') {
            // 禁用可派生 URL 的内容引导；DATERANGE 保留时间线/SCTE-35 元数据。
            if line.starts_with("#EXT-X-CONTENT-STEERING:") {
                continue;
            }
            if line.starts_with("#EXT-X-DATERANGE:") {
                if let Some((tag, attributes)) = hls_attributes(line) {
                    let safe: Vec<&str> = attributes
                        .into_iter()
                        .filter(|attribute| {
                            let key = attribute
                                .split('=')
                                .next()
                                .unwrap_or("")
                                .trim()
                                .to_ascii_uppercase();
                            !key.contains("URI") && key != "X-ASSET-LIST"
                        })
                        .collect();
                    out.push_str(&format!("{tag}:{}\n", safe.join(",")));
                }
                continue;
            }
            if line.contains("URI=\"") {
                // KEY/MAP/MEDIA/SESSION-KEY/PART/PRELOAD-HINT 等 URI 都必须经过目标检查。
                let absolute = absolutize_uri_in_attr(url, line);
                out.push_str(&rewrite_uri_attr(&absolute, prefix));
            } else {
                out.push_str(line);
            }
        } else {
            let absolute = absolutize(url, line);
            out.push_str(&format!("{prefix}/seg?u={}", enc(&absolute)));
        }
        out.push('\n');
    }

    out
}

fn absolutize_uri_in_attr(base: &str, line: &str) -> String {
    map_uri_attributes(line, |uri| absolutize(base, uri))
}

/// 加载分片或 AES Key（带内存缓存，避免拖动进度条时重复拉取同一 Key 或分片导致卡顿）。
pub async fn load_segment(url: &str) -> Result<(Option<String>, Arc<Vec<u8>>), String> {
    crate::network_policy::parse_target(url)?;
    if let Some((mime, bytes)) = crate::media_cache::get(url) { return Ok((mime, Arc::new(bytes))); }
    let _foreground = crate::media_download::foreground();
    let object = shared_segment(url, None, true).await?;
    Ok((object.mime, object.bytes))
}

/// 清空封面 / 播放列表 / 分片内存缓存
pub fn clear_caches() {
    MEMORY_REVISION.fetch_add(1, Ordering::SeqCst);
    if let Ok(mut g) = IMG_CACHE.lock() { g.clear(); }
    if let Ok(mut g) = PL_CACHE.lock() { g.clear(); }
    if let Ok(mut g) = KEY_CACHE.lock() { g.clear(); }
    if let Ok(mut g) = SEG_CACHE.lock() { g.clear(); }
}

/* ───────────────────────── 协议响应 ───────────────────────── */

fn respond(status: u16, ctype: &str, body: Vec<u8>, cache: &str) -> Response<Vec<u8>> {
    let len = body.len().to_string();
    Response::builder()
        .status(StatusCode::from_u16(status).unwrap_or(StatusCode::OK))
        .header("Content-Type", ctype)
        .header("Content-Length", len)
        .header("Accept-Ranges", "bytes")
        .header("X-Content-Type-Options", "nosniff")
        .header("Access-Control-Expose-Headers", "Content-Length, Content-Type, Accept-Ranges")
        .header("Cache-Control", cache)
        .body(body)
        .unwrap_or_else(|_| Response::new(Vec::new()))
}

/// 主清单的子清单也经 /seg 返回：必须继续改写，不能把内层 URL 原样交给播放器。
fn segment_response(
    target: &str,
    ctype: Option<&str>,
    bytes: Vec<u8>,
    origin: &str,
) -> Response<Vec<u8>> {
    let playlist = ctype.is_some_and(|t| t.to_ascii_lowercase().contains("mpegurl"))
        || bytes.starts_with(b"#EXTM3U")
        || bytes.starts_with(b"\xef\xbb\xbf#EXTM3U");
    if playlist {
        observe_playlist(target);
        return match std::str::from_utf8(&bytes) {
            Ok(text) => respond(
                200,
                "application/vnd.apple.mpegurl",
                rewrite_playlist(target, text, origin).into_bytes(),
                // Media playlists can change even when their URL stays the same.
                "no-store",
            ),
            Err(_) => respond(
                502,
                "text/plain",
                b"invalid playlist encoding".to_vec(),
                "no-store",
            ),
        };
    }
    respond(
        200,
        ctype.unwrap_or("application/octet-stream"),
        bytes,
        if crate::media_cache::generation().is_some() { "no-store" } else { "public, max-age=3600" },
    )
}

async fn range_response(target: &str, raw_range: &str, is_direct: bool) -> Response<Vec<u8>> {
    let _foreground = crate::media_download::foreground();
    let cache_generation = crate::media_cache::generation();
    if !valid_range_header(raw_range) {
        return respond(400, "text/plain", b"invalid range".to_vec(), "no-store");
    }
    if let Err(e) = crate::network_policy::parse_target(target) { return respond(403, "text/plain", e.into_bytes(), "no-store"); }
    let bounded = parse_bounded_range(raw_range);
    let start = if raw_range.contains(',') { None } else { raw_range.strip_prefix("bytes=").and_then(|value| value.split_once('-')).and_then(|(start, _)| start.parse::<u64>().ok()) };
    let request_id = if is_direct && start.is_some() { DIRECT_REQUEST_ID.fetch_add(1, Ordering::Relaxed) } else { 0 };
    let probe_allowed = DIRECT_PROBE_FAILURES.lock().ok().is_none_or(|failures| failures.get(&playlist_id(target)).is_none_or(|at| at.elapsed() >= std::time::Duration::from_secs(30)));
    if is_direct && probe_allowed && crate::media_cache::generation().is_some() && DIRECT_MEDIA.lock().ok().is_some_and(|map| !map.contains_key(target)) {
        // Probe before starting a large open-ended request; unsupported origins retain the original path.
        if probe_direct(target, true).await.is_err() {
            if let Ok(mut failures) = DIRECT_PROBE_FAILURES.lock() {
                failures.retain(|_, at| at.elapsed() < std::time::Duration::from_secs(30));
                if failures.len() >= 128 { failures.clear(); }
                failures.insert(playlist_id(target), std::time::Instant::now());
            }
        }
    }
    let direct = if is_direct && crate::media_cache::generation().is_some() {
        if let Ok(mut map) = DIRECT_MEDIA.lock() {
            map.get_mut(target).map(|entry| {
                if let Some(start) = start.filter(|_| request_id > entry.request_id) {
                    entry.cursor = start;
                    entry.request_id = request_id;
                }
                DirectInfo { request_id, ..entry.clone() }
            })
        } else { None }
    } else { None };
    if let (Some(info), Some(start)) = (&direct, start) {
        if let Some(response) = direct_cached_response(target, info, start, bounded) {
            return response;
        }
        if start < info.total {
            let end = bounded.map_or(info.total - 1, |range| range.end)
                .min(((start / DIRECT_BLOCK + 1) * DIRECT_BLOCK - 1).min(info.total - 1));
            return match direct_bytes(target, info, ByteRange { start, end }, true, cache_generation.unwrap_or(0), &[]).await {
                Ok((mime, bytes)) => {
                    advance_direct_cursor(target, info.request_id, end + 1);
                    byte_response(206, mime.as_deref(), Some(&format!("bytes {start}-{end}/{}", info.total)), bytes)
                }
                Err(error) => respond(502, "text/plain", error.into_bytes(), "no-store"),
            };
        }
    }
    if !is_direct {
        if let Some(range) = bounded {
            return match shared_segment(target, Some(range), true).await {
                Ok(object) => {
                    let total = object.total.unwrap();
                    if let Some(generation) = cache_generation {
                        if object.bytes.len() <= 16 * 1024 * 1024 {
                            crate::media_cache::put_entry(&range_key(target, range), object.mime.clone(), Some(total), &object.bytes, generation, &[]);
                        }
                    }
                    byte_response(206, object.mime.as_deref(), Some(&format!("bytes {}-{}/{total}", range.start, range.end)), object.bytes.as_ref().clone())
                }
                Err(error) => respond(502, "text/plain", error.into_bytes(), "no-store"),
            };
        }
    }
    let referer = format!("{}/", crate::net::base());
    match crate::net::get_with_range(target, std::time::Duration::from_secs(20), None, Some(&referer), Some(raw_range)).await {
        Ok(response) => {
            let status = response.status().as_u16();
            let content_range = response.headers().get(reqwest::header::CONTENT_RANGE).and_then(|v| v.to_str().ok()).map(str::to_string);
            let same_direct = direct.as_ref().is_some_and(|info| response.headers().get(reqwest::header::ETAG).and_then(|v| v.to_str().ok()) == Some(info.etag.as_str()));
            if let Some(info) = direct.as_ref().filter(|_| status == 206 && !same_direct) {
                if let Ok(mut map) = DIRECT_MEDIA.lock() {
                    if map.get(target).is_some_and(|entry| entry.tag == info.tag) { map.remove(target); }
                }
            }
            let mime = response.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).map(str::to_string);
            match response.bytes().await {
                Ok(bytes) => {
                    if status == 206 {
                        let valid = if raw_range.contains(',') {
                            mime.as_deref().is_some_and(|value| value.to_ascii_lowercase().starts_with("multipart/byteranges"))
                        } else {
                            let expected = bounded;
                            content_range.as_deref().and_then(parse_content_range).is_some_and(|(actual_start, end, _)| {
                                bytes.len() as u64 == end - actual_start + 1
                                    && start.is_none_or(|requested| actual_start == requested)
                                    && expected.is_none_or(|requested| actual_start == requested.start && end <= requested.end)
                            })
                        };
                        if !valid { return respond(502, "text/plain", b"invalid upstream range".to_vec(), "no-store"); }
                    }
                    if status == 206 {
                        if let (Some(generation), Some((first, last, total))) = (cache_generation, content_range.as_deref().and_then(parse_content_range)) {
                            if let Some(info) = direct.as_ref().filter(|info| same_direct && info.total == total) {
                                // Save complete aligned blocks already fetched for playback, avoiding a second download.
                                let mut offset = first.div_ceil(DIRECT_BLOCK) * DIRECT_BLOCK;
                                let mut blocks = Vec::new();
                                while offset <= last {
                                    let end = offset.saturating_add(DIRECT_BLOCK - 1).min(total - 1);
                                    if end > last { break; }
                                    blocks.push((direct_cache_key(target, ByteRange { start: offset, end }, &info.tag), offset, end));
                                    offset = end + 1;
                                }
                                let protected: Vec<_> = blocks.iter().map(|(key, _, _)| key.clone()).collect();
                                for (key, offset, end) in blocks {
                                    let data = &bytes[(offset - first) as usize..=(end - first) as usize];
                                    crate::media_cache::put_entry(&key, mime.clone(), Some(total), data, generation, &protected);
                                }
                                if start.is_some() {
                                    advance_direct_cursor(target, info.request_id, last + 1);
                                }
                            } else if !is_direct && bounded.is_some_and(|range| range.start == first && range.end == last) && bytes.len() <= 16 * 1024 * 1024 {
                                crate::media_cache::put_entry(&range_key(target, ByteRange { start: first, end: last }), mime.clone(), Some(total), &bytes, generation, &[]);
                            }
                        }
                    }
                    byte_response(status, mime.as_deref(), content_range.as_deref(), bytes.to_vec())
                }
                Err(_) => respond(502, "text/plain", b"range download failed".to_vec(), "no-store"),
            }
        }
        Err(e) => respond(502, "text/plain", e.into_bytes(), "no-store"),
    }
}

fn byte_response(status: u16, mime: Option<&str>, range: Option<&str>, bytes: Vec<u8>) -> Response<Vec<u8>> {
    let mut response = respond(status, mime.unwrap_or("application/octet-stream"), bytes, "no-store");
    if let Some(range) = range.and_then(|r| tauri::http::HeaderValue::from_str(r).ok()) {
        response.headers_mut().insert("Content-Range", range);
    }
    response.headers_mut().insert("Access-Control-Expose-Headers", tauri::http::HeaderValue::from_static("Content-Length, Content-Type, Accept-Ranges, Content-Range"));
    response
}

/// 统一入口：`<LOCAL_ORIGIN>/img?u=` / `hls?u=` / `seg?u=`
///
/// `origin` 是本平台自定义协议的完整基址，见 [`LOCAL_ORIGIN`]；用于改写 m3u8 里的
/// 分片与 key 地址。正常情况下调用方直接传 [`LOCAL_ORIGIN`]。
pub async fn handle(path: &str, query: &str, origin: &str, range: Option<&str>) -> Response<Vec<u8>> {
    let target = query
        .split('&')
        .find_map(|kv| kv.strip_prefix("u="))
        .map(dec)
        .unwrap_or_default();

    if target.is_empty() {
        return respond(400, "text/plain", b"missing u".to_vec(), "no-store");
    }

    let started = std::time::Instant::now();
    let route = match path {
        "/img" => "image",
        "/hls" => "playlist",
        "/seg" => "segment_or_key",
        _ => "unknown",
    };
    crate::playback_log::record(
        "proxy.begin",
        serde_json::json!({"route": route, "resource": crate::playback_log::resource_id(&target)}),
    );
    let response = match path {
        "/img" => match load_image(&target).await {
            Ok((kind, bytes)) => {
                respond(200, &kind, bytes.as_ref().clone(), "public, max-age=86400")
            }
            Err(e) => respond(502, "text/plain", e.into_bytes(), "no-store"),
        },
        "/hls" => match load_playlist(&target, origin).await {
            Ok(text) => respond(
                200,
                "application/vnd.apple.mpegurl",
                text.as_bytes().to_vec(),
                "no-store",
            ),
            Err(e) => respond(502, "text/plain", e.into_bytes(), "no-store"),
        },
        "/seg" if range.is_some() => range_response(&target, range.unwrap(), query.split('&').any(|part| part == "direct=1")).await,
        "/seg" => match load_segment(&target).await {
            Ok((ctype, bytes)) => {
                segment_response(&target, ctype.as_deref(), bytes.as_ref().clone(), origin)
            }
            Err(e) => respond(502, "text/plain", e.into_bytes(), "no-store"),
        },
        _ => respond(404, "text/plain", b"not found".to_vec(), "no-store"),
    };
    crate::playback_log::record(
        "proxy.end",
        serde_json::json!({"route": route, "resource": crate::playback_log::resource_id(&target), "status": response.status().as_u16(), "bytes": response.body().len(), "elapsed_ms": started.elapsed().as_millis() as u64}),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    struct CacheServer {
        origin: String,
        requests: Arc<Mutex<Vec<(String, Option<ByteRange>)>>>,
        held: Arc<(Mutex<bool>, std::sync::Condvar)>,
        changed: Arc<std::sync::atomic::AtomicBool>,
        peak: Arc<AtomicU64>,
        paused: Arc<AtomicU64>,
        stopped: Arc<std::sync::atomic::AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }
    impl CacheServer {
        fn start() -> Self {
            use std::io::{Read, Write};
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let origin = format!("http://{}", listener.local_addr().unwrap());
            listener.set_nonblocking(true).unwrap();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let held = Arc::new((Mutex::new(true), std::sync::Condvar::new()));
            let changed = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let peak = Arc::new(AtomicU64::new(0));
            let paused = Arc::new(AtomicU64::new(0));
            let active = Arc::new(AtomicU64::new(0));
            let mut server = Self { origin, requests: requests.clone(), held: held.clone(), changed: changed.clone(), peak: peak.clone(), paused: paused.clone(), stopped: stopped.clone(), thread: None };
            server.thread = Some(std::thread::spawn(move || {
                let mut workers = Vec::new();
                while !stopped.load(Ordering::SeqCst) {
                    let (mut stream, _) = match listener.accept() {
                        Ok(connection) => connection,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => { std::thread::sleep(std::time::Duration::from_millis(1)); continue; }
                        Err(error) => panic!("{error}"),
                    };
                    let requests = requests.clone(); let held = held.clone(); let changed = changed.clone(); let active = active.clone(); let peak = peak.clone(); let paused = paused.clone();
                    workers.push(std::thread::spawn(move || {
                        stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
                        let mut request = Vec::new();
                        while !request.ends_with(b"\r\n\r\n") {
                            let mut byte = [0];
                            if stream.read(&mut byte).unwrap_or(0) == 0 { return; }
                            request.push(byte[0]);
                        }
                        let request = String::from_utf8(request).unwrap();
                        let path = request.lines().next().unwrap().split_whitespace().nth(1).unwrap().to_string();
                        let range = request.lines().find_map(|line| line.split_once(':').filter(|(name, _)| name.eq_ignore_ascii_case("range"))
                            .and_then(|(_, value)| parse_bounded_range(value.trim())));
                        requests.lock().unwrap().push((path.clone(), range));
                        let concurrent = active.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(concurrent, Ordering::SeqCst);
                        if path.ends_with(".ts") {
                            let mut paused = held.0.lock().unwrap();
                            while *paused { paused = held.1.wait(paused).unwrap(); }
                            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: video/mp2t\r\nContent-Length: 6\r\nConnection: close\r\n\r\nshared");
                        } else {
                            let range = range.unwrap();
                            let etag = if changed.load(Ordering::SeqCst) { "v2" } else { "v1" };
                            let body: Vec<_> = (range.start..=range.end).map(|offset| (offset % 251) as u8).collect();
                            std::thread::sleep(std::time::Duration::from_millis(3));
                            let header = format!("HTTP/1.1 206 Partial Content\r\nContent-Type: video/mp4\r\nContent-Range: bytes {}-{}/{}\r\nETag: \"{etag}\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", range.start, range.end, 2 * DIRECT_BLOCK, body.len());
                            let payload = if path.contains("truncated") { &body[..body.len() - 1] } else { &body[..] };
                            if path.starts_with("/slow") && range.start >= DIRECT_TRANSFER {
                                let half = payload.len() / 2;
                                let _ = stream.write_all(header.as_bytes()).and_then(|_| stream.write_all(&payload[..half]));
                                paused.fetch_add(1, Ordering::SeqCst);
                                let mut hold = held.0.lock().unwrap();
                                while *hold { hold = held.1.wait(hold).unwrap(); }
                                let _ = stream.write_all(&payload[half..]);
                            } else { let _ = stream.write_all(header.as_bytes()).and_then(|_| stream.write_all(payload)); }
                        }
                        active.fetch_sub(1, Ordering::SeqCst);
                    }));
                }
                for worker in workers { worker.join().unwrap(); }
            }));
            server
        }
        fn release(&self) { *self.held.0.lock().unwrap() = false; self.held.1.notify_all(); }
    }

    #[tokio::test]
    #[ignore = "local HTTP integration test; run with loopback access"]
    async fn inflight_cache_changes_local_server() {
        let _test_guard = crate::media_cache::TEST_LOCK.lock().unwrap();
        let server = CacheServer::start();
        let dir = std::env::temp_dir().join(format!("hghub-inflight-{}-{}", std::process::id(), DIRECT_REQUEST_ID.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&dir).unwrap();
        crate::network_policy::bind_security(&dir);
        crate::network_policy::set_strict_security(false).unwrap();
        crate::media_cache::bind(&dir);
        for action in ["clear", "disable", "shrink"] {
            crate::media_cache::set_limit(256).unwrap();
            crate::media_cache::clear().unwrap();
            *server.held.0.lock().unwrap() = true;
            let paused = server.paused.load(Ordering::SeqCst);
            let url = format!("{}/slow-{action}.mp4", server.origin);
            let (_, tag) = probe_direct(&url, true).await.unwrap();
            let block = ByteRange { start: 0, end: DIRECT_BLOCK - 1 };
            let key = direct_cache_key(&url, block, &tag);
            let session_id = format!("inflight-{action}");
            crate::media_cache::start_session(session_id.clone()).unwrap();
            let background_url = url.clone(); let background_tag = tag.clone(); let background_id = session_id.clone();
            let background = tokio::spawn(async move { crate::api::playback_cache_prefetch(background_url, Some(block), Some(background_tag), vec![], background_id, None).await });
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                while server.paused.load(Ordering::SeqCst) == paused { tokio::time::sleep(std::time::Duration::from_millis(2)).await; }
            }).await.unwrap();
            assert!(crate::media_cache::read_range(&key, 0, DIRECT_TRANSFER - 1).is_some());
            let foreground_url = url.clone();
            let foreground = tokio::spawn(async move { range_response(&foreground_url, "bytes=65536-131071", true).await });
            tokio::task::yield_now().await;
            match action {
                "clear" => crate::api::cache_clear().await.unwrap(),
                "disable" => crate::media_cache::set_limit(0).unwrap(),
                _ => crate::media_cache::set_limit(128).unwrap(),
            }
            assert!(!tokio::time::timeout(std::time::Duration::from_secs(1), background).await.unwrap().unwrap().unwrap());
            assert!(crate::media_cache::session(&session_id).is_none());
            assert_eq!(crate::media_cache::PREFETCH_SLOT.available_permits(), 2);
            server.release();
            let response = tokio::time::timeout(std::time::Duration::from_secs(3), foreground).await.unwrap().unwrap();
            assert_eq!(response.status(), 206, "foreground playback must survive a cache setting change");
            assert_eq!(response.body(), &(DIRECT_TRANSFER..2 * DIRECT_TRANSFER).map(|offset| (offset % 251) as u8).collect::<Vec<_>>());
            assert!(crate::media_cache::read_range(&key, DIRECT_TRANSFER, 2 * DIRECT_TRANSFER - 1).is_none(), "the old transfer wrote into the new cache generation");
            if action != "shrink" { assert_eq!(crate::media_cache::status().1, 0); }
            else { assert!(crate::media_cache::status().1 <= 128 * 1024 * 1024); }
            crate::media_cache::set_limit(128).unwrap();
            assert!(prefetch_segment(&url, Some(block), Some(&tag), &[], None).await.unwrap(), "a fresh transfer could not resume caching");
            assert!(crate::media_cache::contains(&key));
        }
        crate::media_cache::clear().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }
    impl Drop for CacheServer {
        fn drop(&mut self) {
            self.release(); self.stopped.store(true, Ordering::SeqCst);
            self.thread.take().unwrap().join().unwrap();
        }
    }

    #[tokio::test]
    #[ignore = "local HTTP integration test; run with loopback access"]
    async fn priority_and_partial_cache_local_server() {
        let _test_guard = crate::media_cache::TEST_LOCK.lock().unwrap();
        let server = CacheServer::start();
        let dir = std::env::temp_dir().join(format!("hghub-priority-{}-{}", std::process::id(), DIRECT_REQUEST_ID.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&dir).unwrap();
        crate::network_policy::bind_security(&dir);
        crate::network_policy::set_strict_security(false).unwrap();
        crate::media_cache::bind(&dir);
        crate::media_cache::set_limit(128).unwrap();
        let url = format!("{}/file.mp4", server.origin);
        let (_, tag) = probe_direct(&url, true).await.unwrap();
        let intervals = [(600_000, 700_000), (DIRECT_BLOCK + 12_345, DIRECT_BLOCK + 23_456)];
        for &(start, end) in &intervals {
            let response = range_response(&url, &format!("bytes={start}-{end}"), true).await;
            assert_eq!(response.status(), 206);
            assert_eq!(response.body(), &(start..=end).map(|offset| (offset % 251) as u8).collect::<Vec<_>>());
        }
        assert_eq!(direct_pending(&url).len(), 2);
        let before = server.requests.lock().unwrap().len();
        let first = ByteRange { start: 0, end: DIRECT_BLOCK - 1 };
        let second = ByteRange { start: DIRECT_BLOCK, end: 2 * DIRECT_BLOCK - 1 };
        let protected = vec![direct_cache_key(&url, first, &tag), direct_cache_key(&url, second, &tag)];
        assert!(prefetch_segment(&url, Some(first), Some(&tag), &protected, Some(700_001)).await.unwrap());
        assert_eq!(direct_pending(&url).len(), 2, "Forward prefetch leaves the earlier prefix for completion");
        assert!(server.requests.lock().unwrap()[before..].iter().all(|(_, range)| range.unwrap().start >= 700_001));
        let (left, right) = tokio::join!(prefetch_segment(&url, Some(first), Some(&tag), &protected, None), prefetch_segment(&url, Some(second), Some(&tag), &protected, None));
        assert!(left.unwrap() && right.unwrap());
        assert!(server.peak.load(Ordering::SeqCst) >= 2, "Both cache directions can transfer concurrently");
        assert!(direct_pending(&url).is_empty());
        let mut ranges: Vec<_> = server.requests.lock().unwrap().iter().skip(1).filter_map(|(_, range)| *range).collect();
        ranges.sort_by_key(|range| range.start);
        assert!(ranges.windows(2).all(|pair| pair[0].end < pair[1].start), "Concurrent completion must not request overlapping bytes");
        for (_, range) in &server.requests.lock().unwrap()[before..] {
            let range = range.unwrap();
            assert!(intervals.iter().all(|&(start, end)| range.end < start || range.start > end), "Previously received bytes must be reused");
        }
        let before = server.requests.lock().unwrap().len();
        let response = range_response(&url, "bytes=610000-620000", true).await;
        assert_eq!(response.body().len(), 10_001);
        assert_eq!(server.requests.lock().unwrap().len(), before);

        let info = DIRECT_MEDIA.lock().unwrap()[&url].clone();
        let generation = crate::media_cache::generation().unwrap();
        let overlap = format!("{}/overlap.mp4", server.origin);
        let (left, right) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(
                direct_bytes(&overlap, &info, ByteRange { start: 600_000, end: 600_015 }, true, generation, &[]),
                direct_bytes(&overlap, &info, ByteRange { start: 600_016, end: 600_031 }, true, generation, &[]),
            )
        }).await.unwrap();
        assert_eq!(left.unwrap().1, (600_000..=600_015).map(|offset| (offset % 251) as u8).collect::<Vec<_>>());
        assert_eq!(right.unwrap().1, (600_016..=600_031).map(|offset| (offset % 251) as u8).collect::<Vec<_>>());
        assert_eq!(server.requests.lock().unwrap().iter().filter(|(path, _)| path == "/overlap.mp4").count(), 2);
        let truncated = format!("{}/truncated.mp4", server.origin);
        assert!(direct_bytes(&truncated, &info, ByteRange { start: 0, end: DIRECT_TRANSFER - 1 }, true, generation, &[]).await.is_err());
        assert!(crate::media_cache::read_range(&direct_cache_key(&truncated, first, &tag), 0, DIRECT_TRANSFER - 1).is_none());

        let segment = format!("{}/held.ts", server.origin);
        crate::media_cache::start_session("shared-network-test".into()).unwrap();
        let background_url = segment.clone();
        let background = tokio::spawn(async move { crate::api::playback_cache_prefetch(background_url, None, None, vec![], "shared-network-test".into(), None).await });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !server.requests.lock().unwrap().iter().any(|(path, _)| path == "/held.ts") { tokio::time::sleep(std::time::Duration::from_millis(5)).await; }
        }).await.unwrap();
        let foreground_url = segment.clone();
        let foreground = tokio::spawn(async move { load_segment(&foreground_url).await });
        tokio::task::yield_now().await;
        background.abort(); let _ = background.await;
        crate::media_cache::cancel_session("shared-network-test");
        server.release();
        let (_, bytes) = tokio::time::timeout(std::time::Duration::from_secs(5), foreground).await.unwrap().unwrap().unwrap();
        assert_eq!(bytes.as_slice(), b"shared");
        assert_eq!(server.requests.lock().unwrap().iter().filter(|(path, _)| path == "/held.ts").count(), 1);

        server.changed.store(true, Ordering::SeqCst);
        let (_, new_tag) = probe_direct(&url, true).await.unwrap();
        assert_ne!(new_tag, tag);
        assert!(prefetch_segment(&url, Some(first), Some(&tag), &protected, None).await.is_err());
        let before = server.requests.lock().unwrap().len();
        assert_eq!(range_response(&url, "bytes=610000-620000", true).await.status(), 206);
        assert!(server.requests.lock().unwrap().len() > before, "A changed ETag cannot reuse the old blocks");
        crate::media_cache::clear().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    #[ignore = "local HTTP integration test; run individually with loopback access"]
    async fn range_cache_local_server() {
        use std::io::{Read, Write};
        let _test_guard = crate::media_cache::TEST_LOCK.lock().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for (sequence, incoming) in listener.incoming().take(10).enumerate() {
                let mut stream = incoming.unwrap();
                stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
                let mut request = Vec::new(); let mut chunk = [0; 2048];
                while !request.ends_with(b"\r\n\r\n") {
                    let size = stream.read(&mut chunk).unwrap();
                    if size == 0 { break; }
                    request.extend_from_slice(&chunk[..size]);
                }
                let request = String::from_utf8(request).unwrap();
                if request.lines().next().unwrap().contains(".aac") || request.lines().next().unwrap().contains(".vtt") {
                    let text = request.lines().next().unwrap().contains(".vtt");
                    let payload: &[u8] = if text { b"WEBVTT\n\n00:00.000 --> 00:04.000\nCached subtitle\n" } else { b"audio fixture" };
                    // Exercise type inference for generic upstream content types.
                    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", payload.len()).unwrap();
                    stream.write_all(payload).unwrap();
                    continue;
                }
                if request.lines().next().unwrap().contains(".m3u8") {
                    let payload = format!("#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4,\nseg{sequence}.ts\n#EXT-X-ENDLIST\n");
                    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/vnd.apple.mpegurl\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", payload.len(), payload).unwrap();
                    continue;
                }
                let header = request.lines().find_map(|line| line.split_once(':').filter(|(name, _)| name.eq_ignore_ascii_case("range")).map(|(_, value)| value.trim())).unwrap();
                let range = parse_bounded_range(header).unwrap();
                let bytes = b"01234567";
                let payload = &bytes[range.start as usize..=range.end as usize];
                write!(stream, "HTTP/1.1 206 Partial Content\r\nContent-Type: video/mp4\r\nETag: \"v1\"\r\nContent-Range: bytes {}-{}/8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", range.start, range.end, payload.len()).unwrap();
                stream.write_all(payload).unwrap();
            }
        });
        let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("hghub-range-net-{}-{unique}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        crate::network_policy::bind_security(&dir);
        crate::network_policy::set_strict_security(false).unwrap();
        crate::media_cache::bind(&dir);
        crate::media_cache::set_limit(128).unwrap();
        let url = format!("http://{address}/video.mp4");
        let (total, tag) = probe_direct(&url, true).await.unwrap();
        assert_eq!(total, 8);
        let response = range_response(&url, "bytes=2-4", true).await;
        assert_eq!(response.status(), 206);
        assert_eq!(response.body(), b"234");
        assert_eq!(direct_cursor(&url), 5);
        assert!(prefetch_segment(&url, Some(ByteRange { start: 0, end: 7 }), Some(&tag), &[], None).await.unwrap());
        let changing = format!("http://{address}/changing.m3u8");
        let (_, first) = load_segment(&changing).await.unwrap();
        let (_, second) = load_segment(&changing).await.unwrap();
        assert_ne!(first, second, "media playlists must not be frozen in the small-object cache");
        let root = format!("http://{address}/root.m3u8");
        let first = load_playlist(&root, LOCAL_ORIGIN).await.unwrap();
        let key = format!("{LOCAL_ORIGIN}|{root}");
        PL_CACHE.lock().unwrap().get_mut(&key).unwrap().0 = std::time::Instant::now();
        let second = load_playlist(&root, LOCAL_ORIGIN).await.unwrap();
        assert_ne!(first, second, "an expired root playlist must be refetched");
        let audio = format!("http://{address}/selected.aac");
        let subtitles = format!("http://{address}/selected.vtt");
        assert!(prefetch_segment(&audio, None, None, &[], None).await.unwrap());
        assert!(prefetch_segment(&subtitles, None, None, &[], None).await.unwrap());
        server.join().unwrap();
        let (mime, bytes) = load_segment(&audio).await.unwrap();
        assert_eq!(mime.as_deref(), Some("audio/aac")); assert_eq!(bytes.as_slice(), b"audio fixture");
        let (mime, bytes) = load_segment(&subtitles).await.unwrap();
        assert_eq!(mime.as_deref(), Some("text/vtt")); assert!(bytes.starts_with(b"WEBVTT"));
        let response = range_response(&url, "bytes=2-4", true).await;
        assert_eq!(response.status(), 206);
        assert_eq!(response.headers()["Content-Range"], "bytes 2-4/8");
        assert_eq!(response.body(), b"234");
        crate::media_cache::clear().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn direct_ranges_read_exact_bytes_from_cached_blocks() {
        let _test_guard = crate::media_cache::TEST_LOCK.lock().unwrap();
        let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("hghub-range-{}-{unique}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        crate::media_cache::bind(&dir);
        crate::media_cache::set_limit(128).unwrap();
        let url = "https://example.test/movie.mp4";
        let info = DirectInfo { total: 8, cursor: 0, request_id: 0, etag: "\"v1\"".into(), tag: "test-v1".into() };
        DIRECT_MEDIA.lock().unwrap().insert(url.into(), info.clone());
        let key = direct_cache_key(url, ByteRange { start: 0, end: 7 }, &info.tag);
        assert!(crate::media_cache::put_entry(&key, Some("video/mp4".into()), Some(8), b"01234567", crate::media_cache::generation().unwrap(), &[]));
        let response = range_response(url, "bytes=2-4", true).await;
        assert_eq!(response.status(), 206);
        assert_eq!(response.headers()["Content-Range"], "bytes 2-4/8");
        assert_eq!(response.body(), b"234");
        assert_eq!(direct_cursor(url), 5);
        let previous_request = DIRECT_MEDIA.lock().unwrap()[url].request_id;
        let response = range_response(url, "bytes=5-", true).await;
        assert_eq!(response.status(), 206);
        assert_eq!(response.headers()["Content-Range"], "bytes 5-7/8");
        assert_eq!(response.body(), b"567");
        assert_eq!(direct_cursor(url), 8);
        advance_direct_cursor(url, previous_request, 5);
        assert_eq!(direct_cursor(url), 8);
        let total = DIRECT_BLOCK + 5;
        DIRECT_MEDIA.lock().unwrap().insert(url.into(), DirectInfo { total, ..info.clone() });
        let generation = crate::media_cache::generation().unwrap();
        let first = direct_cache_key(url, ByteRange { start: 0, end: DIRECT_BLOCK - 1 }, &info.tag);
        let second = direct_cache_key(url, ByteRange { start: DIRECT_BLOCK, end: total - 1 }, &info.tag);
        assert!(crate::media_cache::put_entry(&first, Some("video/mp4".into()), Some(total), &vec![b'A'; DIRECT_BLOCK as usize], generation, &[]));
        assert!(crate::media_cache::put_entry(&second, Some("video/mp4".into()), Some(total), b"BCDEF", generation, &[]));
        let response = range_response(url, "bytes=1048574-1048578", true).await;
        assert_eq!(response.status(), 206);
        assert_eq!(response.body(), b"AABCD");
        assert_eq!(response.headers()["Content-Range"], "bytes 1048574-1048578/1048581");
        assert_eq!(range_response(url, "bytes=4-1", true).await.status(), 400);
        DIRECT_MEDIA.lock().unwrap().remove(url);
        crate::media_cache::clear().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn byte_ranges_have_distinct_keys_and_strict_content_ranges() {
        let a = ByteRange { start: 0, end: 99 };
        let b = ByteRange { start: 100, end: 199 };
        assert_ne!(range_key("https://example.test/video", a), range_key("https://example.test/video", b));
        assert_eq!(parse_content_range("bytes 0-99/200"), Some((0, 99, 200)));
        assert!(parse_content_range("bytes 0-200/200").is_none());
        assert!(parse_content_range("bytes 0-99/*").is_none());
        assert!(parse_bounded_range("bytes=0-99").is_some());
        assert!(parse_bounded_range("bytes=0-").is_none());
        assert!(parse_bounded_range("bytes=99-0").is_none());
        for value in ["bytes=0-", "bytes=-500", "bytes=0-9,20-29"] { assert!(valid_range_header(value)); }
        for value in ["bytes=9-0", "bytes=-0", "bytes=--", "bytes=0-1,"] { assert!(!valid_range_header(value)); }
    }

    #[test]
    fn dateranges_keep_timing_and_scte_metadata_but_drop_external_resources() {
        let line = "#EXTM3U\n#EXT-X-DATERANGE:ID=\"event\",START-DATE=\"2026-11-07T00:00:00Z\",DURATION=4,SCTE35-OUT=0xFC,X-TEXT=\"a,b\",X-ASSET-URI=\"https://evil.example/ad\",X-ASSET-LIST=\"https://evil.example/list.json\"\n";
        let out = rewrite_playlist("https://cdn.example/master.m3u8", line, LOCAL_ORIGIN);
        assert!(out.contains("DURATION=4,SCTE35-OUT=0xFC,X-TEXT=\"a,b\""));
        assert!(out.contains("START-DATE="));
        assert!(!out.contains("evil.example") && !out.contains("X-ASSET"));
    }

    #[test]
    fn every_uri_attribute_is_rewritten_without_splitting_quoted_commas() {
        let line = "#EXT-X-MEDIA:NAME=\"audio,main\",URI=\"first.m3u8\",URI=\"second.m3u8\"";
        let absolute = absolutize_uri_in_attr("https://cdn.example/master.m3u8", line);
        let rewritten = rewrite_uri_attr(&absolute, LOCAL_ORIGIN);
        assert_eq!(
            rewritten.matches(&format!("{LOCAL_ORIGIN}/seg?u=")).count(),
            2
        );
        assert!(rewritten.contains("NAME=\"audio,main\""));
        assert!(!rewritten.contains("URI=\"https://"));
    }

    #[test]
    fn nested_playlists_and_alternate_uris_stay_behind_proxy() {
        let text = "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,URI=\"audio/list.m3u8\"\n#EXT-X-SESSION-KEY:METHOD=AES-128,URI=\"http://127.0.0.1/key\"\n#EXT-X-PART:DURATION=1,URI=\"part.ts\"\nvariant/list.m3u8\n";
        let response = segment_response(
            "https://cdn.example/master.m3u8",
            Some("application/octet-stream"),
            text.as_bytes().to_vec(),
            LOCAL_ORIGIN,
        );
        let body = std::str::from_utf8(response.body()).unwrap();
        assert_eq!(body.matches(&format!("{LOCAL_ORIGIN}/seg?u=")).count(), 4);
        assert!(!body.contains("URI=\"http://127.0.0.1"));
        assert!(body.contains("https%3A%2F%2Fcdn%2Eexample%2Faudio%2Flist%2Em3u8"));
        assert_eq!(
            response.headers()["Content-Type"],
            "application/vnd.apple.mpegurl"
        );
        assert_eq!(response.headers()["X-Content-Type-Options"], "nosniff");
        assert!(!response
            .headers()
            .contains_key("Access-Control-Allow-Origin"));
        let optional = rewrite_playlist("https://cdn.example/list.m3u8",
            "#EXTM3U\n#EXT-X-CONTENT-STEERING:SERVER-URI=\"https://other.example/list.json\"\n#EXT-X-DATERANGE:ID=\"ad\",X-ASSET-LIST=\"http://127.0.0.1/assets\"\nsegment.ts\n", LOCAL_ORIGIN);
        assert!(!optional.contains("CONTENT-STEERING") && !optional.contains("X-ASSET-LIST"));
        assert!(optional.contains("#EXT-X-DATERANGE:ID=\"ad\""));
        let bom = segment_response(
            "https://cdn.example/list.m3u8",
            None,
            b"\xef\xbb\xbf#EXTM3U\nsegment.ts\n".to_vec(),
            LOCAL_ORIGIN,
        );
        assert!(bom.body().starts_with(b"#EXTM3U\n"));
    }

    /// 联网测试：默认跳过，手动跑 `cargo test --lib -- --ignored`。
    /// 依赖上游 `/api/videos/117/play` 返回的实时播放地址。
    #[tokio::test]
    #[ignore = "联网测试：依赖上游实时播放地址，手动用 --ignored 运行"]
    async fn live_play_chain() {
        use crate::net::edge;
        let play = edge(
            &crate::net::base(),
            "GET",
            "/api/videos/117/play?ep=3&page_texts=0",
            None,
            None,
        )
        .await
        .expect("play api");
        let url = play["video_url"].as_str().expect("video_url").to_string();
        println!("play url: {}", &url[..url.len().min(90)]);
        let (_, bytes) = fetch_bytes(&url).await.expect("fetch playlist");
        let text = String::from_utf8_lossy(&bytes);
        println!(
            "raw playlist len={} head={}",
            text.len(),
            &text[..text.len().min(160)]
        );
        let q = format!("u={}", enc(&url));
        let resp = handle("/hls", &q, LOCAL_ORIGIN, None).await;
        let body = String::from_utf8_lossy(resp.body()).to_string();
        println!("proxied status={} len={}", resp.status(), body.len());
        assert!(body.contains("#EXTM3U"));
        // 取一个分片验证
        let seg_line = body
            .lines()
            .find(|l| !l.starts_with('#') && l.contains("/seg?u="))
            .expect("seg line")
            .to_string();
        println!("seg line: {}", &seg_line[..seg_line.len().min(120)]);
        let seg_q = seg_line.split("?u=").nth(1).unwrap();
        let seg = handle("/seg", &format!("u={seg_q}"), LOCAL_ORIGIN, None).await;
        println!("segment status={} len={}", seg.status(), seg.body().len());
        assert_eq!(seg.status(), 200);
        assert!(seg.body().len() > 1000);
    }

    /// 离线回归：不再依赖带 auth_key 时效的真实上游地址（旧地址已过期，会 403）。
    /// 与真实播放列表同构：带签名的分片地址 + 加密 KEY + 相对路径分片。
    #[test]
    fn playlist_rewrite_works() {
        let base = "https://cdn.example/videos5/abc/abc.m3u8?auth_key=9999999999-0-0-deadbeef&v=3&via=hgdj";
        let text = "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin?auth_key=9999999999-0-0-deadbeef\"\n#EXTINF:4,\nseg-1.ts?auth_key=9999999999-0-0-deadbeef\n";
        let body = rewrite_playlist(base, text, LOCAL_ORIGIN);

        assert!(body.contains("#EXTM3U"), "not a playlist: {body}");
        assert!(
            body.contains(&format!("{LOCAL_ORIGIN}/seg?u=")),
            "segments not rewritten: {body}"
        );
        // 分片必须绝对化到源站，并保留签名查询串。
        assert!(
            body.contains("https%3A%2F%2Fcdn%2Eexample%2Fvideos5%2Fabc%2Fseg%2D1%2Ets%3Fauth%5Fkey%3D"),
            "segment not absolutized with query: {body}"
        );
        // 加密 KEY 也要走本地代理，否则 hls.js 会直接打源站被 CORS 拦住。
        assert!(
            body.contains("URI=\"") && body.contains("%2Fkey%2Ebin%3Fauth%5Fkey%3D"),
            "key uri not rewritten: {body}"
        );
        assert!(!body.contains("URI=\"key.bin"), "key uri left relative");
    }

    /// 离线回归：用已知明文 + 项目内置 key/IV 构造密文，验证解密、去填充与类型识别。
    /// 真实封面抓取那条仍保留为 `#[ignore]` 联网测试。
    #[test]
    fn cover_decrypt_roundtrip() {
        use aes::cipher::BlockEncrypt;

        let plain = b"\xFF\xD8\xFF\xE0cover-payload-for-test".to_vec();
        let mut buf = plain.clone();
        let pad = 16 - plain.len() % 16;
        buf.extend(std::iter::repeat_n(pad as u8, pad));

        let cipher = Aes128::new_from_slice(IMG_KEY).expect("key");
        let mut prev = [0u8; 16];
        prev.copy_from_slice(IMG_IV);
        let mut encrypted = Vec::with_capacity(buf.len());
        for chunk in buf.chunks(16) {
            let mut block = [0u8; 16];
            block.copy_from_slice(chunk);
            for i in 0..16 {
                block[i] ^= prev[i];
            }
            let mut out_block = block;
            cipher.encrypt_block((&mut out_block).into());
            encrypted.extend_from_slice(&out_block);
            prev = out_block;
        }

        let decrypted = aes_cbc_decrypt(&encrypted).expect("decrypt");
        let stripped = strip_padding(decrypted);
        assert_eq!(stripped, plain);
        assert_eq!(image_kind(&stripped), Some("image/jpeg"));
    }

    /// 联网测试：默认跳过。封面地址带时效签名，不能硬编码进仓库，
    /// 需要用当次上游返回的地址显式传入：
    /// `HGHUB_TEST_COVER_URL='https://…?auth_key=…' cargo test --lib -- --ignored`
    #[tokio::test]
    #[ignore = "联网测试：需要 HGHUB_TEST_COVER_URL，手动用 --ignored 运行"]
    async fn cover_decrypt_works() {
        let url = std::env::var("HGHUB_TEST_COVER_URL")
            .expect("需要设置 HGHUB_TEST_COVER_URL 为当前有效的封面地址");
        match load_image(&url).await {
            Ok((kind, bytes)) => {
                println!("kind={kind} len={}", bytes.len());
                assert!(kind.starts_with("image/"));
            }
            Err(e) => panic!("image failed: {e}"),
        }
    }
}
