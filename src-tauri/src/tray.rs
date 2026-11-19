use crate::model::{Config, ProxyRoute};
use crate::ngrok::NgrokStatus;
use std::{sync::Mutex, thread, time::Duration};
use tauri::{
    menu::{IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager, Wry,
};

const TRAY_ID: &str = "landns";
const MAX_PROJECTS: usize = 24;

struct Line {
    text: String,
    action: Option<String>,
}

pub fn install(app: &AppHandle) -> Result<(), String> {
    let model = collect(app);
    let menu = render(app, &model)?;
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip(&model.tooltip)
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| on_menu(app, event.id.as_ref()));
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app).map_err(|error| error.to_string())?;
    if let Ok(mut last) = LAST.lock() {
        *last = model.signature;
    }
    let app = app.clone();
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(4));
        let _ = refresh(&app);
    });
    Ok(())
}

pub fn reveal(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let _ = window.unminimize();
    let _ = window.show();
    let _ = window.set_focus();
}

pub fn conceal(window: &tauri::Window) {
    let _ = window.hide();
}

pub fn conceal_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
    }
}

fn on_menu(app: &AppHandle, id: &str) {
    match id {
        "show" => reveal(app),
        "show-proxy" => open_page(app, "proxy"),
        "show-tunnel" => open_page(app, "ngrok"),
        id if id.starts_with("open-project:") => {
            let route_id = id.trim_start_matches("open-project:").to_string();
            let app = app.clone();
            thread::spawn(move || {
                let _ = open_project(&app, &route_id);
            });
        }
        id if id.starts_with("open-public:") => {
            let index = id.trim_start_matches("open-public:").parse::<usize>().ok();
            let app = app.clone();
            thread::spawn(move || {
                let Some(index) = index else { return };
                let Ok(status) = crate::ngrok::status(&app) else {
                    return;
                };
                let Some(url) = status
                    .tunnels
                    .get(index)
                    .map(|tunnel| tunnel.public_url.clone())
                else {
                    return;
                };
                let _ = crate::ngrok::open(&app, &url);
            });
        }
        _ => {}
    }
}

fn open_page(app: &AppHandle, page: &str) {
    reveal(app);
    let _ = app.emit("tray-open", page);
}

fn open_project(app: &AppHandle, route_id: &str) -> Result<(), String> {
    let config = crate::platform::load_config(app)?;
    let route = config
        .routes
        .iter()
        .find(|route| route.id == route_id)
        .ok_or("找不到这个本地项目。")?;
    if !route.enabled {
        return Err("这个本地项目已停用。".into());
    }
    let url = project_url(route, &config.domain).ok_or("这个项目没有可打开的网址。")?;
    crate::open_configured_http(app, &url)
}

static LAST: Mutex<String> = Mutex::new(String::new());

fn refresh(app: &AppHandle) -> Result<(), String> {
    let model = collect(app);
    {
        let mut last = LAST.lock().map_err(|_| "菜单栏状态暂不可用。")?;
        if *last == model.signature {
            return Ok(());
        }
        *last = model.signature.clone();
    }
    let menu = render(app, &model)?;
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return Ok(());
    };
    tray.set_menu(Some(menu))
        .map_err(|error| error.to_string())?;
    let _ = tray.set_tooltip(Some(model.tooltip));
    Ok(())
}

struct TrayModel {
    signature: String,
    tooltip: String,
    projects_title: String,
    projects: Vec<Line>,
    tunnel: Vec<Line>,
}

fn collect(app: &AppHandle) -> TrayModel {
    let snapshot = crate::platform::snapshot(app);
    let ngrok = crate::ngrok::status(app);
    let (projects_title, projects) = match &snapshot {
        Ok(snapshot) => (
            project_title(snapshot.config.routes.len(), snapshot.service.running),
            project_lines(&snapshot.config, snapshot.service.running),
        ),
        Err(error) => (
            "本地项目".into(),
            vec![Line {
                text: error.clone(),
                action: None,
            }],
        ),
    };
    let (tunnel, tooltip) = match &ngrok {
        Ok(status) => (tunnel_lines(status), tooltip(status)),
        Err(error) => (
            vec![Line {
                text: error.clone(),
                action: None,
            }],
            "LanDNS".into(),
        ),
    };
    let signature = signature_of(&projects_title, &projects, &tunnel, &tooltip);
    TrayModel {
        signature,
        tooltip,
        projects_title,
        projects,
        tunnel,
    }
}

