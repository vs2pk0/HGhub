//! Bounded disk cache. Only complete objects are visible to readers.
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, io::{Read, Seek, SeekFrom, Write}, path::{Path, PathBuf}, sync::Mutex, time::{Instant, SystemTime, UNIX_EPOCH}};

#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    size: u64, accessed: u64, mime: Option<String>, #[serde(default)] total: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")] partial: Option<Partial>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Partial { group: String, offset: u64, ranges: Vec<(u64, u64)> }
#[derive(Serialize, Deserialize)]
#[serde(default)]
struct Index { max_mib: u32, entries: HashMap<String, Entry> }
impl Default for Index {
    fn default() -> Self { Self { max_mib: 256, entries: HashMap::new() } }
}
struct Store { root: PathBuf, index: Index, generation: u64, last_access: u64, dirty: bool, flushed: Instant }
#[cfg(test)]
pub(crate) static TEST_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));
static STORE: Lazy<Mutex<Option<Store>>> = Lazy::new(|| Mutex::new(None));
static SESSIONS: Lazy<Mutex<HashMap<String, tokio::sync::watch::Sender<bool>>>> = Lazy::new(|| Mutex::new(HashMap::new()));
pub static PREFETCH_SLOT: Lazy<tokio::sync::Semaphore> = Lazy::new(|| tokio::sync::Semaphore::new(2));
pub fn start_session(id: String) -> Result<(), String> {
    if id.is_empty() || id.len() > 128 { return Err("无效的预缓存任务".into()); }
    let mut sessions = SESSIONS.lock().map_err(|_| "缓存任务锁失败")?;
    if sessions.contains_key(&id) || sessions.len() >= 64 { return Err("预缓存任务过多或已存在".into()); }
    sessions.insert(id, tokio::sync::watch::channel(false).0);
    Ok(())
}
pub fn cancel_session(id: &str) {
    if let Ok(mut sessions) = SESSIONS.lock() {
        if let Some(sender) = sessions.remove(id) {
            let _ = sender.send(true);
            crate::media_download::buffer_ready(id, true);
        }
    }
}
pub fn buffer_ready(id: &str, ready: bool) {
    if let Ok(sessions) = SESSIONS.lock() {
        if sessions.contains_key(id) { crate::media_download::buffer_ready(id, ready); }
    }
}
fn cancel_all() {
    if let Ok(mut sessions) = SESSIONS.lock() { for (id, sender) in sessions.drain() {
        let _ = sender.send(true);
        crate::media_download::buffer_ready(&id, true);
    } }
}
pub fn session(id: &str) -> Option<tokio::sync::watch::Receiver<bool>> {
    SESSIONS.lock().ok()?.get(id).map(|sender| sender.subscribe())
}
fn now() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64 }
fn key(url: &str) -> String { format!("{:x}", Sha256::digest(url.as_bytes())) }
fn valid_key(id: &str) -> bool { id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()) }
fn save(store: &mut Store) -> Result<(), String> {
    let data = serde_json::to_vec(&store.index).map_err(|e| e.to_string())?;
    let tmp = store.root.join("index.tmp");
    std::fs::write(&tmp, data).and_then(|_| std::fs::rename(tmp, store.root.join("index.json"))).map_err(|e| e.to_string())?;
    store.dirty = false; store.flushed = Instant::now(); Ok(())
}
fn remove(store: &mut Store, id: &str) -> Result<(), String> {
    match std::fs::remove_file(store.root.join(id)) { Ok(()) => {}, Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}, Err(e) => return Err(e.to_string()) }
    store.index.entries.remove(id); store.dirty = true; Ok(())
}
fn trim(store: &mut Store) -> Result<(), String> {
    let limit = u64::from(store.index.max_mib) * 1024 * 1024;
    let mut size: u64 = store.index.entries.values().map(|e| e.size).sum();
    let mut oldest: Vec<_> = store.index.entries.iter().map(|(id, entry)| (id.clone(), entry.clone())).collect();
    oldest.sort_by_key(|(_, entry)| entry.accessed);
    for (id, entry) in oldest { if size <= limit { break; } remove(store, &id)?; size = size.saturating_sub(entry.size); }
    Ok(())
}
pub fn bind(dir: &Path) {
    let root = dir.join("playback-cache");
    if std::fs::create_dir_all(&root).is_err() { return; }
    let mut index: Index = std::fs::read(root.join("index.json")).ok().and_then(|v| serde_json::from_slice(&v).ok()).unwrap_or_default();
    if index.max_mib != 0 && !(128..=4096).contains(&index.max_mib) { index.max_mib = 0; }
    index.entries.retain(|id, e| valid_key(id) && e.size <= 4096 * 1024 * 1024
        && e.partial.as_ref().is_none_or(|partial| e.size > 0 && e.size <= 1024 * 1024 && valid_key(&partial.group)
            && partial.offset % (1024 * 1024) == 0
            && e.total.is_some_and(|total| partial.offset < total && e.size == (total - partial.offset).min(1024 * 1024))
            && partial.ranges.iter().all(|&(start, end)| start <= end && end < e.size)
            && partial.ranges.windows(2).all(|pair| pair[0].1 < pair[1].0))
        && std::fs::metadata(root.join(id)).is_ok_and(|m| m.is_file() && m.len() == e.size));
    let last_access = index.entries.values().map(|e| e.accessed).max().unwrap_or(0);
    let mut store = Store { root, index, generation: 0, last_access, dirty: false, flushed: Instant::now() };
    if let Ok(files) = std::fs::read_dir(&store.root) { for file in files.flatten() {
        let name = file.file_name().to_string_lossy().to_string();
        if name != "index.json" && !store.index.entries.contains_key(&name) { let _ = std::fs::remove_file(file.path()); }
    }}
    let _ = trim(&mut store); let _ = save(&mut store);
    if let Ok(mut guard) = STORE.lock() { *guard = Some(store); }
}
pub fn status() -> (u32, u64) {
    STORE.lock().ok().and_then(|mut s| s.as_mut().map(|s| {
        if s.dirty && s.flushed.elapsed().as_secs() >= 5 { let _ = save(s); }
        (s.index.max_mib, s.index.entries.values().map(|e| e.size).sum())
    })).unwrap_or_default()
}
pub fn revision() -> u64 { STORE.lock().ok().and_then(|s| s.as_ref().map(|s| s.generation)).unwrap_or(0) }
pub fn set_limit(mib: u32) -> Result<(), String> {
    if mib != 0 && !(128..=4096).contains(&mib) { return Err("缓存容量须为 0 或 128–4096 MiB 的整数".into()); }
    let mut guard = STORE.lock().map_err(|_| "缓存锁失败")?;
    let store = guard.as_mut().ok_or("缓存尚未初始化")?;
    let previous = store.index.max_mib; store.index.max_mib = mib;
    if let Err(error) = save(store) { store.index.max_mib = previous; return Err(error); }
    store.generation += 1; cancel_all();
    let result = trim(store); let saved = save(store); result.and(saved)
}
pub fn clear() -> Result<(), String> {
    let mut guard = STORE.lock().map_err(|_| "缓存锁失败")?;
    let store = guard.as_mut().ok_or("缓存尚未初始化")?;
    store.generation += 1; cancel_all();
    let ids: Vec<_> = store.index.entries.keys().cloned().collect();
    for id in ids { if let Err(error) = remove(store, &id) { let _ = save(store); return Err(error); } }
    save(store)
}
pub fn generation() -> Option<u64> { STORE.lock().ok().and_then(|s| s.as_ref().filter(|s| s.index.max_mib != 0).map(|s| s.generation)) }
pub fn contains(url: &str) -> bool {
    let Ok(mut guard) = STORE.lock() else { return false }; let Some(s) = guard.as_mut() else { return false };
    if s.index.max_mib == 0 { return false; }
    let id = key(url); let Some(entry) = s.index.entries.get(&id) else { return false };
    if entry.partial.is_some() { return false; }
    if std::fs::metadata(s.root.join(&id)).is_ok_and(|m| m.is_file() && m.len() == entry.size) { return true; }
    let _ = remove(s, &id); false
}
pub fn get(url: &str) -> Option<(Option<String>, Vec<u8>)> { get_entry(url).map(|(mime, _, bytes)| (mime, bytes)) }
pub fn get_entry(url: &str) -> Option<(Option<String>, Option<u64>, Vec<u8>)> {
    let mut guard = STORE.lock().ok()?; let s = guard.as_mut()?;
    if s.index.max_mib == 0 { return None; }
    let id = key(url); let entry = s.index.entries.get(&id)?.clone();
    if entry.partial.is_some() { return None; }
    let bytes = std::fs::read(s.root.join(&id)).ok();
    let Some(bytes) = bytes.filter(|bytes| bytes.len() as u64 == entry.size) else { let _ = remove(s, &id); return None };
    s.last_access = now().max(s.last_access.saturating_add(1));
    s.index.entries.get_mut(&id)?.accessed = s.last_access; s.dirty = true;
    if s.flushed.elapsed().as_secs() >= 5 { let _ = save(s); }
    Some((entry.mime, entry.total, bytes))
}
fn eviction_plan(store: &Store, id: &str, size: u64, protected: &[String]) -> Option<Vec<String>> {
    let limit = u64::from(store.index.max_mib) * 1024 * 1024;
    if size > limit { return None; }
    let old = store.index.entries.get(id).map_or(0, |entry| entry.size);
    let mut used: u64 = store.index.entries.values().map(|entry| entry.size).sum();
    let protected: std::collections::HashSet<_> = protected.iter().map(|url| key(url)).collect();
    let mut candidates: Vec<_> = store.index.entries.iter().filter(|(candidate, _)| candidate.as_str() != id && !protected.contains(*candidate))
        .map(|(id, entry)| (id.clone(), entry.clone())).collect();
    candidates.sort_by_key(|(_, entry)| entry.accessed);
    let mut victims = Vec::new();
    for (victim, entry) in candidates {
        if used.saturating_sub(old) + size <= limit { break; }
        used = used.saturating_sub(entry.size); victims.push(victim);
    }
    if used.saturating_sub(old) + size > limit { None } else { Some(victims) }
}
pub fn put(url: &str, mime: Option<String>, bytes: &[u8], generation: u64, protected_urls: &[String]) -> bool { put_entry(url, mime, None, bytes, generation, protected_urls) }
pub fn put_entry(url: &str, mime: Option<String>, total: Option<u64>, bytes: &[u8], generation: u64, protected_urls: &[String]) -> bool {
    let Ok(mut guard) = STORE.lock() else { return false }; let Some(s) = guard.as_mut() else { return false };
    if s.generation != generation || s.index.max_mib == 0 { return false; }
    let id = key(url);
    let Some(victims) = eviction_plan(s, &id, bytes.len() as u64, protected_urls) else { return false };
    let temp = s.root.join(format!("{id}.tmp"));
    if std::fs::write(&temp, bytes).is_err() { let _ = std::fs::remove_file(temp); return false; }
    for victim in victims { if remove(s, &victim).is_err() { let _ = std::fs::remove_file(&temp); let _ = save(s); return false; } }
    if std::fs::rename(&temp, s.root.join(&id)).is_err() { let _ = std::fs::remove_file(temp); let _ = save(s); return false; }
    s.last_access = now().max(s.last_access.saturating_add(1));
    s.index.entries.insert(id.clone(), Entry { size: bytes.len() as u64, accessed: s.last_access, mime, total, partial: None });
    if save(s).is_err() { let _ = remove(s, &id); return false; }
    true
}

