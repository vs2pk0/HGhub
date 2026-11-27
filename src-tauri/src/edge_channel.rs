//! 站点业务通道。
//!
//! `/api/edge/request` 已返回「接口已停用」。公开读接口走 `/api/public/request` 的明文 JSON；
//! 播放、详情、搜索和账号走 ECDH P-256 + HKDF-SHA256 + AES-256-GCM（`dx150-edge-v1`）。

use crate::net::{post_json, Posted};
use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use base64::engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD as B64URL};
use base64::Engine;
use hkdf::Hkdf;
use once_cell::sync::Lazy;
use p256::ecdh::EphemeralSecret;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::{EncodedPoint, PublicKey};
use rand::rngs::OsRng;
use serde_json::{json, Value};
use sha2::Sha256;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

const PROTOCOL: &str = "dx150-edge-v1";
const MAX_SEQUENCE: u32 = 16384;

pub struct Exchanged {
    pub value: Value,
    pub status: u16,
    pub headers_ms: u64,
    pub target_host: String,
    pub response_kind: &'static str,
    pub gateway: &'static str,
}

struct Session {
    info: String,
    kid: String,
    sid: String,
    clock_offset: i64,
    expires_at: i64,
    sequence: u32,
    request_key: [u8; 32],
    response_key: [u8; 32],
}

struct Material {
    info: String,
    kid: String,
    sid: String,
    sequence: u32,
    timestamp: i64,
    request_key: [u8; 32],
    response_key: [u8; 32],
}

struct Slot {
    session: tokio::sync::Mutex<Option<Session>>,
}

static SLOTS: Lazy<Mutex<HashMap<String, Arc<Slot>>>> = Lazy::new(|| Mutex::new(HashMap::new()));

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn slot(origin: &str, scope: &str) -> Arc<Slot> {
    let key = format!("{origin}\n{scope}");
    let mut map = SLOTS.lock().expect("加密会话锁失败");
    map.entry(key)
        .or_insert_with(|| {
            Arc::new(Slot {
                session: tokio::sync::Mutex::new(None),
            })
        })
        .clone()
}

fn b64url_decode(value: &str) -> Result<Vec<u8>, String> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
        return Err("加密响应编码无效".into());
    }
    B64URL.decode(value).map_err(|_| "加密响应编码无效".into())
}

fn b64_decode(value: &str) -> Result<Vec<u8>, String> {
    B64.decode(value).or_else(|_| B64URL.decode(value)).map_err(|_| "接口正文编码无效".into())
}

fn token_ok(value: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&value.len())
        && value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn site_ok(value: &str) -> bool {
    (1..=256).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b':' | b'[' | b']' | b'-'))
}

fn derive_key(shared: &[u8], salt: &[u8], info: &str) -> Result<[u8; 32], String> {
    let hk = Hkdf::<Sha256>::new(Some(salt), shared);
    let mut key = [0u8; 32];
    hk.expand(info.as_bytes(), &mut key).map_err(|_| "加密密钥派生失败")?;
    Ok(key)
}

fn seal(key: &[u8; 32], iv: &[u8; 12], aad: &str, plain: &[u8]) -> Result<Vec<u8>, String> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| "加密密钥无效")?;
    cipher
        .encrypt(
            Nonce::from_slice(iv),
            Payload { msg: plain, aad: aad.as_bytes() },
        )
        .map_err(|_| "加密请求失败".to_string())
}

fn open(key: &[u8; 32], iv: &[u8; 12], aad: &str, cipher_text: &[u8]) -> Result<Vec<u8>, String> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| "加密密钥无效")?;
    cipher
        .decrypt(
            Nonce::from_slice(iv),
            Payload { msg: cipher_text, aad: aad.as_bytes() },
        )
        .map_err(|_| "加密响应无法解密".to_string())
}

