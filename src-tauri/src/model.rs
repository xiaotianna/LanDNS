use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    net::Ipv4Addr,
    time::{SystemTime, UNIX_EPOCH},
};

pub const SERVICE_DIR: &str = "/Library/Application Support/DNS X";
pub const SERVICE_CONFIG: &str = "/Library/Application Support/DNS X/config.json";
pub const STATUS_PATH: &str = "/Library/Application Support/DNS X/status.json";
pub const LOG_PATH: &str = "/Library/Application Support/DNS X/service.jsonl";
pub const PLIST_PATH: &str = "/Library/LaunchDaemons/app.dnsx.service.plist";
pub const SERVICE_LABEL: &str = "app.dnsx.service";

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProxyRoute {
    pub id: String,
    pub name: String,
    pub subdomain: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    pub port: u16,
    pub enabled: bool,
}

impl ProxyRoute {
    pub fn effective_hostname(&self, domain: &str) -> String {
        self.hostname
            .clone()
            .unwrap_or_else(|| format!("{}.{}", self.subdomain, domain))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    pub domain: String,
    pub upstream_dns: String,
    pub interface_name: String,
    pub proxy_enabled: bool,
    pub ttl: u32,
    pub routes: Vec<ProxyRoute>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            domain: "dev.home.arpa".into(),
            upstream_dns: "1.1.1.1".into(),
            interface_name: String::new(),
            proxy_enabled: false,
            ttl: 15,
            routes: vec![],
        }
    }
}

pub fn valid_dns_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && value.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

impl Config {
    pub fn bonjour_hostnames(&self) -> Result<Vec<String>, String> {
        let local_suffix = self.domain == "local" || self.domain.ends_with(".local");
        let mut names = Vec::new();
        if self.domain.ends_with(".local") {
            names.push(self.domain.clone());
        }
        for route in self.routes.iter().filter(|route| route.enabled) {
            if route.subdomain == "*" {
                if local_suffix {
                    return Err(".local 使用 Bonjour，不能使用通配项目。请填写具体项目域名，或将默认后缀改为 dev.home.arpa。".into());
                }
                continue;
            }
            let hostname = route.effective_hostname(&self.domain);
            if hostname.ends_with(".local") {
                names.push(hostname);
            }
        }
        names.sort();
        names.dedup();
        Ok(names)
    }

    pub fn resolves_hostname(&self, name: &str) -> bool {
        name == self.domain
            || name.ends_with(&format!(".{}", self.domain))
            || self
                .routes
                .iter()
                .any(|route| route.enabled && route.hostname.as_deref() == Some(name))
    }

