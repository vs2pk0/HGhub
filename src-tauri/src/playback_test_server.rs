//! Loopback bridge for browser integration tests, compiled only into the Rust test binary.
use serde_json::{json, Value};
use std::{io::{Read, Write}, sync::{Arc, atomic::{AtomicBool, Ordering}}};

async fn command(name: &str, args: Value) -> Result<Value, String> {
    let url = || args["url"].as_str().unwrap_or_default().to_string();
    let session = || args["sessionId"].as_str().unwrap_or_default().to_string();
    match name {
        "status" => Ok(crate::api::playback_cache_status().await),
        "set" => { crate::api::playback_cache_set(args["maxMib"].as_u64().ok_or("missing limit")? as u32).await?; Ok(Value::Null) }
        "contains" => Ok(json!(crate::api::playback_cache_contains(serde_json::from_value(args["urls"].clone()).map_err(|e| e.to_string())?).await)),
        "prefetch" => Ok(json!(crate::api::playback_cache_prefetch(url(),
            serde_json::from_value(args["range"].clone()).map_err(|e| e.to_string())?, args["validator"].as_str().map(str::to_string),
            serde_json::from_value(args["protectedUrls"].clone()).map_err(|e| e.to_string())?, session(), args["focus"].as_u64()).await?)),
        "observed" => Ok(json!(crate::api::playback_cache_observed(serde_json::from_value(args["urls"].clone()).map_err(|e| e.to_string())?).await)),
        "probe" => crate::api::playback_cache_probe(url(), session()).await,
        "cursor" => Ok(json!(crate::api::playback_cache_cursor(url()).await)),
        "pending" => Ok(crate::api::playback_cache_pending(url()).await),
        "ready" => { crate::api::playback_cache_ready(session(), args["ready"].as_bool().unwrap_or(false)).await; Ok(Value::Null) }
        "start" => { crate::api::playback_cache_start(session()).await?; Ok(Value::Null) }
        "stop" => { crate::api::playback_cache_stop(session()).await; Ok(Value::Null) }
        "clear" => { crate::api::cache_clear().await?; Ok(Value::Null) }
        _ => Err("unknown fixture command".into()),
    }
}

#[test]
#[ignore = "browser fixture server; started and stopped by scripts/test-playback.mjs"]
fn browser_media_fixture() {
    let dir = std::path::PathBuf::from(std::env::var("HGHUB_TEST_CACHE_DIR").expect("fixture cache directory"));
    std::fs::create_dir_all(&dir).unwrap();
    crate::network_policy::bind_security(&dir);
    crate::network_policy::set_strict_security(false).unwrap();
    crate::media_cache::bind(&dir);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    println!("HGHUB_TEST_BRIDGE=http://{}", listener.local_addr().unwrap());
    std::io::stdout().flush().unwrap();
    let stopped = Arc::new(AtomicBool::new(false));
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut workers = Vec::new();
    while !stopped.load(Ordering::SeqCst) {
        let (mut stream, _) = match listener.accept() {
            Ok(connection) => connection,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(2)); continue;
            }
            Err(error) => panic!("{error}"),
        };
        let stopped = stopped.clone();
        let handle = runtime.handle().clone();
        workers.push(std::thread::spawn(move || {
            stream.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                if stream.read(&mut byte).unwrap_or(0) == 0 { return; }
                request.push(byte[0]);
                if request.len() > 16384 { return; }
            }
            let header = String::from_utf8(request).unwrap();
            let mut line = header.lines().next().unwrap().split_whitespace();
            let method = line.next().unwrap();
            let target = line.next().unwrap();
            let header_value = |name: &str| header.lines().find_map(|line| line.split_once(':')
                .filter(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.trim()));
            let length: usize = header_value("Content-Length").unwrap_or("0").parse().unwrap();
            if length > 4 * 1024 * 1024 { return; }
            let mut body = vec![0; length];
            if stream.read_exact(&mut body).is_err() { return; }
            let (path, query) = target.split_once('?').unwrap_or((target, ""));
            let response = if method == "OPTIONS" {
                tauri::http::Response::builder().status(204).body(Vec::new()).unwrap()
            } else if path == "/api/shutdown" {
                stopped.store(true, Ordering::SeqCst);
                handle.block_on(crate::api::cache_clear()).unwrap();
                tauri::http::Response::builder().body(b"null".to_vec()).unwrap()
            } else if let Some(command_name) = path.strip_prefix("/api/") {
                let args = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let result = handle.block_on(command(command_name, args));
                let (status, value) = match result { Ok(value) => (200, value), Err(error) => (500, json!({"error": error})) };
                tauri::http::Response::builder().status(status).header("Content-Type", "application/json")
                    .body(serde_json::to_vec(&value).unwrap()).unwrap()
            } else {
                handle.block_on(crate::media::handle(path, query, "http://hg.localhost", header_value("Range")))
            };
            let mut output = format!("HTTP/1.1 {} OK\r\nConnection: close\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Headers: Content-Type, Range\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\n", response.status().as_u16());
            for (name, value) in response.headers() {
                if !["content-length", "access-control-allow-origin"].contains(&name.as_str()) {
                    output.push_str(&format!("{name}: {}\r\n", value.to_str().unwrap()));
                }
            }
            output.push_str(&format!("Content-Length: {}\r\n\r\n", response.body().len()));
            let _ = stream.write_all(output.as_bytes()).and_then(|_| stream.write_all(response.body()));
        }));
    }
    for worker in workers { worker.join().unwrap(); }
}