/// Reserved partial files count at their full block size toward the configured limit.
pub fn write_range(url: &str, group: &str, block_start: u64, size: u64, offset: u64, bytes: &[u8], mime: Option<String>, total: u64, generation: u64, protected: &[String]) -> bool {
    if size == 0 || size > 1024 * 1024 || bytes.is_empty() || offset > size || bytes.len() as u64 > size - offset { return false; }
    if block_start % (1024 * 1024) != 0 || !block_start.checked_add(size).is_some_and(|end| end <= total) { return false; }
    let Ok(mut guard) = STORE.lock() else { return false };
    let Some(store) = guard.as_mut().filter(|store| store.generation == generation && store.index.max_mib != 0) else { return false };
    let id = key(url);
    if store.index.entries.get(&id).is_some_and(|entry| !std::fs::metadata(store.root.join(&id)).is_ok_and(|file| file.len() == entry.size)) {
        let _ = remove(store, &id);
    }
    if let Some(entry) = store.index.entries.get(&id) {
        if entry.size != size || entry.total != Some(total) { return false; }
        if entry.partial.is_none() { return true; }
    } else {
        let Some(victims) = eviction_plan(store, &id, size, protected) else { return false };
        let Ok(file) = std::fs::File::create(store.root.join(&id)) else { return false };
        if file.set_len(size).is_err() { let _ = std::fs::remove_file(store.root.join(&id)); return false; }
        for victim in victims {
            if remove(store, &victim).is_err() { let _ = std::fs::remove_file(store.root.join(&id)); let _ = save(store); return false; }
        }
        store.index.entries.insert(id.clone(), Entry {
            size, accessed: now(), mime: mime.clone(), total: Some(total),
            partial: Some(Partial { group: key(group), offset: block_start, ranges: Vec::new() }),
        });
        if save(store).is_err() { let _ = remove(store, &id); return false; }
    }
    let Ok(mut file) = std::fs::OpenOptions::new().write(true).open(store.root.join(&id)) else { return false };
    if file.seek(SeekFrom::Start(offset)).and_then(|_| file.write_all(bytes)).is_err() { return false; }
    store.last_access = now().max(store.last_access.saturating_add(1));
    let entry = store.index.entries.get_mut(&id).unwrap();
    entry.accessed = store.last_access;
    if mime.is_some() { entry.mime = mime; }
    let partial = entry.partial.as_mut().unwrap();
    partial.ranges.push((offset, offset + bytes.len() as u64 - 1));
    partial.ranges.sort_unstable();
    let mut merged: Vec<(u64, u64)> = Vec::new();
    for &(start, end) in &partial.ranges {
        if let Some(last) = merged.last_mut().filter(|last| start <= last.1 + 1) { last.1 = last.1.max(end); }
        else { merged.push((start, end)); }
    }
    partial.ranges = merged;
    let complete = partial.ranges == [(0, size - 1)];
    if complete { entry.partial = None; }
    store.dirty = true;
    if complete || store.flushed.elapsed().as_secs() >= 5 { return save(store).is_ok(); }
    true
}

