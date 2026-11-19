use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    net::TcpStream,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Mutex,
    thread,
    time::{Duration, SystemTime},
};
use tauri::Manager;
use url::Url;

const INSPECT_PORT: u16 = 4040;
const AUTHTOKEN_PAGE: &str = "https://dashboard.ngrok.com/get-started/your-authtoken";

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NgrokSettings {
    pub target: String,
    pub public_url: String,
    pub host_header: String,
    pub traffic_policy_file: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NgrokTunnel {
    pub public_url: String,
    pub proto: String,
    pub addr: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NgrokStatus {
    pub installed: bool,
    pub binary: Option<String>,
    pub version: Option<String>,
    pub authtoken_configured: bool,
    pub config_path: Option<String>,
    pub running: bool,
    pub owned: bool,
    pub pid: Option<u32>,
    pub tunnels: Vec<NgrokTunnel>,
    pub inspect_url: Option<String>,
    pub logs: Vec<String>,
    pub settings: NgrokSettings,
    pub message: Option<String>,
}

#[derive(Deserialize)]
struct ApiTunnels {
    tunnels: Vec<ApiTunnel>,
}

#[derive(Deserialize)]
struct ApiTunnel {
    public_url: String,
    proto: String,
    config: Option<ApiTunnelConfig>,
}

#[derive(Deserialize)]
struct ApiTunnelConfig {
    addr: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Session {
    pid: u32,
}

fn lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|error| error.into_inner())
}

fn config_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_config_dir()
        .map_err(|error| error.to_string())
}

fn settings_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(config_dir(app)?.join("ngrok.json"))
}

fn session_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(config_dir(app)?.join("ngrok-session.json"))
}

fn log_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(config_dir(app)?.join("ngrok.log"))
}

fn candidate_binaries() -> Vec<PathBuf> {
    let mut candidates = vec![
        PathBuf::from("/opt/homebrew/bin/ngrok"),
        PathBuf::from("/usr/local/bin/ngrok"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join(".local/bin/ngrok"));
    }
    if let Some(path) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&path) {
            candidates.push(directory.join("ngrok"));
        }
    }
    candidates
}