fn tooltip(status: &NgrokStatus) -> String {
    status
        .tunnels
        .iter()
        .find(|tunnel| !tunnel.public_url.is_empty())
        .map(|tunnel| {
            format!(
                "LanDNS · {}",
                tunnel.public_url.trim_start_matches("https://")
            )
        })
        .unwrap_or_else(|| "LanDNS".into())
}

fn signature_of(title: &str, projects: &[Line], tunnel: &[Line], tooltip: &str) -> String {
    let mut parts = vec![title.to_string(), tooltip.to_string()];
    for lines in [projects, tunnel] {
        for line in lines {
            parts.push(line.action.clone().unwrap_or_default());
            parts.push(line.text.clone());
        }
    }
    parts.join("\n")
}

fn project_title(count: usize, service_running: bool) -> String {
    if service_running {
        format!("本地项目 ({count})")
    } else {
        format!("本地项目 ({count}) · 未启动")
    }
}

fn project_lines(config: &Config, service_running: bool) -> Vec<Line> {
    let mut lines = Vec::new();
    if config.routes.is_empty() {
        lines.push(Line {
            text: "还没有本地项目".into(),
            action: None,
        });
    }
    let hidden = config.routes.len().saturating_sub(MAX_PROJECTS);
    for route in config.routes.iter().take(MAX_PROJECTS) {
        let host = project_host(route, &config.domain);
        let mut text = format!("{} · {host} → localhost:{}", route_name(route), route.port);
        let url = project_url(route, &config.domain);
        let clickable = route.enabled && service_running && config.proxy_enabled && url.is_some();
        if !route.enabled {
            text.push_str(" · 已停用");
        }
        lines.push(Line {
            text,
            action: clickable.then(|| format!("open-project:{}", route.id)),
        });
    }
    if hidden > 0 {
        lines.push(Line {
            text: format!("还有 {hidden} 个项目"),
            action: None,
        });
    }
    lines.push(Line {
        text: "在窗口中查看".into(),
        action: Some("show-proxy".into()),
    });
    lines
}

fn tunnel_lines(status: &NgrokStatus) -> Vec<Line> {
    let mut lines = Vec::new();
    let visible: Vec<_> = status
        .tunnels
        .iter()
        .enumerate()
        .filter(|(_, tunnel)| !tunnel.public_url.is_empty())
        .collect();
    if visible.is_empty() {
        lines.push(Line {
            text: if status.running {
                "正在连接".into()
            } else {
                "未启动".into()
            },
            action: None,
        });
    }
    for (index, tunnel) in visible {
        let text = if tunnel.addr.is_empty() {
            tunnel.public_url.clone()
        } else {
            format!("{} → {}", tunnel.public_url, tunnel.addr)
        };
        lines.push(Line {
            text,
            action: Some(format!("open-public:{index}")),
        });
    }
    lines.push(Line {
        text: "在窗口中查看".into(),
        action: Some("show-tunnel".into()),
    });
    lines
}

fn route_name(route: &ProxyRoute) -> String {
    let name = route.name.trim();
    if name.is_empty() {
        route.subdomain.clone()
    } else {
        name.to_string()
    }
}

fn project_host(route: &ProxyRoute, domain: &str) -> String {
    if let Some(host) = route.hostname.as_deref().filter(|host| !host.is_empty()) {
        return host.to_string();
    }
    if route.subdomain == "*" {
        return format!("*.{domain}");
    }
    route.effective_hostname(domain)
}

fn project_url(route: &ProxyRoute, domain: &str) -> Option<String> {
    if route.subdomain == "*" && route.hostname.as_deref().unwrap_or("").is_empty() {
        return None;
    }
    let host = project_host(route, domain);
    if host.contains('*') || host.is_empty() {
        return None;
    }
    Some(format!("http://{host}"))
}

fn render(app: &AppHandle, model: &TrayModel) -> Result<Menu<Wry>, String> {
    let menu = Menu::with_id(app, "landns-tray").map_err(|error| error.to_string())?;
    let show = item(app, "show", "打开 LanDNS", true)?;
    let projects = submenu(app, "projects", &model.projects_title, &model.projects)?;
    let tunnel = submenu(app, "tunnel", "公网地址", &model.tunnel)?;
    let separator = PredefinedMenuItem::separator(app).map_err(|error| error.to_string())?;
    menu.append_items(&[
        &show as &dyn IsMenuItem<Wry>,
        &separator,
        &projects,
        &tunnel,
    ])
    .map_err(|error| error.to_string())?;
    Ok(menu)
}