fn nonce(sequence: u32) -> [u8; 12] {
    let mut iv = [0u8; 12];
    iv[8..].copy_from_slice(&sequence.to_be_bytes());
    iv
}

fn aad(info: &str, direction: &str, sequence: u32, timestamp: i64) -> String {
    format!("{info}\n{direction}\n{sequence}\n{timestamp}")
}

fn endpoint(origin: &str, scope: &str) -> String {
    format!("{origin}/api/{scope}/request")
}

fn exchanged(posted: Posted, value: Value) -> Exchanged {
    Exchanged {
        value,
        status: posted.status,
        headers_ms: posted.headers_ms,
        target_host: posted.target_host,
        response_kind: posted.response_kind,
        gateway: posted.gateway,
    }
}

fn parse_business(text: &str) -> Result<Value, String> {
    if text.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(text).map_err(|e| format!("{e}: {}", text.chars().take(160).collect::<String>()))
}

fn http_error(status: u16, text: &str) -> String {
    if status == 401 || status == 403 {
        return "401".into();
    }
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        if let Some(msg) = value.get("msg").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
            return format!("HTTP {status}: {}", msg.chars().take(160).collect::<String>());
        }
    }
    format!("HTTP {status}: {}", text.chars().take(160).collect::<String>())
}

async fn handshake(origin: &str, scope: &str) -> Result<Session, String> {
    let secret = EphemeralSecret::random(&mut OsRng);
    let public = secret.public_key().to_encoded_point(false);
    let client_pub = B64URL.encode(public.as_bytes());
    let posted = post_json(
        &endpoint(origin, scope),
        &json!({ "v": 1, "type": "hello", "client_pub": client_pub }),
        &[
            ("Content-Type", "application/json"),
            ("Referer", &format!("{origin}/")),
        ],
        None,
    )
    .await?;
    if posted.status == 401 || posted.status == 403 {
        return Err("401".into());
    }
    if !(200..300).contains(&posted.status) {
        return Err("加密握手失败".into());
    }
    let hello: Value = serde_json::from_str(&posted.text).map_err(|_| "加密握手响应无效")?;
    let kid = hello.get("kid").and_then(|v| v.as_str()).unwrap_or("");
    let sid = hello.get("sid").and_then(|v| v.as_str()).unwrap_or("");
    let site = hello.get("site").and_then(|v| v.as_str()).unwrap_or("");
    let server_time = hello.get("server_time").and_then(|v| v.as_i64());
    let expires_at = hello.get("expires_at").and_then(|v| v.as_i64());
    if hello.get("v").and_then(|v| v.as_i64()) != Some(1)
        || hello.get("type").and_then(|v| v.as_str()) != Some("hello")
        || !token_ok(kid, 1, 64)
        || !token_ok(sid, 16, 128)
        || !site_ok(site)
    {
        return Err("加密握手响应无效".into());
    }
    let (Some(server_time), Some(expires_at)) = (server_time, expires_at) else {
        return Err("加密握手响应无效".into());
    };
    if expires_at <= server_time || expires_at - server_time > 600 {
        return Err("加密握手响应无效".into());
    }
    let server_pub = b64url_decode(hello.get("server_pub").and_then(|v| v.as_str()).unwrap_or(""))?;
    let salt = b64url_decode(hello.get("salt").and_then(|v| v.as_str()).unwrap_or(""))?;
    if salt.len() != 32 || EncodedPoint::from_bytes(&server_pub).is_err() {
        return Err("加密握手响应无效".into());
    }
    let server_key = PublicKey::from_sec1_bytes(&server_pub).map_err(|_| "加密握手响应无效")?;
    let shared = secret.diffie_hellman(&server_key);
    let info = [PROTOCOL, scope, site, kid, sid].join("\n");
    let request_key = derive_key(shared.raw_secret_bytes(), &salt, &format!("{info}\nreq"))?;
    let response_key = derive_key(shared.raw_secret_bytes(), &salt, &format!("{info}\nres"))?;
    let local = now_secs();
    Ok(Session {
        info,
        kid: kid.to_string(),
        sid: sid.to_string(),
        clock_offset: server_time - local,
        expires_at: local + expires_at - server_time,
        sequence: 0,
        request_key,
        response_key,
    })
}