fn find_binary() -> Option<PathBuf> {
    if let Some(path) = candidate_binaries()
        .into_iter()
        .find(|path| path.is_file() && is_executable(path))
    {
        return Some(path);
    }
    let output = Command::new("/usr/bin/which")
        .arg("ngrok")
        .env(
            "PATH",
            format!(
                "/opt/homebrew/bin:/usr/local/bin:{}",
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = PathBuf::from(String::from_utf8(output.stdout).ok()?.trim());
    (path.is_file() && is_executable(&path)).then_some(path)
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path)
            .map(|meta| meta.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

fn command_line(pid: u32) -> Option<String> {
    let output = Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let line = text.trim();
    if line.is_empty() {
        None
    } else {
        Some(line.to_string())
    }
}

fn is_ngrok_command(line: &str) -> bool {
    line.split_whitespace()
        .next()
        .is_some_and(|program| program == "ngrok" || program.ends_with("/ngrok"))
}

fn session_pid(app: &tauri::AppHandle) -> Option<u32> {
    let path = session_path(app).ok()?;
    let bytes = fs::read(path).ok()?;
    serde_json::from_slice::<Session>(&bytes)
        .ok()
        .map(|session| session.pid)
}

fn owned_pid(app: &tauri::AppHandle) -> Option<u32> {
    let pid = session_pid(app)?;
    command_line(pid).filter(|line| is_ngrok_command(line))?;
    Some(pid)
}

fn listener_pids() -> Vec<u32> {
    let output = Command::new("/usr/sbin/lsof")
        .args(["-nP", "-iTCP:4040", "-sTCP:LISTEN", "-t"])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8(output.stdout)
        .unwrap_or_default()
        .split_whitespace()
        .filter_map(|pid| pid.parse().ok())
        .filter(|pid| command_line(*pid).is_some_and(|line| is_ngrok_command(&line)))
        .collect()
}

fn running_pid(app: &tauri::AppHandle) -> Option<u32> {
    owned_pid(app).or_else(|| listener_pids().into_iter().next())
}

fn version_of(binary: &Path) -> Option<String> {
    let output = Command::new(binary).arg("version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    text.split_whitespace().last().map(str::to_string)
}

fn config_has_authtoken(text: &str) -> bool {
    text.lines().any(|line| {
        let trimmed = line.split('#').next().unwrap_or("").trim();
        let Some(value) = trimmed.strip_prefix("authtoken:") else {
            return false;
        };
        let value = value.trim().trim_matches(['"', '\'']);
        !value.is_empty() && value != "null" && value != "~"
    })
}

fn default_ngrok_config() -> Option<String> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    [
        "Library/Application Support/ngrok/ngrok.yml",
        ".config/ngrok/ngrok.yml",
        ".ngrok2/ngrok.yml",
    ]
    .into_iter()
    .map(|relative| home.join(relative))
    .find(|path| path.is_file())
    .map(|path| path.display().to_string())
}

fn local_account(binary: &Path) -> (bool, Option<String>) {
    let output = Command::new(binary).args(["config", "check"]).output();
    let reported = output.ok().and_then(|output| {
        let text = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        text.lines().find_map(|line| {
            let marker = " at ";
            line.rfind(marker)
                .map(|index| line[index + marker.len()..].trim().to_string())
                .filter(|path| !path.is_empty())
        })
    });
    let path = reported.or_else(default_ngrok_config);
    let configured = path
        .as_deref()
        .is_some_and(|path| fs::read_to_string(path).is_ok_and(|text| config_has_authtoken(&text)));
    let path = path.filter(|path| Path::new(path).is_file());
    (configured, path)
}

fn load_settings(app: &tauri::AppHandle) -> NgrokSettings {
    settings_path(app)
        .ok()
        .and_then(|path| fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    if let Some(directory) = path.parent() {
        fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    }
    let directory = path.parent().ok_or("配置目录无效")?;
    let mut file = tempfile::NamedTempFile::new_in(directory).map_err(|error| error.to_string())?;
    file.write_all(&serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    file.as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    file.persist(path)
        .map_err(|error| format!("无法保存 ngrok 配置：{error}"))?;
    Ok(())
}

fn save_settings(app: &tauri::AppHandle, settings: &NgrokSettings) -> Result<(), String> {
    write_json(&settings_path(app)?, settings)
}

fn write_session(app: &tauri::AppHandle, pid: u32) -> Result<(), String> {
    write_json(&session_path(app)?, &Session { pid })
}

fn clear_session(app: &tauri::AppHandle) {
    if let Ok(path) = session_path(app) {
        let _ = fs::remove_file(path);
    }
}

fn tail_log(path: &Path) -> Vec<String> {
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return Vec::new(),
    };
    let length = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let start = length.saturating_sub(64 * 1024);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut text = String::new();
    if file.read_to_string(&mut text).is_err() {
        return Vec::new();
    }
    let mut lines: Vec<String> = text
        .lines()
        .skip(if start > 0 { 1 } else { 0 })
        .map(redact_line)
        .filter(|line| !line.trim().is_empty())
        .collect();
    let keep = lines.len().saturating_sub(80);
    lines.drain(..keep);
    lines
}

fn redact_line(line: &str) -> String {
    let mut words = Vec::new();
    let mut hide_next = false;
    for word in line.split_whitespace() {
        let lower = word.to_ascii_lowercase();
        if hide_next {
            words.push("<已隐藏>".to_string());
            hide_next = false;
            continue;
        }
        if lower.contains("authtoken") || lower.contains("api-key") {
            if let Some((key, value)) = word.split_once('=') {
                if !value.is_empty() && value != "\"\"" && !value.eq_ignore_ascii_case("authtoken")
                {
                    words.push(format!("{key}=<已隐藏>"));
                    continue;
                }
            }
            hide_next = true;
        }
        words.push(word.to_string());
    }
    words.join(" ")
}

fn log_text(line: &str) -> String {
    let Some(rest) = line.split("msg=").nth(1) else {
        return line.to_string();
    };
    if let Some(quoted) = rest.strip_prefix('"') {
        if let Some(end) = quoted.find('"') {
            return quoted[..end].to_string();
        }
    }
    rest.split_whitespace().next().unwrap_or(rest).to_string()
}

fn recent_error(path: &Path, logs: &[String]) -> Option<String> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    if modified.elapsed().unwrap_or(Duration::from_secs(u64::MAX)) > Duration::from_secs(600) {
        return None;
    }
    logs.iter().rev().find_map(|line| {
        if line.contains("lvl=error") || line.contains("lvl=crit") || line.contains("lvl=er ") {
            Some(log_text(line))
        } else {
            None
        }
    })
}

fn http_get(path: &str) -> Result<String, String> {
    let address = format!("127.0.0.1:{INSPECT_PORT}")
        .parse()
        .map_err(|error| format!("{error}"))?;
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(400))
        .map_err(|_| "无法连接 ngrok 本地接口".to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|error| error.to_string())?;
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{INSPECT_PORT}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(size) => {
                bytes.extend_from_slice(&chunk[..size]);
                if bytes.len() > 512 * 1024 {
                    return Err("ngrok 本地接口响应过大".into());
                }
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                break;
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    let raw = String::from_utf8_lossy(&bytes);
    let (head, body) = raw.split_once("\r\n\r\n").ok_or("ngrok 本地接口响应无效")?;
    let status = head.lines().next().unwrap_or("");
    if !status.contains(" 200 ") {
        return Err("ngrok 本地接口没有返回隧道信息".into());
    }
    let lower = head.to_ascii_lowercase();
    let body = if lower.contains("transfer-encoding: chunked") {
        decode_chunked(body)
    } else {
        body.to_string()
    };
    Ok(body)
}

fn decode_chunked(body: &str) -> String {
    let mut output = String::new();
    let mut rest = body;
    loop {
        let Some((size_line, after)) = rest.split_once("\r\n") else {
            break;
        };
        let size =
            usize::from_str_radix(size_line.trim().split(';').next().unwrap_or("").trim(), 16)
                .unwrap_or(0);
        if size == 0 || after.len() < size {
            break;
        }
        output.push_str(&after[..size]);
        rest = after[size..].strip_prefix("\r\n").unwrap_or("");
    }
    output
}

fn tunnels_from_api() -> Vec<NgrokTunnel> {
    let Ok(body) = http_get("/api/tunnels") else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_str::<ApiTunnels>(&body) else {
        return Vec::new();
    };
    parsed
        .tunnels
        .into_iter()
        .filter(|tunnel| {
            tunnel.public_url.starts_with("https://") || tunnel.public_url.starts_with("http://")
        })
        .map(|tunnel| NgrokTunnel {
            addr: tunnel
                .config
                .and_then(|config| config.addr)
                .unwrap_or_default(),
            proto: tunnel.proto,
            public_url: tunnel.public_url,
        })
        .collect()
}

fn tunnels_from_logs(logs: &[String]) -> Vec<NgrokTunnel> {
    let Some(url) = logs.iter().rev().find_map(|line| {
        let rest = line.split("url=").nth(1)?;
        let value = rest
            .trim_start_matches('"')
            .split([' ', '"'])
            .next()
            .unwrap_or("");
        if value.starts_with("https://") {
            Some(value.to_string())
        } else {
            None
        }
    }) else {
        return Vec::new();
    };
    vec![NgrokTunnel {
        public_url: url,
        proto: "https".into(),
        addr: String::new(),
    }]
}

fn collect(app: &tauri::AppHandle) -> Result<NgrokStatus, String> {
    let binary = find_binary();
    let version = binary.as_ref().and_then(|path| version_of(path));
    let (authtoken_configured, config_path) = binary
        .as_ref()
        .map(|path| local_account(path))
        .unwrap_or((false, None));
    let owned = owned_pid(app);
    let pid = owned.or_else(|| listener_pids().into_iter().next());
    let running = pid.is_some();
    let log_file = log_path(app).ok();
    let logs = log_file
        .as_ref()
        .filter(|path| owned.is_some() || path.is_file())
        .map(|path| tail_log(path))
        .unwrap_or_default();
    let mut tunnels = if running {
        tunnels_from_api()
    } else {
        Vec::new()
    };
    if tunnels.is_empty() && running {
        tunnels = tunnels_from_logs(&logs);
    }
    let message = if running && tunnels.is_empty() {
        Some("正在向 ngrok 申请公网地址。".into())
    } else if !running {
        log_file.as_ref().and_then(|path| recent_error(path, &logs))
    } else {
        None
    };
    Ok(NgrokStatus {
        installed: binary.is_some(),
        binary: binary.map(|path| path.display().to_string()),
        version,
        authtoken_configured,
        config_path,
        running,
        owned: owned.is_some(),
        pid,
        inspect_url: running.then_some(format!("http://127.0.0.1:{INSPECT_PORT}")),
        tunnels,
        logs,
        settings: load_settings(app),
        message,
    })
}

pub fn status(app: &tauri::AppHandle) -> Result<NgrokStatus, String> {
    let _guard = lock();
    collect(app)
}

fn draft(settings: NgrokSettings) -> Result<NgrokSettings, String> {
    let settings = NgrokSettings {
        target: settings.target.trim().to_string(),
        public_url: settings.public_url.trim().to_string(),
        host_header: settings.host_header.trim().to_string(),
        traffic_policy_file: settings.traffic_policy_file.trim().to_string(),
    };
    for value in [
        &settings.target,
        &settings.public_url,
        &settings.host_header,
        &settings.traffic_policy_file,
    ] {
        if value.chars().count() > 1024 || value.chars().any(char::is_control) {
            return Err("填写内容过长或包含无法使用的字符。".into());
        }
    }
    Ok(settings)
}

pub fn save(app: &tauri::AppHandle, settings: NgrokSettings) -> Result<(), String> {
    let _guard = lock();
    save_settings(app, &draft(settings)?)
}

fn normalize_target(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("请填写本地地址，例如 http://localhost:8080。".into());
    }
    if let Ok(port) = value.parse::<u16>() {
        if port == 0 {
            return Err("本地端口无效。".into());
        }
        return Ok(format!("http://127.0.0.1:{port}"));
    }
    let url = Url::parse(value).map_err(|_| "本地地址格式无效。请填写 http://localhost:8080。")?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("本地地址只支持本机的 http 或 https，不能包含账号、查询参数。".into());
    }
    let host = url.host_str().unwrap_or("");
    if !matches!(host, "localhost" | "127.0.0.1" | "::1") {
        return Err("本地地址只能指向 localhost 或 127.0.0.1。".into());
    }
    let port = url
        .port_or_known_default()
        .filter(|port| *port != 0)
        .ok_or("请在本地地址中填写端口。")?;
    if url.path() != "/" && !url.path().is_empty() {
        return Err("本地地址填写服务根地址即可，不要带页面路径。".into());
    }
    Ok(format!("{}://{host}:{port}", url.scheme()))
}

fn normalize_public_url(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(String::new());
    }
    let url = Url::parse(value).map_err(|_| "固定公网网址格式无效。".to_string())?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port().is_some()
        || !(url.path().is_empty() || url.path() == "/")
    {
        return Err("固定公网网址需要是 https 域名，例如 https://name.ngrok-free.dev。".into());
    }
    let host = url
        .host_str()
        .filter(|host| {
            !host.is_empty()
                && host.len() <= 253
                && host.contains('.')
                && host.split('.').all(|label| {
                    !label.is_empty()
                        && label.len() <= 63
                        && !label.starts_with('-')
                        && !label.ends_with('-')
                        && label
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                })
        })
        .ok_or("固定公网网址的域名无效。")?;
    Ok(format!("https://{}", host.to_ascii_lowercase()))
}