/// Return the contiguous received bytes starting at offset, including an incomplete block.
pub fn read_range(url: &str, offset: u64, end: u64) -> Option<(Option<String>, u64, Vec<u8>)> {
    let mut guard = STORE.lock().ok()?;
    let store = guard.as_mut().filter(|store| store.index.max_mib != 0)?;
    let id = key(url);
    let entry = store.index.entries.get(&id)?.clone();
    let mut end = end.min(entry.size.checked_sub(1)?);
    if let Some(partial) = entry.partial {
        end = end.min(partial.ranges.iter().find(|&&(start, end)| start <= offset && end >= offset)?.1);
    }
    if offset > end { return None; }
    let mut bytes = vec![0; (end - offset + 1) as usize];
    let file = std::fs::File::open(store.root.join(&id)).ok().filter(|file| file.metadata().is_ok_and(|file| file.len() == entry.size));
    let Some(mut file) = file else { let _ = remove(store, &id); return None };
    file.seek(SeekFrom::Start(offset)).and_then(|_| file.read_exact(&mut bytes)).ok()?;
    store.last_access = now().max(store.last_access.saturating_add(1));
    store.index.entries.get_mut(&id)?.accessed = store.last_access;
    store.dirty = true;
    if store.flushed.elapsed().as_secs() >= 5 { let _ = save(store); }
    Some((entry.mime, entry.total?, bytes))
}