fn stale(session: &Session) -> bool {
    session.expires_at <= now_secs() + 5 || session.sequence >= MAX_SEQUENCE
}

async fn material(origin: &str, scope: &str) -> Result<Material, String> {
    let slot = slot(origin, scope);
    let mut guard = slot.session.lock().await;
    if guard.as_ref().map(stale).unwrap_or(true) {
        *guard = Some(handshake(origin, scope).await?);
    }
    let session = guard.as_mut().expect("加密会话缺失");
    session.sequence += 1;
    if session.sequence > MAX_SEQUENCE {
        return Err("加密会话已到期".into());
    }
    Ok(Material {
        info: session.info.clone(),
        kid: session.kid.clone(),
        sid: session.sid.clone(),
        sequence: session.sequence,
        timestamp: now_secs() + session.clock_offset,
        request_key: session.request_key,
        response_key: session.response_key,
    })
}

async fn invalidate(origin: &str, scope: &str, sid: &str) {
    let slot = slot(origin, scope);
    let mut guard = slot.session.lock().await;
    if guard.as_ref().is_some_and(|session| session.sid == sid) {
        *guard = None;
    }
}

fn business_from_inner(inner: &Value) -> Result<Value, String> {
    let status = inner.get("status").and_then(|v| v.as_i64()).ok_or("加密接口响应格式无效")?;
    if !(200..=599).contains(&status) {
        return Err("加密接口响应格式无效".into());
    }
    let headers = inner.get("headers").ok_or("加密接口响应格式无效")?;
    if !headers.is_object() {
        return Err("加密接口响应格式无效".into());
    }
    let body = inner.get("body").and_then(|v| v.as_str()).unwrap_or("");
    let no_body = matches!(status, 204 | 205 | 304);
    let text = if no_body || body.is_empty() {
        String::new()
    } else {
        String::from_utf8(b64_decode(body)?).map_err(|_| "加密接口响应格式无效")?
    };
    if !(200..300).contains(&status) {
        return Err(http_error(status as u16, &text));
    }
    parse_business(&text)
}

async fn encrypted(origin: &str, scope: &str, packet: &Value) -> Result<Exchanged, String> {
    for _ in 0..2 {
        let material = material(origin, scope).await?;
        let iv = nonce(material.sequence);
        let plain = serde_json::to_vec(packet).map_err(|e| e.to_string())?;
        let cipher_text = seal(
            &material.request_key,
            &iv,
            &aad(&material.info, "req", material.sequence, material.timestamp),
            &plain,
        )?;
        let posted = post_json(
            &endpoint(origin, scope),
            &json!({
                "v": 1,
                "type": "call",
                "kid": material.kid,
                "sid": material.sid,
                "seq": material.sequence,
                "ts": material.timestamp,
                "ciphertext": B64URL.encode(cipher_text),
            }),
            &[
                ("Content-Type", "application/json"),
                ("Referer", &format!("{origin}/")),
            ],
            None,
        )
        .await?;
        if !(200..300).contains(&posted.status) {
            let failure = serde_json::from_str::<Value>(&posted.text).unwrap_or(Value::Null);
            if failure.get("error").and_then(|v| v.as_str()) == Some("session_expired") {
                invalidate(origin, scope, &material.sid).await;
                continue;
            }
            return Err(format!("加密接口请求失败 ({})", posted.status));
        }
        let result: Value = serde_json::from_str(&posted.text).map_err(|_| "加密接口响应不匹配")?;
        if result.get("v").and_then(|v| v.as_i64()) != Some(1)
            || result.get("type").and_then(|v| v.as_str()) != Some("result")
            || result.get("sid").and_then(|v| v.as_str()) != Some(material.sid.as_str())
            || result.get("kid").and_then(|v| v.as_str()) != Some(material.kid.as_str())
            || result.get("seq").and_then(|v| v.as_u64()) != Some(material.sequence as u64)
            || result.get("ts").and_then(|v| v.as_i64()) != Some(material.timestamp)
        {
            return Err("加密接口响应不匹配".into());
        }
        let cipher_text = b64url_decode(result.get("ciphertext").and_then(|v| v.as_str()).unwrap_or(""))?;
        let plain = open(
            &material.response_key,
            &iv,
            &aad(&material.info, "res", material.sequence, material.timestamp),
            &cipher_text,
        )?;
        let inner: Value = serde_json::from_slice(&plain).map_err(|_| "加密接口响应格式无效")?;
        return Ok(exchanged(posted, business_from_inner(&inner)?));
    }
    Err("加密接口请求失败".into())
}

