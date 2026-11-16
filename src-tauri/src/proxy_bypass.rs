use crate::model::{Config, SERVICE_CONFIG, SERVICE_DIR};
use std::{collections::BTreeMap, fs, io::Write, process::Command};

pub const STATE_FILE: &str = "proxy-bypass.json";

type ManagedDomains = BTreeMap<String, Vec<String>>;

fn desired_domains(config: &Config) -> Vec<String> {
    let mut domains = vec![config.domain.clone(), format!("*.{}", config.domain)];
    domains.extend(
        config
            .routes
            .iter()
            .filter(|route| route.enabled)
            .filter_map(|route| route.hostname.clone()),
    );
    domains.sort();
    domains.dedup();
    domains
}

fn merge_domains(
    current: &[String],
    managed: &[String],
    desired: &[String],
) -> (Vec<String>, Vec<String>) {
    let mut merged: Vec<_> = current
        .iter()
        .filter(|domain| !managed.contains(domain))
        .cloned()
        .collect();
    let mut owned = Vec::new();
    for domain in desired {
        if !merged
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(domain))
        {
            merged.push(domain.clone());
            owned.push(domain.clone());
        }
    }
    (merged, owned)
}

fn rollback_domains(current: &[String], original: &[String], applied: &[String]) -> Vec<String> {
    // Undo only this transaction's changes so concurrent additions and removals survive.
    let mut restored: Vec<_> = current
        .iter()
        .filter(|domain| original.contains(domain) || !applied.contains(domain))
        .cloned()
        .collect();
    for (index, domain) in original.iter().enumerate() {
        if !applied.contains(domain) && !restored.contains(domain) {
            restored.insert(index.min(restored.len()), domain.clone());
        }
    }
    restored
}

