use crate::model::{
    now, Config, LogEntry, NetworkInfo, ServiceStatus, Snapshot, LOG_PATH, PLIST_PATH,
    SERVICE_CONFIG, SERVICE_DIR, SERVICE_LABEL, STATUS_PATH,
};
use if_addrs::IfAddr;
use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use tauri::Manager;

const RESOLVER_MARKER: &str = "# Managed by DNS X (app.dnsx.service)";

pub fn interfaces() -> Result<Vec<NetworkInfo>, String> {
    let mut networks = Vec::new();
    for interface in if_addrs::get_if_addrs().map_err(|e| format!("无法读取网络接口：{e}"))?
    {
        if ["lo", "utun", "tun", "tap", "ppp"]
            .iter()
            .any(|prefix| interface.name.starts_with(prefix))
        {
            continue;
        }
        if let IfAddr::V4(address) = interface.addr {
            if address.ip.is_private() || address.ip.is_link_local() {
                networks.push(NetworkInfo {
                    interface_name: interface.name,
                    ip: address.ip.to_string(),
                    netmask: address.netmask.to_string(),
                });
            }
        }
    }
    networks.sort_by(|a, b| {
        a.interface_name
            .cmp(&b.interface_name)
            .then(a.ip.cmp(&b.ip))
    });
    networks.dedup_by(|a, b| a.interface_name == b.interface_name && a.ip == b.ip);
    Ok(networks)
}

pub fn detect_network(config: &Config) -> Result<NetworkInfo, String> {
    let mut networks = interfaces()?;
    // A VPN may own the default route; prefer a physical LAN over virtual bridges.
    networks.sort_by_key(|network| {
        let interface_priority = if network.interface_name.starts_with("en") {
            0
        } else if network.interface_name.starts_with("bridge") {
            1
        } else {
            2
        };
        (network.ip.starts_with("169.254."), interface_priority)
    });
    if !config.interface_name.is_empty() {
        return networks
            .into_iter()
            .find(|n| n.interface_name == config.interface_name)
            .ok_or_else(|| format!("接口 {} 当前没有局域网 IPv4 地址", config.interface_name));
    }
    let preferred = Command::new("/sbin/route")
        .args(["-n", "get", "default"])
        .output()
        .ok()
        .and_then(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .find_map(|line| {
                    line.trim()
                        .strip_prefix("interface:")
                        .map(str::trim)
                        .map(str::to_owned)
                })
        });
    if let Some(network) =
        preferred.and_then(|name| networks.iter().find(|n| n.interface_name == name))
    {
        return Ok(NetworkInfo {
            interface_name: network.interface_name.clone(),
            ip: network.ip.clone(),
            netmask: network.netmask.clone(),
        });
    }
    networks
        .into_iter()
        .next()
        .ok_or_else(|| "未找到局域网 IPv4 地址，请连接 Wi-Fi 或以太网".to_owned())
}

fn config_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_config_dir()
        .map_err(|e| e.to_string())?
        .join("config.json"))
}

pub fn load_config(app: &tauri::AppHandle) -> Result<Config, String> {
    let local = config_path(app)?;
    let service = PathBuf::from(SERVICE_CONFIG);
    let paths = if service_loaded() {
        [service, local]
    } else {
        [local, service]
    };
    for path in paths {
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("无法读取配置：{error}")),
        };
        let config: Config =
            serde_json::from_slice(&bytes).map_err(|e| format!("配置文件格式错误：{e}"))?;
        config.validate()?;
        return Ok(config);
    }
    Ok(Config::default())
}

pub fn save_config(app: &tauri::AppHandle, config: Config) -> Result<(), String> {
    config.validate()?;
    let path = config_path(app)?;
    let directory = path.parent().ok_or("配置目录无效")?;
    fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut file = tempfile::NamedTempFile::new_in(directory).map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec_pretty(&config).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    file.as_file().sync_all().map_err(|e| e.to_string())?;
    file.persist(path)
        .map_err(|e| format!("无法保存配置：{e}"))?;
    Ok(())
}

fn launch_state() -> Option<(bool, Option<u32>)> {
    let output = Command::new("/bin/launchctl")
        .args(["print", &format!("system/{SERVICE_LABEL}")])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let running = text.lines().any(|line| line.trim() == "state = running");
    let pid = text.lines().find_map(|line| {
        line.trim()
            .strip_prefix("pid = ")
            .and_then(|pid| pid.parse().ok())
    });
    Some((running, pid))
}