fn canonical_public_path(path: &str) -> String {
    let Some((pathname, query)) = path.split_once('?') else {
        return path.to_string();
    };
    let mut pairs: Vec<(String, String)> = url::form_urlencoded::parse(query.as_bytes()).into_owned().collect();
    pairs.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    if pairs.is_empty() {
        return pathname.to_string();
    }
    let query = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .finish();
    format!("{pathname}?{query}")
}

async fn public_json(origin: &str, method: &str, path: &str, body: &str, content_type: &str) -> Result<Result<Exchanged, ()>, String> {
    let packet = json!({
        "method": method,
        "path": canonical_public_path(path),
        "body": body,
        "content_type": content_type,
    });
    let posted = post_json(
        &endpoint(origin, "public"),
        &packet,
        &[
            ("Content-Type", "application/json"),
            ("X-HG-Public-JSON", "1"),
            ("Referer", &format!("{origin}/")),
        ],
        Some("x-hg-public-json"),
    )
    .await?;
    let marked = posted.extra_header.as_deref() == Some("1");
    if posted.status == 400 && !marked {
        let failure = serde_json::from_str::<Value>(&posted.text).unwrap_or(Value::Null);
        let error = failure.get("error").and_then(|v| v.as_str()).unwrap_or("");
        if error == "invalid_request" || error == "unsupported_version" {
            return Ok(Err(()));
        }
    }
    if !(200..300).contains(&posted.status) {
        return Err(http_error(posted.status, &posted.text));
    }
    let value = parse_business(&posted.text)?;
    Ok(Ok(exchanged(posted, value)))
}

pub async fn exchange(
    origin: &str,
    method: &str,
    path: &str,
    body: &str,
    content_type: &str,
    token: Option<&str>,
) -> Result<Exchanged, String> {
    let scope = request_scope(method, path)?;
    if scope == "public" {
        match public_json(origin, method, path, body, content_type).await? {
            Ok(value) => return Ok(value),
            Err(()) => {}
        }
    }
    let mut packet = json!({
        "request": {
            "method": method,
            "path": if scope == "public" { canonical_public_path(path) } else { path.to_string() },
            "body": body,
            "content_type": content_type,
        }
    });
    if scope != "public" {
        if let Some(token) = token.map(str::trim).filter(|token| !token.is_empty()) {
            packet["authorization"] = json!(format!("Bearer {token}"));
        }
    }
    encrypted(origin, scope, &packet).await
}

fn sort_is_random(path: &str) -> bool {
    let Some(query) = path.split_once('?').map(|(_, query)| query) else {
        return false;
    };
    url::form_urlencoded::parse(query.as_bytes()).any(|(key, value)| {
        key == "sort" && (value.eq_ignore_ascii_case("random") || value.eq_ignore_ascii_case("rand"))
    })
}