pub fn gap_end(url: &str, offset: u64, end: u64) -> u64 {
    let Ok(guard) = STORE.lock() else { return end };
    let Some(store) = guard.as_ref() else { return end };
    store.index.entries.get(&key(url)).and_then(|entry| entry.partial.as_ref())
        .and_then(|partial| partial.ranges.iter().find(|&&(start, _)| start > offset))
        .map_or(end, |&(start, _)| end.min(start - 1))
}

pub fn partial_blocks(group: &str) -> Vec<(u64, u64)> {
    let Ok(guard) = STORE.lock() else { return Vec::new() };
    let Some(store) = guard.as_ref().filter(|store| store.index.max_mib != 0) else { return Vec::new() };
    let group = key(group);
    let mut blocks: Vec<_> = store.index.entries.values().filter_map(|entry| entry.partial.as_ref()
        .filter(|partial| partial.group == group).map(|partial| (partial.offset, partial.offset + entry.size - 1))).collect();
    blocks.sort_unstable(); blocks
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn partial_checkpoint_failure_and_restart_never_expose_unrecorded_ranges() {
        let _lock = TEST_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("hghub-partial-failure-{}-{}", std::process::id(), now()));
        bind(&dir);
        let generation = generation().unwrap();
        let blocked = dir.join("playback-cache").join(key("blocked"));
        std::fs::create_dir(&blocked).unwrap();
        assert!(!write_range("blocked", "file", 0, 8, 3, b"34567", None, 8, generation, &[]));
        assert_eq!(status().1, 0);
        std::fs::remove_dir(blocked).unwrap();
        assert!(write_range("block", "file", 0, 8, 3, b"34567", None, 8, generation, &[]));
        // Only allocation has reached index.json. Existing bytes in a sparse file are not coverage.
        bind(&dir);
        assert!(read_range("block", 3, 7).is_none());
        assert!(!contains("block"));
        assert!(write_range("block", "file", 0, 8, 3, b"34567", None, 8, generation, &[]));
        save(STORE.lock().unwrap().as_mut().unwrap()).unwrap();
        let temporary_index = dir.join("playback-cache/index.tmp");
        std::fs::create_dir(&temporary_index).unwrap();
        assert!(!write_range("block", "file", 0, 8, 0, b"012", None, 8, generation, &[]));
        std::fs::remove_dir(temporary_index).unwrap();
        bind(&dir);
        assert!(!contains("block"));
        assert!(read_range("block", 0, 7).is_none());
        assert_eq!(read_range("block", 3, 7).unwrap().2, b"34567");
        assert!(write_range("block", "file", 0, 8, 0, b"012", None, 8, generation, &[]));
        bind(&dir);
        assert_eq!(get("block").unwrap().1, b"01234567");
        clear().unwrap(); *STORE.lock().unwrap() = None;
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn allocating_a_partial_block_evicts_old_data_and_preserves_protected_data() {
        let _lock = TEST_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("hghub-partial-quota-{}-{}", std::process::id(), now()));
        bind(&dir); set_limit(128).unwrap();
        {
            let mut guard = STORE.lock().unwrap(); let store = guard.as_mut().unwrap();
            for (url, accessed) in [("old", 1), ("upcoming", 2)] {
                let id = key(url);
                std::fs::File::create(store.root.join(&id)).unwrap().set_len(64 * 1024 * 1024).unwrap();
                store.index.entries.insert(id, Entry { size: 64 * 1024 * 1024, accessed, mime: None, total: None, partial: None });
            }
        }
        assert!(write_range("partial", "file", 0, 1024 * 1024, 500_000, b"tail", None, 1024 * 1024, generation().unwrap(), &["upcoming".into()]));
        assert!(!contains("old") && contains("upcoming"));
        assert!(!contains("partial"));
        assert_eq!(status().1, 65 * 1024 * 1024);
        assert_eq!(read_range("partial", 500_000, 500_003).unwrap().2, b"tail");
        set_limit(0).unwrap(); assert_eq!(status().1, 0);
        *STORE.lock().unwrap() = None; std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn partial_blocks_merge_reopen_and_count_against_the_limit() {
        let _lock = TEST_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("hghub-partial-{}-{}", std::process::id(), now()));
        bind(&dir);
        let generation = generation().unwrap();
        assert!(write_range("block", "file", 0, 8, 3, b"34567", None, 8, generation, &[]));
        assert_eq!(status(), (256, 8));
        assert!(!contains("block"));
        assert!(get_entry("block").is_none());
        assert_eq!(read_range("block", 3, 7).unwrap().2, b"34567");
        assert!(read_range("block", 0, 7).is_none());
        assert_eq!(gap_end("block", 0, 7), 2);
        assert_eq!(partial_blocks("file"), [(0, 7)]);
        assert!(partial_blocks("other validator").is_empty());
        save(STORE.lock().unwrap().as_mut().unwrap()).unwrap();
        bind(&dir);
        assert_eq!(read_range("block", 3, 7).unwrap().2, b"34567");
        assert!(write_range("block", "file", 0, 8, 0, b"012", None, 8, generation, &[]));
        assert!(contains("block"));
        assert!(partial_blocks("file").is_empty());
        assert_eq!(get("block").unwrap().1, b"01234567");
        clear().unwrap();
        assert!(!write_range("late", "file", 0, 8, 0, b"012", None, 8, generation, &[]));
        *STORE.lock().unwrap() = None;
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn default_limit_persistence_and_clear_invalidate_inflight() {
        let _test_guard = TEST_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("hghub-disk-cache-{}-{}", std::process::id(), now()));
        std::fs::create_dir_all(&dir).unwrap();
        bind(&dir);
        assert_eq!(status(), (256, 0));
        assert!(generation().is_some());
        assert!(set_limit(127).is_err());
        set_limit(128).unwrap();
        let gen = generation().unwrap();
        assert!(put("https://example.test/segment?secret=1", Some("video/mp2t".into()), b"segment", gen, &[]));
        assert_eq!(get("https://example.test/segment?secret=1").unwrap().1, b"segment");
        let index = std::fs::read_to_string(dir.join("playback-cache/index.json")).unwrap();
        assert!(!index.contains("secret"));
        {
            let mut guard = STORE.lock().unwrap();
            let store = guard.as_mut().unwrap();
            let first = key("https://example.test/old");
            let second = key("https://example.test/upcoming");
            for (id, accessed) in [(first, 1), (second, 2)] {
                let file = std::fs::File::create(store.root.join(&id)).unwrap();
                file.set_len(64 * 1024 * 1024).unwrap();
                store.index.entries.insert(id, Entry { size: 64 * 1024 * 1024, accessed, mime: None, total: None, partial: None });
            }
        }
        assert!(put("https://example.test/new", None, b"12345678", gen, &["https://example.test/upcoming".into()]));
        assert!(!contains("https://example.test/old"));
        assert!(contains("https://example.test/upcoming"));
        assert!(contains("https://example.test/new"));
        // Missing files cannot keep the prefetch queue or progress indicator stuck on a false hit.
        let missing = dir.join("playback-cache").join(key("https://example.test/new"));
        std::fs::remove_file(missing).unwrap();
        assert!(!contains("https://example.test/new"));
        // A failed settings write must preserve the confirmed effective limit.
        std::fs::create_dir(dir.join("playback-cache/index.tmp")).unwrap();
        assert!(set_limit(256).is_err());
        assert_eq!(status().0, 128);
        std::fs::remove_dir(dir.join("playback-cache/index.tmp")).unwrap();
        start_session("cache-clear-test".into()).unwrap();
        let cancelled = session("cache-clear-test").unwrap();
        clear().unwrap();
        assert!(*cancelled.borrow());
        assert!(session("cache-clear-test").is_none());
        let gen = generation().unwrap();
        assert!(put("https://example.test/segment?secret=1", None, b"segment", gen, &[]));
        bind(&dir);
        assert_eq!(status(), (128, 7));
        assert!(contains("https://example.test/segment?secret=1"));
        let gen = generation().unwrap();
        clear().unwrap();
        assert!(!put("https://example.test/late", None, b"late", gen, &[]));
        set_limit(0).unwrap();
        bind(&dir);
        assert_eq!(status(), (0, 0));
        assert!(generation().is_none());
        *STORE.lock().unwrap() = None;
        std::fs::remove_dir_all(dir).unwrap();
    }
}