pub fn service_loaded() -> bool {
    launch_state().is_some()
}

fn logs() -> Vec<LogEntry> {
    let Ok(mut file) = fs::File::open(LOG_PATH) else {
        return Vec::new();
    };
    let length = file.metadata().map(|m| m.len()).unwrap_or(0);
    let offset = length.saturating_sub(64 * 1024);
    if file.seek(SeekFrom::Start(offset)).is_err() {
        return Vec::new();
    }
    let mut bytes = Vec::new();
    if file.take(64 * 1024).read_to_end(&mut bytes).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut entries: Vec<LogEntry> = text
        .lines()
        .skip(usize::from(offset > 0))
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    if entries.len() > 100 {
        entries.drain(..entries.len() - 100);
    }
    entries.reverse();
    entries
}

pub fn snapshot(app: &tauri::AppHandle) -> Result<Snapshot, String> {
    let config = load_config(app)?;
    let network = detect_network(&config).ok();
    let mut service: ServiceStatus = fs::read(STATUS_PATH)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let (process_running, pid) = launch_state().unwrap_or((false, None));
    let fresh = service
        .updated_at
        .is_some_and(|updated| now().saturating_sub(updated) < 15);
    if !process_running || !fresh || service.pid != pid {
        service.running = false;
        if process_running && !fresh {
            service.last_error = Some("服务尚未就绪或状态更新已中断".to_owned());
        }
        service.pid = pid;
    }
    Ok(Snapshot {
        config,
        network,
        interfaces: interfaces()?,
        service,
        logs: logs(),
        installed: Path::new(SERVICE_CONFIG).is_file(),
        service_loaded: service_loaded(),
    })
}

fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn administrator(script: &str) -> Result<(), String> {
    if !cfg!(target_os = "macos") {
        return Err("后台服务目前仅支持 macOS".to_owned());
    }
    // Pass the script over stdin; neither shell interpolation nor process arguments carry its contents.
    let escaped = script.replace('\\', "\\\\").replace('"', "\\\"");
    let apple_script = format!("do shell script \"{escaped}\" with administrator privileges with prompt \"LanDNS 需要安装或管理本机 DNS 后台服务。\"\n");
    let mut child = Command::new("/usr/bin/osascript")
        .current_dir("/")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    child
        .stdin
        .take()
        .ok_or("无法打开系统授权输入")?
        .write_all(apple_script.as_bytes())
        .map_err(|e| e.to_string())?;
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    let error = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if error.contains("-128") {
        return Err("已取消管理员授权，服务未更改".to_owned());
    }
    Err(format!("系统服务操作失败：{error}"))
}

