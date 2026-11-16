#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod engine;
mod model;
mod platform;

use hickory_proto::{
    op::{Message, MessageType, Query},
    rr::{Name, RData, RecordType},
};
use model::{Config, DiagnosticResult, Snapshot};
use std::{
    net::UdpSocket,
    process::Command,
    time::{Duration, Instant},
};

#[derive(Default)]
struct Operations(tokio::sync::Mutex<()>);

#[tauri::command]
async fn get_snapshot(app: tauri::AppHandle) -> Result<Snapshot, String> {
    tauri::async_runtime::spawn_blocking(move || platform::snapshot(&app))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn save_config(
    app: tauri::AppHandle,
    state: tauri::State<'_, Operations>,
    config: Config,
) -> Result<(), String> {
    let _guard = state.0.lock().await;
    tauri::async_runtime::spawn_blocking(move || {
        config.validate()?;
        if platform::service_loaded() {
            platform::install_service(&app, config)
        } else {
            platform::save_config(&app, config)
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn start_service(
    app: tauri::AppHandle,
    state: tauri::State<'_, Operations>,
    config: Config,
) -> Result<(), String> {
    let _guard = state.0.lock().await;
    tauri::async_runtime::spawn_blocking(move || platform::install_service(&app, config))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn stop_service(state: tauri::State<'_, Operations>) -> Result<(), String> {
    let _guard = state.0.lock().await;
    tauri::async_runtime::spawn_blocking(platform::stop_service)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn uninstall_service(state: tauri::State<'_, Operations>) -> Result<(), String> {
    let _guard = state.0.lock().await;
    tauri::async_runtime::spawn_blocking(platform::uninstall_service)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn diagnose(app: tauri::AppHandle, hostname: String) -> Result<DiagnosticResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let config = platform::load_config(&app)?;
        if !model::valid_dns_name(&hostname)
            || (hostname != config.domain && !hostname.ends_with(&format!(".{}", config.domain)))
        {
            return Err("请输入当前开发域名下的有效主机名。".into());
        }
        let expected_ip = platform::detect_network(&config).ok().map(|n| n.ip);
        let start = Instant::now();
        let answer = query_local_dns(&hostname);
        let (resolved_ip, message) = match answer {
            Ok(ip) => (Some(ip), String::new()),
            Err(error) => (None, error),
        };
        let ok = expected_ip.is_some() && resolved_ip == expected_ip;
        let message = if ok {
            "本机 DNS 返回了当前局域网地址。请在另一台设备上访问域名以验证网络接入。".into()
        } else if message.is_empty() {
            "DNS 返回地址与当前网络不一致，请检查服务状态后重试。".into()
        } else {
            format!("DNS 查询失败：{message}。请确认服务已启动且 53 端口未被占用。")
        };
        Ok(DiagnosticResult {
            hostname,
            expected_ip,
            resolved_ip,
            elapsed_ms: start.elapsed().as_millis() as u64,
            ok,
            message,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

fn query_local_dns(hostname: &str) -> Result<String, String> {
    let socket = UdpSocket::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    socket.connect("127.0.0.1:53").map_err(|e| e.to_string())?;
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .map_err(|e| e.to_string())?;
    let name = Name::from_ascii(hostname).map_err(|e| e.to_string())?;
    let mut query = Message::new();
    query
        .set_id(socket.local_addr().map_err(|e| e.to_string())?.port())
        .set_recursion_desired(true)
        .add_query(Query::query(name, RecordType::A));
    socket
        .send(&query.to_vec().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let mut bytes = [0_u8; 4096];
    let size = socket.recv(&mut bytes).map_err(|e| e.to_string())?;
    let response = Message::from_vec(&bytes[..size]).map_err(|e| e.to_string())?;
    if response.id() != query.id()
        || response.message_type() != MessageType::Response
        || response.queries() != query.queries()
    {
        return Err("收到不匹配的 DNS 响应".into());
    }
    response
        .answers()
        .iter()
        .find_map(|r| match r.data() {
            RData::A(ip) => Some(ip.0.to_string()),
            _ => None,
        })
        .ok_or_else(|| format!("未收到 A 记录（{}）", response.response_code()))
}

#[tauri::command]
fn open_url(app: tauri::AppHandle, url: String) -> Result<(), String> {
    let parsed = url::Url::parse(&url).map_err(|_| "地址格式无效。")?;
    let config = platform::load_config(&app)?;
    let host = parsed.host_str().unwrap_or_default();
    let local = host == config.domain || host.ends_with(&format!(".{}", config.domain));
    let docs = ["support.apple.com", "v2.tauri.app"].contains(&host);
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || !(local && parsed.scheme() == "http" && parsed.port_or_known_default() == Some(80)
            || docs && parsed.scheme() == "https" && parsed.port_or_known_default() == Some(443))
    {
        return Err("仅允许打开当前开发域名的 HTTP 页面或官方帮助页面。".into());
    }
    let status = Command::new("/usr/bin/open")
        .arg(parsed.as_str())
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err("无法打开浏览器，请检查默认浏览器设置。".into())
    }
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--service") {
        if let Err(error) = engine::run() {
            eprintln!("LanDNS: {error}");
            std::process::exit(1);
        }
        return;
    }
    tauri::Builder::default()
        .manage(Operations::default())
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            save_config,
            start_service,
            stop_service,
            uninstall_service,
            diagnose,
            open_url
        ])
        .run(tauri::generate_context!())
        .expect("无法启动 LanDNS 桌面应用");
}