fn normalize_host_header(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(String::new());
    }
    if value == "rewrite" {
        return Ok(value.to_string());
    }
    let (host, port) = value
        .rsplit_once(':')
        .map_or((value, None), |(host, port)| {
            if port.chars().all(|char| char.is_ascii_digit()) {
                (host, Some(port))
            } else {
                (value, None)
            }
        });
    let valid_host = !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        });
    let valid_port = port
        .map(|port| port.parse::<u16>().ok().is_some_and(|port| port != 0))
        .unwrap_or(true);
    if !valid_host || !valid_port {
        return Err("Host 头需要是域名，或 rewrite。".into());
    }
    Ok(value.to_string())
}

fn normalize_policy(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(String::new());
    }
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err("流量策略请填写绝对路径。".into());
    }
    let meta = fs::metadata(&path).map_err(|_| "找不到流量策略文件。")?;
    if !meta.is_file() || meta.len() > 1024 * 1024 {
        return Err("流量策略必须是 1 MB 以内的文件。".into());
    }
    Ok(path.display().to_string())
}

fn prepare(settings: &NgrokSettings) -> Result<(String, String, String, String), String> {
    Ok((
        normalize_target(&settings.target)?,
        normalize_public_url(&settings.public_url)?,
        normalize_host_header(&settings.host_header)?,
        normalize_policy(&settings.traffic_policy_file)?,
    ))
}