fn submenu(app: &AppHandle, id: &str, title: &str, lines: &[Line]) -> Result<Submenu<Wry>, String> {
    let submenu =
        Submenu::with_id(app, id, menu_label(title), true).map_err(|error| error.to_string())?;
    let mut owned = Vec::with_capacity(lines.len());
    for (index, line) in lines.iter().enumerate() {
        let id = line
            .action
            .clone()
            .unwrap_or_else(|| format!("{id}-info-{index}"));
        owned.push(item(app, &id, &line.text, line.action.is_some())?);
    }
    let refs: Vec<&dyn IsMenuItem<Wry>> = owned
        .iter()
        .map(|entry| entry as &dyn IsMenuItem<Wry>)
        .collect();
    submenu
        .append_items(&refs)
        .map_err(|error| error.to_string())?;
    Ok(submenu)
}

fn item(app: &AppHandle, id: &str, text: &str, enabled: bool) -> Result<MenuItem<Wry>, String> {
    MenuItem::with_id(app, id, menu_label(text), enabled, None::<&str>)
        .map_err(|error| error.to_string())
}

fn menu_label(text: &str) -> String {
    let text = text.replace(['\n', '\r', '\t'], " ").replace('&', "&&");
    if text.chars().count() <= 96 {
        text
    } else {
        let mut truncated: String = text.chars().take(95).collect();
        truncated.push('…');
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::{menu_label, project_host, project_lines, project_url, tunnel_lines};
    use crate::model::{Config, ProxyRoute};
    use crate::ngrok::{NgrokSettings, NgrokStatus, NgrokTunnel};

    fn route(name: &str, subdomain: &str, port: u16, enabled: bool) -> ProxyRoute {
        ProxyRoute {
            id: format!("id-{subdomain}"),
            name: name.into(),
            subdomain: subdomain.into(),
            hostname: None,
            port,
            enabled,
        }
    }

    fn config(routes: Vec<ProxyRoute>) -> Config {
        Config {
            domain: "dev.test".into(),
            upstream_dns: "1.1.1.1".into(),
            interface_name: String::new(),
            proxy_enabled: true,
            ttl: 15,
            routes,
        }
    }

    fn status() -> NgrokStatus {
        NgrokStatus {
            installed: true,
            binary: Some("/opt/homebrew/bin/ngrok".into()),
            version: Some("3.39.13".into()),
            authtoken_configured: true,
            config_path: Some("/Users/example/Library/Application Support/ngrok/ngrok.yml".into()),
            running: true,
            owned: true,
            pid: Some(1),
            tunnels: vec![NgrokTunnel {
                public_url: "https://example.ngrok-free.dev".into(),
                proto: "https".into(),
                addr: "http://localhost:8080".into(),
            }],
            inspect_url: None,
            logs: Vec::new(),
            settings: NgrokSettings {
                target: "http://localhost:8080".into(),
                public_url: String::new(),
                host_header: String::new(),
                traffic_policy_file: String::new(),
            },
            message: None,
        }
    }

    #[test]
    fn menu_labels_escape_shortcuts_and_keep_a_single_line() {
        assert_eq!(menu_label("A & B"), "A && B");
        assert_eq!(menu_label("第一行\n第二行"), "第一行 第二行");
    }

    #[test]
    fn project_menu_lists_configured_routes() {
        let config = config(vec![
            route("控制台", "app", 3002, true),
            route("旧项目", "old", 3003, false),
        ]);
        let lines = project_lines(&config, true);
        assert_eq!(lines[0].text, "控制台 · app.dev.test → localhost:3002");
        assert_eq!(lines[0].action.as_deref(), Some("open-project:id-app"));
        assert!(lines[1].text.contains("已停用"));
        assert!(lines[1].action.is_none());
        assert!(lines.iter().all(|line| line.text != "服务运行中"));
        assert_eq!(
            project_host(&config.routes[0], &config.domain),
            "app.dev.test"
        );
        assert_eq!(
            project_url(&config.routes[0], &config.domain).as_deref(),
            Some("http://app.dev.test")
        );
    }

    #[test]
    fn public_address_is_one_line_per_tunnel() {
        let status = status();
        let tunnel = tunnel_lines(&status);
        assert_eq!(
            tunnel[0].text,
            "https://example.ngrok-free.dev → http://localhost:8080"
        );
        assert_eq!(tunnel[0].action.as_deref(), Some("open-public:0"));
        assert_eq!(tunnel.len(), 2);
        assert!(tunnel.iter().all(|line| !line.text.contains("tokenvalue")));
    }
}