pub fn request_scope(method: &str, path: &str) -> Result<&'static str, String> {
    let mut method = method.to_ascii_uppercase();
    if method == "HEAD" {
        method = "GET".to_string();
    }
    let pathname = path.split('?').next().unwrap_or(path);
    let segments: Vec<&str> = pathname.split('/').collect();
    for (_row_method, scope, pattern) in routes().iter().filter(|(row_method, _, _)| *row_method == method) {
        let parts: Vec<&str> = pattern.split('/').collect();
        if parts.len() != segments.len() {
            continue;
        }
        let matched = parts.iter().zip(segments.iter()).all(|(pattern, segment)| {
            if let Some(name) = pattern.strip_prefix(':') {
                if matches!(name, "id" | "cid" | "ep") {
                    !segment.is_empty() && segment.bytes().all(|b| b.is_ascii_digit())
                } else {
                    !segment.is_empty()
                }
            } else {
                pattern == segment
            }
        });
        if !matched {
            continue;
        }
        if *scope == "public" && sort_is_random(path) {
            return Ok("dynamic");
        }
        return Ok(*scope);
    }
    Err("接口未配置加密通道".into())
}

fn routes() -> &'static [(&'static str, &'static str, &'static str)] {
    // 按动态段数量排序，先匹配更具体的固定路径。同长度保持站点清单里的先后。
    static ROWS: Lazy<Vec<(&'static str, &'static str, &'static str)>> = Lazy::new(|| {
        let mut rows = ROUTE_TABLE.to_vec();
        rows.sort_by_key(|(method, _, pattern)| {
            (*method, pattern.split('/').filter(|part| part.starts_with(':')).count())
        });
        rows
    });
    &ROWS
}