pub fn start(app: &tauri::AppHandle, settings: NgrokSettings) -> Result<NgrokStatus, String> {
    let _guard = lock();
    let settings = draft(settings)?;
    let binary = find_binary().ok_or("未找到 ngrok。可先安装：brew install --cask ngrok")?;
    if !local_account(&binary).0 {
        return Err(
            "本机 ngrok 配置里没有 authtoken。请先打开申请页面，按页面说明写入本机配置。".into(),
        );
    }
    let (target, public_url, host_header, policy) = prepare(&settings)?;
    if running_pid(app).is_some() {
        return Err("已经有 ngrok 在运行。请先停止当前隧道。".into());
    }
    let stored = NgrokSettings {
        target: target.clone(),
        public_url: public_url.clone(),
        host_header: host_header.clone(),
        traffic_policy_file: policy.clone(),
    };
    save_settings(app, &stored)?;
    let log_file = log_path(app)?;
    if let Some(directory) = log_file.parent() {
        fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    }
    fs::write(&log_file, b"").map_err(|error| format!("无法写入 ngrok 日志：{error}"))?;

    let mut command = Command::new(&binary);
    command
        .arg("http")
        .arg(&target)
        .arg("--log")
        .arg(&log_file)
        .arg("--log-format")
        .arg("logfmt")
        .arg("--log-level")
        .arg("info");
    if !public_url.is_empty() {
        command.arg("--url").arg(&public_url);
    }
    if !host_header.is_empty() {
        command.arg("--host-header").arg(&host_header);
    }
    if !policy.is_empty() {
        command.arg("--traffic-policy-file").arg(&policy);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("无法启动 ngrok：{error}"))?;
    let pid = child.id();
    thread::spawn(move || {
        let _ = child.wait();
    });
    write_session(app, pid)?;

    let mut exited = false;
    for _ in 0..20 {
        thread::sleep(Duration::from_millis(200));
        if command_line(pid).is_none() {
            exited = true;
            break;
        }
        if tunnels_from_api()
            .iter()
            .any(|tunnel| tunnel.public_url.starts_with("https://"))
        {
            break;
        }
    }
    if exited {
        clear_session(app);
        let logs = tail_log(&log_file);
        let detail = logs
            .iter()
            .rev()
            .find(|line| line.contains("lvl=error") || line.contains("lvl=crit"))
            .map(|line| log_text(line))
            .unwrap_or_else(|| "ngrok 已退出，请查看下方记录。".into());
        return Err(detail);
    }
    collect(app)
}