fn networksetup(arguments: &[&str]) -> Result<String, String> {
    let output = Command::new("/usr/sbin/networksetup")
        .env("LC_ALL", "C")
        .args(arguments)
        .output()
        .map_err(|error| format!("无法管理系统代理绕过列表：{error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    // networksetup can print authorization errors while returning exit code zero.
    if !output.status.success() || stdout.contains("** Error:") || !stderr.trim().is_empty() {
        return Err(format!(
            "系统代理设置失败：{} {}",
            stdout.trim(),
            stderr.trim()
        ));
    }
    Ok(stdout.trim_end_matches(['\r', '\n']).to_owned())
}

fn read_domains(service: &str) -> Result<Vec<String>, String> {
    let output = networksetup(&["-getproxybypassdomains", service])?;
    if output == format!("There aren't any bypass domains set on {service}.") {
        return Ok(Vec::new());
    }
    Ok(output.lines().map(str::to_owned).collect())
}

fn write_domains(service: &str, domains: &[String]) -> Result<(), String> {
    let mut arguments = vec!["-setproxybypassdomains", service];
    if domains.is_empty() {
        arguments.push("Empty");
    } else {
        arguments.extend(domains.iter().map(String::as_str));
    }
    networksetup(&arguments)?;
    if read_domains(service)? != domains {
        return Err(format!(
            "{service} 的代理绕过列表未生效，请检查代理软件是否覆盖了系统设置。"
        ));
    }
    Ok(())
}

fn network_services() -> Result<Vec<(String, String)>, String> {
    // shortcut: Manage the current location; use SystemConfiguration if cross-location cleanup is needed.
    let output = Command::new("/usr/bin/plutil")
        .args([
            "-convert",
            "json",
            "-o",
            "-",
            "/Library/Preferences/SystemConfiguration/preferences.plist",
        ])
        .output()
        .map_err(|error| format!("无法读取网络服务：{error}"))?;
    if !output.status.success() {
        return Err("无法读取系统网络配置，代理绕过列表未更改。".into());
    }
    let preferences: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())?;
    let current = preferences["CurrentSet"]
        .as_str()
        .ok_or("无法识别当前网络位置")?;
    let services = preferences
        .pointer(&format!("{current}/Network/Service"))
        .and_then(serde_json::Value::as_object)
        .ok_or("无法读取当前网络位置的服务")?;
    // UUID ownership survives service renames and separates identically named network locations.
    services
        .keys()
        .map(|id| {
            let name = preferences["NetworkServices"][id]["UserDefinedName"]
                .as_str()
                .ok_or("网络服务名称缺失")?;
            Ok((id.clone(), name.to_owned()))
        })
        .collect()
}

pub fn sync(clear: bool) -> Result<(), String> {
    let state_path = std::path::Path::new(SERVICE_DIR).join(STATE_FILE);
    let mut managed: ManagedDomains = match fs::read(&state_path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| format!("代理绕过记录格式错误：{error}"))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
        Err(error) => return Err(format!("无法读取代理绕过记录：{error}")),
    };
    if clear && managed.is_empty() {
        return Ok(());
    }
    let desired = if clear {
        Vec::new()
    } else {
        let config: Config =
            serde_json::from_slice(&fs::read(SERVICE_CONFIG).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        config.validate()?;
        desired_domains(&config)
    };
    let mut changes = Vec::new();
    for (id, name) in network_services()? {
        if clear && !managed.contains_key(&id) {
            continue;
        }
        let current = read_domains(&name)?;
        let previous = managed.remove(&id).unwrap_or_default();
        let (merged, owned) = merge_domains(&current, &previous, &desired);
        if !owned.is_empty() {
            managed.insert(id, owned);
        }
        if current != merged {
            changes.push((name, current, merged));
        }
    }
    let mut state =
        tempfile::NamedTempFile::new_in(SERVICE_DIR).map_err(|error| error.to_string())?;
    state
        .write_all(&serde_json::to_vec_pretty(&managed).map_err(|error| error.to_string())?)
        .and_then(|()| state.as_file().sync_all())
        .map_err(|error| error.to_string())?;
    let mut attempted = 0;
    let result = (|| {
        for (name, original, merged) in &changes {
            if read_domains(name)? != *original {
                return Err(format!("{name} 的代理设置刚刚发生变化，请重新保存配置。"));
            }
            attempted += 1;
            write_domains(name, merged)?;
        }
        state
            .persist(&state_path)
            .map_err(|error| format!("无法保存代理绕过记录：{error}"))?;
        Ok(())
    })();
    if let Err(mut error) = result {
        for (name, original, applied) in changes[..attempted].iter().rev() {
            let rollback = read_domains(name).and_then(|latest| {
                let restored = rollback_domains(&latest, original, applied);
                if latest == restored {
                    Ok(())
                } else {
                    write_domains(name, &restored)
                }
            });
            if let Err(rollback) = rollback {
                error.push_str(&format!("；恢复原代理设置失败：{rollback}"));
            }
        }
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn preserves_existing_rules_and_removes_only_owned_domains() {
        let original = strings(&["localhost", "*.dev.home.arpa", "SUB.SSO", "*.company.test"]);
        let desired = strings(&["*.dev.home.arpa", "sub.sso", "api.sso"]);
        let (merged, owned) = merge_domains(&original, &[], &desired);
        assert_eq!(owned, strings(&["api.sso"]));
        assert_eq!(
            merge_domains(&merged, &owned, &desired),
            (merged.clone(), owned.clone())
        );
        assert_eq!(merge_domains(&merged, &owned, &[]), (original, vec![]));
        let (renamed, owned) = merge_domains(&merged, &owned, &strings(&["new.sso"]));
        assert!(!renamed.contains(&"api.sso".to_owned()));
        assert!(renamed.contains(&"SUB.SSO".to_owned()));
        assert_eq!(owned, strings(&["new.sso"]));
    }

    #[test]
    fn includes_default_suffix_and_only_enabled_custom_hosts() {
        let mut config = Config::default();
        config.routes = serde_json::from_value(serde_json::json!([
            {"id":"one","name":"One","subdomain":"sub","hostname":"sub.sso","port":3002,"enabled":true},
            {"id":"two","name":"Two","subdomain":"off","hostname":"off.sso","port":3002,"enabled":false}
        ])).unwrap();
        assert_eq!(
            desired_domains(&config),
            strings(&["*.dev.home.arpa", "dev.home.arpa", "sub.sso"])
        );
    }

    #[test]
    fn rollback_preserves_concurrent_edits_and_removes_transaction_domains() {
        let original = strings(&["localhost", "old.sso", "user-removed.test"]);
        let applied = strings(&["localhost", "user-removed.test", "new.sso"]);
        let concurrent = strings(&["localhost", "new.sso", "external.test"]);
        assert_eq!(rollback_domains(&applied, &original, &applied), original);
        assert_eq!(
            rollback_domains(&concurrent, &original, &applied),
            strings(&["localhost", "old.sso", "external.test"])
        );
    }
}