    pub fn validate(&self) -> Result<(), String> {
        if !valid_dns_name(&self.domain)
            || url::Host::parse(&format!("project.{}", self.domain)).is_err()
        {
            return Err(
                "网址后缀格式无效，例如 dev.test；每段仅支持小写字母、数字和连字符，不能以连字符开头或结尾。"
                    .into(),
            );
        }
        let upstream: Ipv4Addr = self
            .upstream_dns
            .parse()
            .map_err(|_| "上游 DNS 必须是 IPv4 地址。")?;
        if upstream.is_loopback()
            || upstream.is_unspecified()
            || upstream.is_multicast()
            || upstream.is_broadcast()
            || upstream.is_link_local()
            || upstream.octets()[0] == 0
            || upstream.octets()[0] >= 240
        {
            return Err("上游 DNS 必须是可访问的单播地址，不能指向本机回环地址。".into());
        }
        if self.interface_name.len() > 20
            || !self
                .interface_name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric())
        {
            return Err("网络接口名称无效。请从接口列表选择。".into());
        }
        if !(5..=300).contains(&self.ttl) {
            return Err("DNS 缓存时间必须在 5 至 300 秒之间。".into());
        }
        if self.routes.len() > 100 {
            return Err("最多支持 100 条代理规则。".into());
        }
        let mut names = HashSet::new();
        let mut ids = HashSet::new();
        for route in &self.routes {
            if route.id.is_empty()
                || route.id.len() > 64
                || !route
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                || !ids.insert(&route.id)
            {
                return Err("代理规则标识无效或重复，请重新创建该规则。".into());
            }
            if route.name.trim().is_empty()
                || route.name.chars().count() > 80
                || route.name.chars().any(char::is_control)
            {
                return Err("规则名称应为 1 至 80 个可见字符。".into());
            }
            if route.subdomain != "*"
                && (!valid_dns_name(&route.subdomain)
                    || (route.hostname.is_none()
                        && route.subdomain.len() + self.domain.len() + 1 > 253))
            {
                return Err("子域名格式无效。填写 app、api 或 *，无需填写完整域名。".into());
            }
            if let Some(hostname) = &route.hostname {
                if route.subdomain == "*"
                    || !valid_dns_name(hostname)
                    || !matches!(url::Host::parse(hostname), Ok(url::Host::Domain(_)))
                {
                    return Err(
                        "访问域名格式无效。请填写域名而非 IP 或完整网址；默认项目不能设置独立域名。"
                            .into(),
                    );
                }
            }
            if !names.insert(route.effective_hostname(&self.domain)) {
                return Err("每个访问域名只能有一条代理规则。".into());
            }
            if route.port == 0 || route.port == 53 || route.port == 80 {
                return Err(
                    "目标端口须在 1 至 65535 之间，且不能使用本应用占用的 53 或 80。".into(),
                );
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkInfo {
    pub interface_name: String,
    pub ip: String,
    pub netmask: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceStatus {
    pub running: bool,
    #[serde(default)]
    pub version: Option<String>,
    pub pid: Option<u32>,
    pub ip: Option<String>,
    pub interface_name: Option<String>,
    pub started_at: Option<u64>,
    pub updated_at: Option<u64>,
    pub dns_queries: u64,
    pub proxy_requests: u64,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    pub timestamp: u64,
    pub level: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub config: Config,
    pub network: Option<NetworkInfo>,
    pub interfaces: Vec<NetworkInfo>,
    pub service: ServiceStatus,
    pub logs: Vec<LogEntry>,
    pub installed: bool,
    pub service_loaded: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticResult {
    pub hostname: String,
    pub expected_ip: Option<String>,
    pub resolved_ip: Option<String>,
    pub elapsed_ms: u64,
    pub ok: bool,
    pub message: String,
}

#[cfg(test)]
mod checks {
    use super::*;

    #[test]
    fn selects_enabled_bonjour_names_and_preserves_legacy_config_loading() {
        let mut config = Config::default();
        config.routes = serde_json::from_value(serde_json::json!([
            {"id":"one","name":"One","subdomain":"sub-sso","hostname":"sub-sso.local","port":3002,"enabled":true},
            {"id":"two","name":"Two","subdomain":"disabled","hostname":"disabled.local","port":3002,"enabled":false},
            {"id":"three","name":"Three","subdomain":"app","port":3002,"enabled":true}
        ])).unwrap();
        assert_eq!(config.bonjour_hostnames().unwrap(), ["sub-sso.local"]);
        config.domain = "dev.local".into();
        assert_eq!(
            config.bonjour_hostnames().unwrap(),
            ["app.dev.local", "dev.local", "sub-sso.local"]
        );
        config.routes[2].subdomain = "*".into();
        assert!(config.validate().is_ok());
        assert!(config.bonjour_hostnames().is_err());
        config.domain = "dev.home.arpa".into();
        assert_eq!(config.bonjour_hostnames().unwrap(), ["sub-sso.local"]);
    }

    #[test]
    fn reads_service_status_without_version() {
        let mut status: ServiceStatus = serde_json::from_str(
            r#"{"running":true,"dnsQueries":7,"proxyRequests":3}"#,
        )
        .unwrap();
        assert_eq!(status.version, None);
        assert!(status.running);
        assert_eq!(status.dns_queries, 7);
        assert!(serde_json::to_value(&status).unwrap()["version"].is_null());
        status.version = Some(env!("CARGO_PKG_VERSION").into());
        let current: ServiceStatus =
            serde_json::from_value(serde_json::to_value(&status).unwrap()).unwrap();
        assert_eq!(current.version.as_deref(), Some(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn custom_hostnames_preserve_legacy_routes_and_validate_effective_names() {
        let legacy: ProxyRoute = serde_json::from_str(
            r#"{"id":"legacy","name":"App","subdomain":"app","port":3000,"enabled":true}"#,
        )
        .unwrap();
        assert_eq!(legacy.hostname, None);
        assert!(serde_json::to_value(&legacy)
            .unwrap()
            .get("hostname")
            .is_none());
        let mut config = Config::default();
        let mut custom = legacy.clone();
        custom.id = "custom".into();
        custom.hostname = Some("example.com".into());
        config.routes = vec![legacy, custom];
        assert!(config.validate().is_ok());
        assert_eq!(
            config.routes[0].effective_hostname(&config.domain),
            "app.dev.home.arpa"
        );
        assert_eq!(
            config.routes[1].effective_hostname(&config.domain),
            "example.com"
        );
        assert!(config.resolves_hostname("example.com"));
        for name in ["com", "sub.example.com", "otherexample.com"] {
            assert!(!config.resolves_hostname(name), "{name}");
        }
        config.routes[1].enabled = false;
        assert!(!config.resolves_hostname("example.com"));
        config.routes[1].enabled = true;
        for name in [
            "",
            "127.0.0.1",
            "2130706433",
            "[::1]",
            "http://example.com",
            "example.com/path",
            "example.com:80",
            "Example.com",
            "example.com.",
            "app.dev.home.arpa",
        ] {
            config.routes[1].hostname = Some(name.into());
            assert!(config.validate().is_err(), "{name}");
        }
        config.routes[1].hostname = Some("example.com".into());
        config.routes[1].subdomain = "*".into();
        assert!(config.validate().is_err());
        config.routes[1].subdomain = [
            "a".repeat(63),
            "b".repeat(63),
            "c".repeat(63),
            "d".repeat(60),
        ]
        .join(".");
        assert!(config.validate().is_ok());
        config.routes[1].hostname = None;
        assert!(config.validate().is_err());
        for route in &mut config.routes {
            route.subdomain = "*".into();
        }
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_unsafe_configuration() {
        let mut config = Config::default();
        assert!(config.validate().is_ok());
        for name in ["dev.home.arpa", "dev.test", "example.com", "lan", "123.test"] {
            config.domain = name.into();
            assert!(config.validate().is_ok(), "{name}");
        }
        for name in [
            "",
            "../dev.home.arpa",
            "-dev.home.arpa",
            "Dev.home.arpa",
            "dev..home.arpa",
            "dev.test-",
            "dev_test",
            "dev.test/",
            "dev.123",
            "dev.0x7f",
            "127.0.0.1",
        ] {
            config.domain = name.into();
            assert!(config.validate().is_err(), "{name}");
        }
        config = Config::default();
        config.upstream_dns = "127.0.0.1".into();
        assert!(config.validate().is_err());
        config = Config::default();
        config.routes.push(ProxyRoute {
            id: "one".into(),
            name: "App".into(),
            subdomain: "*".into(),
            hostname: None,
            port: 3000,
            enabled: true,
        });
        assert!(config.validate().is_ok());
        config.routes[0].port = 80;
        assert!(config.validate().is_err());
    }
}