fn signal(pid: u32, name: &str) -> bool {
    Command::new("/bin/kill")
        .args([name, &pid.to_string()])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

pub fn stop(app: &tauri::AppHandle) -> Result<NgrokStatus, String> {
    let _guard = lock();
    let pid = running_pid(app);
    if let Some(pid) = pid {
        if !command_line(pid).is_some_and(|line| is_ngrok_command(&line)) {
            clear_session(app);
            return collect(app);
        }
        if !signal(pid, "-TERM") {
            return Err("无法停止 ngrok。".into());
        }
        let started = SystemTime::now();
        while command_line(pid).is_some()
            && started.elapsed().unwrap_or_default() < Duration::from_secs(3)
        {
            thread::sleep(Duration::from_millis(100));
        }
        if command_line(pid).is_some() && !signal(pid, "-KILL") {
            return Err("ngrok 没有在限定时间内退出。".into());
        }
    }
    clear_session(app);
    if let Ok(path) = log_path(app) {
        let _ = fs::write(path, b"");
    }
    collect(app)
}

pub fn set_authtoken(token: &str) -> Result<(), String> {
    let _guard = lock();
    let token = token.trim();
    if !(16..=128).contains(&token.len())
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Err("authtoken 格式无效。请从申请页面复制完整 token。".into());
    }
    let binary = find_binary().ok_or("未找到 ngrok。可先安装：brew install --cask ngrok")?;
    let output = Command::new(&binary)
        .args(["config", "add-authtoken", token])
        .output()
        .map_err(|error| format!("无法写入 ngrok 配置：{error}"))?;
    if output.status.success() && local_account(&binary).0 {
        return Ok(());
    }
    let mut detail = String::from_utf8_lossy(&output.stderr).to_string();
    if detail.trim().is_empty() {
        detail = String::from_utf8_lossy(&output.stdout).to_string();
    }
    let detail = detail.replace(token, "<已隐藏>");
    Err(if detail.trim().is_empty() {
        "写入本机 ngrok 配置失败。".into()
    } else {
        format!("写入本机 ngrok 配置失败：{}", detail.trim())
    })
}

