mod api;
mod lines;
mod logging;
#[cfg(target_os = "macos")]
mod macos_titlebar;
mod media;
mod media_cache;
mod media_download;
mod net;
mod network_policy;
mod playback_log;
#[cfg(test)]
mod playback_test_server;
mod session;

use api::Api;
use tauri::Manager;

fn allowed_dev_host(host: &str) -> bool {
    if matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]") {
        return true;
    }
    if let Some(configured) = option_env!("TAURI_DEV_HOST") {
        if host.eq_ignore_ascii_case(configured.trim()) {
            return true;
        }
    }
    std::env::var("TAURI_DEV_HOST")
        .ok()
        .is_some_and(|v| host.eq_ignore_ascii_case(v.trim()))
}

fn allowed_frontend_origin(origin: &str) -> bool {
    if matches!(
        origin,
        "tauri://localhost" | "http://tauri.localhost" | "https://tauri.localhost" | "null"
    ) {
        return true;
    }
    // 移动端开发可能使用 TAURI_DEV_HOST；严格模式下仅放行本机或显式配置的 TAURI_DEV_HOST。
    cfg!(debug_assertions)
        && url::Url::parse(origin).is_ok_and(|u| {
            u.scheme() == "http"
                && u.port() == Some(1420)
                && (!network_policy::strict_security()
                    || u.host_str().is_some_and(allowed_dev_host))
        })
}

fn allowed_navigation(url: &url::Url, strict: bool) -> bool {
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    // "null" is needed for media CORS on opaque WebView origins, but must
    // never authorize navigation to data:, file:, javascript: or other schemes.
    match url.scheme() {
        "tauri" => url.host_str() == Some("localhost"),
        "http" | "https" => {
            !strict || allowed_frontend_origin(&url.origin().ascii_serialization())
        }
        _ => false,
    }
}