fn preamble() -> String {
    format!(
        r#"set -eu
export PATH=/usr/bin:/bin:/usr/sbin:/sbin
umask 022
service_dir={service_dir}
plist={plist}
label={label}
marker={marker}
fail() {{ echo "$1" >&2; exit 1; }}
for path in '/Library/Application Support' /Library/LaunchDaemons "$service_dir" "$plist" /etc/resolver; do
  [ ! -L "$path" ] || fail "拒绝操作符号链接：$path"
  if [ -e "$path" ]; then
    [ "$(/usr/bin/stat -f %u "$path")" = 0 ] || fail "系统服务目录不是 root 所有：$path"
    [ -z "$(/usr/bin/find "$path" -prune -perm +0022 -print)" ] || fail "系统服务路径允许普通用户写入：$path"
  fi
done
[ ! -e "$service_dir" ] || [ -d "$service_dir" ] || fail "后台服务目录被同名文件占用"
[ ! -e "$plist" ] || [ -f "$plist" ] || fail "LaunchDaemon 路径被同名目录占用"
remove_resolvers() {{
  [ -d /etc/resolver ] || return 0
  for resolver in /etc/resolver/*; do
    [ -f "$resolver" ] && [ ! -L "$resolver" ] || continue
    if [ "$(/usr/bin/head -n 1 "$resolver")" = "$marker" ]; then /bin/rm -f "$resolver"; fi
  done
}}
stop_loaded() {{
  if /bin/launchctl print "system/$label" >/dev/null 2>&1; then
    old_pid=$(/bin/launchctl print "system/$label" | /usr/bin/awk '$1 == "pid" && $2 == "=" {{print $3; exit}}')
    /bin/launchctl bootout "system/$label"
    count=0
    while /bin/launchctl print "system/$label" >/dev/null 2>&1 || {{ [ -n "$old_pid" ] && /bin/kill -0 "$old_pid" 2>/dev/null; }}; do
      count=$((count + 1)); [ "$count" -lt 10 ] || fail "后台进程仍在退出，请稍后重试"
      /bin/sleep 1
    done
  fi
}}
"#,
        service_dir = shell_quote(SERVICE_DIR),
        plist = shell_quote(PLIST_PATH),
        label = shell_quote(SERVICE_LABEL),
        marker = shell_quote(RESOLVER_MARKER)
    )
}

pub fn install_service(app: &tauri::AppHandle, config: Config) -> Result<(), String> {
    config.validate()?;
    if interfaces()?
        .iter()
        .any(|network| network.ip == config.upstream_dns)
    {
        return Err("上游 DNS 不能使用本机的网络地址，这会造成循环转发".to_owned());
    }
    detect_network(&config)?;
    let executable = std::env::current_exe()
        .and_then(fs::canonicalize)
        .map_err(|e| format!("无法读取当前程序路径：{e}"))?;
    let temporary = tempfile::Builder::new()
        .prefix("dns-x-install-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    let staged_executable = temporary.path().join("dns-x-service");
    // Snapshot the executable before authorization so rebuilding cannot change the install source.
    fs::copy(&executable, &staged_executable).map_err(|e| format!("无法准备后台服务程序：{e}"))?;
    let staged_config = temporary.path().join("config.json");
    let staged_plist = temporary.path().join("service.plist");
    fs::write(
        &staged_config,
        serde_json::to_vec_pretty(&config).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let installed_executable = format!("{SERVICE_DIR}/dns-x-service");
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{label}</string>
<key>ProgramArguments</key><array><string>{executable}</string><string>--service</string></array>
<key>RunAtLoad</key><true/><key>KeepAlive</key><true/>
<key>ThrottleInterval</key><integer>10</integer>
<key>ProcessType</key><string>Background</string>
<key>WorkingDirectory</key><string>{directory}</string>
<key>Umask</key><integer>18</integer>
<key>EnvironmentVariables</key><dict><key>PATH</key><string>/usr/bin:/bin:/usr/sbin:/sbin</string></dict>
</dict></plist>"#,
        label = xml_escape(SERVICE_LABEL),
        executable = xml_escape(&installed_executable),
        directory = xml_escape(SERVICE_DIR)
    );
    fs::write(&staged_plist, plist).map_err(|e| e.to_string())?;
    let mut resolver_domains = vec![config.domain.as_str()];
    for route in config.routes.iter().filter(|route| route.enabled) {
        if let Some(hostname) = route.hostname.as_deref() {
            if hostname != config.domain && !hostname.ends_with(&format!(".{}", config.domain)) {
                resolver_domains.push(hostname);
            }
        }
    }
    resolver_domains.sort_unstable();
    resolver_domains.dedup();
    let resolver_domains = resolver_domains
        .into_iter()
        .map(shell_quote)
        .collect::<Vec<_>>()
        .join(" ");
    let script = format!(
        r#"{preamble}
source_executable={source_executable}
source_config={source_config}
source_plist={source_plist}
set -- {resolver_domains}
for domain in "$@"; do
  resolver="/etc/resolver/$domain"
  [ ! -L "$resolver" ] || fail "已有 $domain 的 DNS resolver 是符号链接，未作修改"
  if [ -e "$resolver" ] && [ "$(/usr/bin/head -n 1 "$resolver")" != "$marker" ]; then
    fail "已有 $domain 的 DNS resolver 配置，未覆盖，请先处理冲突"
  fi
done
work=$(/usr/bin/mktemp -d '/Library/Application Support/.dns-x-install.XXXXXXXX')
/bin/chmod 700 "$work"
swapped=0
committed=0
was_loaded=0
shutdown_started=0
/bin/launchctl print "system/$label" >/dev/null 2>&1 && was_loaded=1
rollback() {{
  code=$?
  trap - EXIT
  set +e
  if [ "$committed" = 0 ] && [ "$swapped" = 1 ]; then
    /bin/launchctl bootout "system/$label" >/dev/null 2>&1 || true
    /bin/rm -rf "$service_dir"
    [ ! -d "$work/previous" ] || /bin/mv "$work/previous" "$service_dir"
    /bin/rm -f "$plist"
    [ ! -f "$work/previous.plist" ] || /bin/mv "$work/previous.plist" "$plist"
    remove_resolvers
    for saved in "$work/resolvers/"*; do
      [ ! -f "$saved" ] || /bin/cp "$saved" /etc/resolver/
    done
  fi
  if [ "$committed" = 0 ] && [ "$shutdown_started" = 1 ] && [ "$was_loaded" = 1 ] && [ -f "$plist" ]; then /bin/launchctl bootstrap system "$plist" >/dev/null 2>&1 || true; fi
  if [ "$committed" = 0 ] && [ "$swapped" = 1 ] && [ "$was_loaded" = 0 ]; then /bin/launchctl disable "system/$label" >/dev/null 2>&1 || true; fi
  /bin/rm -rf "$work"
  exit "$code"
}}
trap rollback EXIT
/bin/mkdir "$work/new" "$work/resolvers"
/usr/bin/install -o root -g wheel -m 755 "$source_executable" "$work/new/dns-x-service"
/bin/cp "$source_config" "$work/new/config.json"
/bin/cp "$source_plist" "$work/new.plist"
/usr/bin/plutil -lint -s "$work/new.plist"
/bin/chmod -RN "$work/new"
/usr/sbin/chown -R root:wheel "$work/new"
/bin/chmod -R go-w "$work/new"
/bin/chmod 755 "$work/new"
/bin/chmod 644 "$work/new/config.json" "$work/new.plist"
if [ -d /etc/resolver ]; then
  for existing in /etc/resolver/*; do
    [ -f "$existing" ] && [ ! -L "$existing" ] || continue
    if [ "$(/usr/bin/head -n 1 "$existing")" = "$marker" ]; then /bin/cp "$existing" "$work/resolvers/"; fi
  done
fi
if [ -f "$plist" ]; then /bin/cp "$plist" "$work/previous.plist"; fi
shutdown_started=1
stop_loaded
if [ -d "$service_dir" ]; then /bin/mv "$service_dir" "$work/previous"; fi
swapped=1
/bin/mv "$work/new" "$service_dir"
/usr/bin/install -o root -g wheel -m 644 "$work/new.plist" "$plist"
/bin/launchctl enable "system/$label"
/bin/launchctl bootstrap system "$plist"
ready=0
for attempt in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do
  if [ -f "$service_dir/status.json" ] && [ "$(/usr/bin/plutil -extract running raw -o - "$service_dir/status.json" 2>/dev/null || true)" = true ]; then ready=1; break; fi
  /bin/sleep 1
done
if [ "$ready" != 1 ]; then
  detail=$(/usr/bin/plutil -extract lastError raw -o - "$service_dir/status.json" 2>/dev/null || true)
  fail "后台服务未能启动，已尝试恢复原配置。$detail"
fi
/bin/mkdir -p /etc/resolver
printf '%s\nnameserver 127.0.0.1\nport 53\n' "$marker" > "$work/resolver"
for domain in "$@"; do
  /usr/bin/install -o root -g wheel -m 644 "$work/resolver" "/etc/resolver/$domain"
done
for old in /etc/resolver/*; do
  [ -f "$old" ] && [ ! -L "$old" ] || continue
  keep=0
  for domain in "$@"; do
    [ "$old" != "/etc/resolver/$domain" ] || keep=1
  done
  if [ "$keep" = 0 ] && [ "$(/usr/bin/head -n 1 "$old")" = "$marker" ]; then /bin/rm -f "$old"; fi
done
committed=1
"#,
        preamble = preamble(),
        source_executable = shell_quote(&staged_executable.to_string_lossy()),
        source_config = shell_quote(&staged_config.to_string_lossy()),
        source_plist = shell_quote(&staged_plist.to_string_lossy()),
        resolver_domains = resolver_domains
    );
    administrator(&script)?;
    save_config(app, config).map_err(|e| format!("服务已启动，但本地配置保存失败：{e}"))
}

pub fn stop_service() -> Result<(), String> {
    administrator(&format!("{}\nstop_loaded\n/bin/launchctl disable \"system/$label\"\nremove_resolvers\n/bin/rm -f \"$service_dir/status.json\"\n", preamble()))
}

pub fn uninstall_service() -> Result<(), String> {
    administrator(&format!("{}\nstop_loaded\nremove_resolvers\n/bin/rm -f \"$plist\"\n/bin/rm -rf \"$service_dir\"\n/bin/launchctl enable \"system/$label\"\n", preamble()))
}