const ROUTE_TABLE: &[(&str, &str, &str)] = &[
    ("POST", "dynamic", "/api/edge/image"),
    ("GET", "dynamic", "/api/home"),
    ("POST", "dynamic", "/api/media/covers"),
    ("GET", "dynamic", "/api/posts/:id"),
    ("GET", "dynamic", "/api/posts/:id/comments"),
    ("POST", "dynamic", "/api/posts/:id/share"),
    ("GET", "dynamic", "/api/search"),
    ("GET", "dynamic", "/api/site/address-poster"),
    ("POST", "dynamic", "/api/site/click"),
    ("GET", "dynamic", "/api/site/entries"),
    ("GET", "dynamic", "/api/site/go-home"),
    ("POST", "dynamic", "/api/site/home-slots"),
    ("POST", "dynamic", "/api/stats/click"),
    ("POST", "dynamic", "/api/stats/origin"),
    ("GET", "dynamic", "/api/users/:id"),
    ("GET", "dynamic", "/api/videos/:id"),
    ("GET", "dynamic", "/api/videos/:id/comments"),
    ("GET", "dynamic", "/api/videos/:id/comments/:cid/replies"),
    ("GET", "dynamic", "/api/videos/:id/next"),
    ("POST", "dynamic", "/api/videos/:id/play"),
    ("POST", "dynamic", "/api/videos/:id/episode-stats"),
    ("POST", "dynamic", "/api/videos/:id/visit"),
    ("POST", "dynamic", "/api/videos/recommend"),
    ("POST", "dynamic", "/api/videos/recommend/ref/:ref"),
    ("GET", "dynamic", "/api/whoami"),
    ("POST", "playback", "/api/media/playback/previews"),
    ("POST", "playback", "/api/media/prerolls/:id/playback"),
    ("POST", "playback", "/api/posts/:id/playback"),
    ("POST", "playback", "/api/videos/:id/episodes/:ep/playback"),
    ("GET", "playback", "/api/videos/:id/play"),
    ("POST", "playback", "/api/videos/:id/playback"),
    ("GET", "public", "/api/feed/post/today"),
    ("GET", "public", "/api/feed/recommend"),
    ("GET", "public", "/api/feed/today"),
    ("GET", "public", "/api/home/latest"),
    ("GET", "public", "/api/home/recommends"),
    ("GET", "public", "/api/posts"),
    ("GET", "public", "/api/posts/:id/adjacent"),
    ("GET", "public", "/api/posts/:id/related"),
    ("GET", "public", "/api/posts/category/:route"),
    ("GET", "public", "/api/posts/tag/:tag"),
    ("GET", "public", "/api/posts/user/:id"),
    ("GET", "public", "/api/ranks/:type"),
    ("GET", "public", "/api/search/hot"),
    ("GET", "public", "/api/search/suggest"),
    ("GET", "public", "/api/search/tags/hot"),
    ("GET", "public", "/api/site/categories/post"),
    ("GET", "public", "/api/site/categories/video"),
    ("GET", "public", "/api/site/config"),
    ("GET", "public", "/api/site/links"),
    ("GET", "public", "/api/site/menus"),
    ("GET", "public", "/api/site/meta"),
    ("GET", "public", "/api/site/pages"),
    ("GET", "public", "/api/site/pages/:slug"),
    ("GET", "public", "/api/site/sorts/video"),
    ("GET", "public", "/api/site/transit"),
    ("GET", "public", "/api/site/watermark"),
    ("GET", "public", "/api/tags"),
    ("GET", "public", "/api/topics"),
    ("GET", "public", "/api/topics/:slug"),
    ("GET", "public", "/api/users/:id/stats"),
    ("GET", "public", "/api/videos"),
    ("GET", "public", "/api/videos/:id/related"),
    ("GET", "public", "/api/videos/category/:route"),
    ("GET", "public", "/api/videos/long"),
    ("GET", "public", "/api/videos/report-reasons"),
    ("GET", "public", "/api/videos/sort/:route"),
    ("GET", "public", "/api/videos/tag/:tag"),
    ("GET", "public", "/api/videos/user/:id"),
    ("POST", "user", "/api/account/loginByPassword"),
    ("POST", "user", "/api/account/logout"),
    ("GET", "user", "/api/account/me"),
    ("POST", "user", "/api/account/password"),
    ("PATCH", "user", "/api/account/profile"),
    ("POST", "user", "/api/account/registerByPassword"),
    ("POST", "user", "/api/ai-wish"),
    ("DELETE", "user", "/api/posts/:id/collect"),
    ("POST", "user", "/api/posts/:id/collect"),
    ("POST", "user", "/api/posts/:id/comments"),
    ("POST", "user", "/api/posts/:id/comments/:cid/guest-like"),
    ("DELETE", "user", "/api/posts/:id/comments/:cid/like"),
    ("POST", "user", "/api/posts/:id/comments/:cid/like"),
    ("GET", "user", "/api/posts/:id/comments/liked"),
    ("POST", "user", "/api/posts/:id/guest-comments"),
    ("POST", "user", "/api/posts/:id/guest-like"),
    ("GET", "user", "/api/posts/:id/guest-liked"),
    ("DELETE", "user", "/api/posts/:id/like"),
    ("POST", "user", "/api/posts/:id/like"),
    ("POST", "user", "/api/posts/:id/visit"),
    ("DELETE", "user", "/api/users/:id/follow"),
    ("POST", "user", "/api/users/:id/follow"),
    ("GET", "user", "/api/users/me/collects"),
    ("POST", "user", "/api/users/me/collects/delete"),
    ("GET", "user", "/api/users/me/comments"),
    ("GET", "user", "/api/users/me/follows"),
    ("GET", "user", "/api/users/me/history"),
    ("POST", "user", "/api/users/me/history"),
    ("POST", "user", "/api/users/me/history/delete"),
    ("POST", "user", "/api/users/me/history/sync"),
    ("GET", "user", "/api/users/me/likes"),
    ("POST", "user", "/api/users/me/likes/delete"),
    ("POST", "user", "/api/users/me/likes/sync"),
    ("GET", "user", "/api/users/me/messages"),
    ("DELETE", "user", "/api/users/me/messages/:id"),
    ("POST", "user", "/api/users/me/messages/:id/read"),
    ("POST", "user", "/api/users/me/messages/batch-delete"),
    ("POST", "user", "/api/users/me/messages/read-all"),
    ("GET", "user", "/api/users/me/messages/unread-count"),
    ("DELETE", "user", "/api/videos/:id/collect"),
    ("POST", "user", "/api/videos/:id/collect"),
    ("POST", "user", "/api/videos/:id/comments"),
    ("POST", "user", "/api/videos/:id/comments/:cid/guest-like"),
    ("DELETE", "user", "/api/videos/:id/comments/:cid/like"),
    ("POST", "user", "/api/videos/:id/comments/:cid/like"),
    ("GET", "user", "/api/videos/:id/comments/liked"),
    ("GET", "user", "/api/videos/:id/comments/state"),
    ("POST", "user", "/api/videos/:id/feedback"),
    ("POST", "user", "/api/videos/:id/guest-comments"),
    ("POST", "user", "/api/videos/:id/guest-like"),
    ("GET", "user", "/api/videos/:id/guest-liked"),
    ("GET", "user", "/api/videos/:id/interaction-state"),
    ("DELETE", "user", "/api/videos/:id/like"),
    ("POST", "user", "/api/videos/:id/like"),
    ("GET", "user", "/api/videos/:id/rating"),
    ("POST", "user", "/api/videos/:id/rating"),
    ("POST", "user", "/api/videos/:id/report"),
    ("GET", "user", "/api/videos/following"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_follow_the_live_channel_table() {
        assert_eq!(request_scope("GET", "/api/videos?page=1&size=10").unwrap(), "public");
        assert_eq!(request_scope("GET", "/api/videos/category/duanju?page=1").unwrap(), "public");
        assert_eq!(request_scope("GET", "/api/ranks/hot").unwrap(), "public");
        assert_eq!(request_scope("GET", "/api/videos?sort=random").unwrap(), "dynamic");
        assert_eq!(request_scope("GET", "/api/search?q=a&page=1").unwrap(), "dynamic");
        assert_eq!(request_scope("GET", "/api/videos/42").unwrap(), "dynamic");
        assert_eq!(request_scope("GET", "/api/posts/7").unwrap(), "dynamic");
        assert_eq!(request_scope("POST", "/api/videos/42/episodes/3/playback").unwrap(), "playback");
        assert_eq!(request_scope("GET", "/api/videos/42/play?ep=1").unwrap(), "playback");
        assert_eq!(request_scope("POST", "/api/account/loginByPassword").unwrap(), "user");
        assert_eq!(request_scope("DELETE", "/api/videos/42/like?ep=1").unwrap(), "user");
        assert!(request_scope("GET", "/api/videos/nope").is_err());
    }

    #[tokio::test]
    #[ignore = "联网测试：确认新通道能拿到列表、详情和播放地址"]
    async fn live_feed_detail_and_playback() {
        let list = crate::net::edge(
            crate::net::DEFAULT_BASE,
            "GET",
            "/api/videos?page=1&size=1",
            None,
            None,
        )
        .await
        .expect("feed");
        let id = list["items"][0]["id"].as_i64().expect("id");
        assert!(id > 0);
        let detail = crate::net::edge(
            crate::net::DEFAULT_BASE,
            "GET",
            &format!("/api/videos/{id}"),
            None,
            None,
        )
        .await
        .expect("detail");
        assert!(!detail["title"].as_str().unwrap_or("").is_empty());
        let play = crate::net::edge(
            crate::net::DEFAULT_BASE,
            "POST",
            &format!("/api/videos/{id}/episodes/1/playback"),
            Some(json!({})),
            None,
        )
        .await
        .expect("playback");
        let main = play["streams"]["main"].as_str().unwrap_or("");
        assert!(main.contains(".m3u8"), "播放地址缺失");
    }
}