pub fn open(app: &tauri::AppHandle, url: &str) -> Result<(), String> {
    let _guard = lock();
    let parsed = Url::parse(url).map_err(|_| "地址格式无效。")?;
    let current = collect(app)?;
    let inspect = current.running
        && parsed.scheme() == "http"
        && matches!(parsed.host_str(), Some("127.0.0.1" | "localhost"))
        && parsed.port_or_known_default() == Some(INSPECT_PORT)
        && parsed.username().is_empty()
        && parsed.password().is_none();
    let tunnel = parsed.scheme() == "https"
        && current.tunnels.iter().any(|tunnel| {
            tunnel.public_url.trim_end_matches('/') == parsed.as_str().trim_end_matches('/')
                || tunnel.public_url.trim_end_matches('/')
                    == format!(
                        "{}://{}",
                        parsed.scheme(),
                        parsed.host_str().unwrap_or_default()
                    )
        });
    let authtoken_page = parsed.as_str().trim_end_matches('/') == AUTHTOKEN_PAGE
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.query().is_none()
        && parsed.fragment().is_none();
    if !inspect && !tunnel && !authtoken_page {
        return Err("只能打开当前隧道、本机请求检查页，或 ngrok 的 authtoken 申请页。".into());
    }
    let status = Command::new("/usr/bin/open")
        .arg(parsed.as_str())
        .status()
        .map_err(|error| error.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err("无法打开浏览器，请检查默认浏览器设置。".into())
    }
}

#[tauri::command]
pub async fn ngrok_status(app: tauri::AppHandle) -> Result<NgrokStatus, String> {
    tauri::async_runtime::spawn_blocking(move || status(&app))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn ngrok_save(app: tauri::AppHandle, settings: NgrokSettings) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || save(&app, settings))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn ngrok_start(
    app: tauri::AppHandle,
    settings: NgrokSettings,
) -> Result<NgrokStatus, String> {
    tauri::async_runtime::spawn_blocking(move || start(&app, settings))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn ngrok_stop(app: tauri::AppHandle) -> Result<NgrokStatus, String> {
    tauri::async_runtime::spawn_blocking(move || stop(&app))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn ngrok_set_authtoken(
    app: tauri::AppHandle,
    token: String,
) -> Result<NgrokStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        set_authtoken(&token)?;
        status(&app)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn open_ngrok_url(app: tauri::AppHandle, url: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || open(&app, &url))
        .await
        .map_err(|error| error.to_string())?
}

#[cfg(test)]
mod tests {
    use super::{
        config_has_authtoken, find_binary, normalize_host_header, normalize_public_url,
        normalize_target, redact_line, version_of,
    };
    use std::path::Path;

    #[test]
    fn accepts_local_targets() {
        assert_eq!(normalize_target("8080").unwrap(), "http://127.0.0.1:8080");
        assert_eq!(
            normalize_target("http://localhost:3002").unwrap(),
            "http://localhost:3002"
        );
        assert_eq!(
            normalize_target("https://127.0.0.1:8443/").unwrap(),
            "https://127.0.0.1:8443"
        );
    }

    #[test]
    fn rejects_remote_targets() {
        assert!(normalize_target("http://example.com:80").is_err());
        assert!(normalize_target("http://localhost:3002/app").is_err());
        assert!(normalize_target("http://user:pass@localhost:3002").is_err());
    }

    #[test]
    fn accepts_optional_public_url_and_host_header() {
        assert_eq!(normalize_public_url("  ").unwrap(), "");
        assert_eq!(
            normalize_public_url("https://Juicy.ngrok-free.dev/").unwrap(),
            "https://juicy.ngrok-free.dev"
        );
        assert!(normalize_public_url("http://juicy.ngrok-free.dev").is_err());
        assert_eq!(normalize_host_header("rewrite").unwrap(), "rewrite");
        assert_eq!(
            normalize_host_header("localhost:8080").unwrap(),
            "localhost:8080"
        );
    }

    #[test]
    fn finds_installed_ngrok() {
        let homebrew = Path::new("/opt/homebrew/bin/ngrok");
        if !homebrew.is_file() {
            return;
        }
        let found = find_binary().expect("本机已有 ngrok 时应该能检测到");
        assert!(found.ends_with("ngrok"));
        assert!(version_of(&found).is_some());
    }

    #[test]
    fn reads_authtoken_presence_without_keeping_the_value() {
        assert!(config_has_authtoken("agent:\n  authtoken: present\n"));
        assert!(!config_has_authtoken("agent:\n  authtoken:\n"));
        assert!(!config_has_authtoken("# authtoken: hidden\n"));
    }

    #[test]
    fn hides_tokens_in_logs() {
        assert_eq!(
            redact_line("msg=authtoken exampletokenvalue"),
            "msg=authtoken <已隐藏>"
        );
    }
}