#[cfg(test)]
mod navigation_tests {
    #[test]
    fn opaque_media_origins_do_not_authorize_navigation() {
        for raw in ["data:text/html,<script>alert(1)</script>", "file:///etc/passwd", "javascript:alert(1)", "tauri://evil/index.html", "about:blank"] {
            let url = url::Url::parse(raw).unwrap();
            assert!(!super::allowed_navigation(&url, true), "{raw}");
            assert!(!super::allowed_navigation(&url, false), "{raw}");
        }
        for raw in ["tauri://localhost/index.html", "http://tauri.localhost/index.html", "https://tauri.localhost/"] {
            assert!(super::allowed_navigation(&url::Url::parse(raw).unwrap(), true), "{raw}");
        }
        let external = url::Url::parse("https://example.com/").unwrap();
        assert!(!super::allowed_navigation(&external, true));
        assert!(super::allowed_navigation(&external, false));
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(
            tauri_plugin_opener::Builder::new()
                .open_js_links_on_click(false)
                .build(),
        )
        .manage(Api::new())
        .setup(|app| {
            if let Ok(dir) = app.path().app_data_dir() {
                let _ = std::fs::create_dir_all(&dir);
                // 上次选择的线路（设置 → 线路）在启动时恢复
                net::bind_base(&dir);
                playback_log::bind(&dir);
                media_cache::bind(&dir);
                let state: tauri::State<Api> = app.state();
                state.session.bind(dir);
            }
            // macOS 主窗口在此创建：先按系统红绿灯尺寸算出居中位置（见 macos_titlebar.rs）
            #[cfg(target_os = "macos")]
            macos_titlebar::create_main_window(app)?;
            Ok(())
        })
        .plugin(
            tauri::plugin::Builder::<_, ()>::new("navigation-guard")
                .on_navigation(|_webview, url| {
                    allowed_navigation(url, network_policy::strict_security())
                })
                .build(),
        )
        .register_asynchronous_uri_scheme_protocol("hg", |_app, request, responder| {
            let request_origin = request
                .headers()
                .get("Origin")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            if playback_log::enabled() {
                let resource = url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes())
                    .find(|(key, _)| key == "u")
                    .map(|(_, value)| playback_log::resource_id(&value));
                let route = match request.uri().path() { "/hls" => "playlist", "/seg" => "segment_or_key", "/img" => "image", _ => "unknown" };
                let range = request.headers().get("Range").and_then(|v| v.to_str().ok())
                    .filter(|v| v.len() <= 100 && v.bytes().all(|b| b.is_ascii_digit() || b"bytes=,- ".contains(&b)));
                let origin_kind = match request_origin.as_deref() { None => "missing", Some("null") => "null", Some(origin) if allowed_frontend_origin(origin) => "app", _ => "other" };
                playback_log::record("protocol.request", serde_json::json!({"resource": resource, "route": route, "method": request.method().as_str(), "range": range, "origin_kind": origin_kind}));
            }
            if network_policy::strict_security()
                && request_origin
                    .as_deref()
                    .is_some_and(|origin| !allowed_frontend_origin(origin))
            {
                playback_log::record("security.origin_denied", serde_json::json!({"origin_null": request_origin.as_deref() == Some("null")}));
                responder.respond(
                    tauri::http::Response::builder()
                        .status(403)
                        .body(b"origin denied".to_vec())
                        .unwrap(),
                );
                return;
            }
            let range = request.headers().get("Range").and_then(|v| v.to_str().ok()).map(str::to_string);
            let uri = request.uri().clone();
            let path = uri.path().to_string();
            let query = uri.query().unwrap_or("").to_string();
            // 自定义协议基址随平台不同，统一定义在 media::LOCAL_ORIGIN
            // （Windows/Android 是 http://<scheme>.localhost，macOS/iOS/Linux 是 <scheme>://localhost）。
            // 它同时是 m3u8 分片/key 改写用的前缀，必须与前端 src/api.ts 的 LOCAL 一致。
            tauri::async_runtime::spawn(async move {
                let mut resp = media::handle(&path, &query, media::LOCAL_ORIGIN, range.as_deref()).await;
                if let Some(origin) =
                    request_origin.and_then(|s| tauri::http::HeaderValue::from_str(&s).ok())
                {
                    resp.headers_mut()
                        .insert("Access-Control-Allow-Origin", origin);
                    resp.headers_mut()
                        .insert("Vary", tauri::http::HeaderValue::from_static("Origin"));
                } else {
                    resp.headers_mut().insert(
                        "Access-Control-Allow-Origin",
                        tauri::http::HeaderValue::from_static("*"),
                    );
                }
                responder.respond(resp);
            });
        })
        .invoke_handler(tauri::generate_handler![
            api::app_boot,
            playback_log::playback_log_status,
            playback_log::playback_log_set,
            playback_log::playback_log_write,
            playback_log::playback_log_read,
            api::security_set,
            api::security_cidrs_get,
            api::security_cidrs_set,
            api::debug_log,
            api::net_diag,
            api::line_state,
            api::line_set,
            api::line_mirrors,
            api::line_probe,
            api::check_update,
            api::open_url,
            api::set_orientation,
            api::api_login,
            api::api_register,
            api::api_logout,
            api::api_me,
            api::api_feed,
            api::api_category,
            api::api_rank,
            api::api_search,
            api::cache_clear,
            api::playback_cache_status,
            api::playback_cache_set,
            api::playback_cache_prefetch,
            api::playback_cache_observed,
            api::playback_cache_probe,
            api::playback_cache_cursor,
            api::playback_cache_pending,
            api::playback_cache_ready,
            api::playback_cache_start,
            api::playback_cache_stop,
            api::playback_cache_contains,
            api::api_posts,
            api::api_post,
            api::api_post_like,
            api::api_post_collect,
            api::api_post_comments,
            api::api_video,
            api::api_play,
            api::api_comments,
            api::api_comment_send,
            api::api_like,
            api::api_collect,
            api::api_follow,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
