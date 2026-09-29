use dbx_plugin_sdk::PluginError;
use serde_json::Value;
use std::sync::Arc;
use std::sync::OnceLock;

// 会话与 SSH 配置类型：connect 表单解析与复用会话的基础

// 全局 Tokio Runtime：同步 handle 内 block_on 跑 SSH/SFTP，本地逻辑零触碰
static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
pub(crate) fn rt() -> &'static tokio::runtime::Runtime {
    RT.get_or_init(|| tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2).enable_all().build().expect("tokio runtime"))
}

// 会话只存非敏感信息：目录列表（逗号分隔多根）。密码/私钥由宿主保管，绝不落内存日志。
// SSH 模式：conf 只存连接参数，live 复用 SFTP 会话；Secret（密码/密钥原文）仅 connect 瞬时使用，不持久化。
#[derive(Clone)]
pub(crate) struct Session {
    pub(crate) log_dirs: Vec<String>,
    pub(crate) ssh: Option<SshConf>,
}

// SSH 连接参数：指纹为用户确认后的 SHA256（表单 host_fingerprint 持久化）
#[derive(Clone)]
pub(crate) struct SshConf {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) user: String,
    pub(crate) auth: SshAuth,
    pub(crate) fingerprint: Option<String>,
}

#[derive(Clone)]
pub(crate) enum SshAuth {
    Password(String),
    Key { content: Option<String>, path: Option<String>, passphrase: Option<String> },
}

// 复用 SFTP 会话：connect 建连一次，browse/search/tail/download 共用（Arc 共享，方法均为 &self）
#[derive(Clone)]
pub(crate) struct SshLive {
    pub(crate) sftp: std::sync::Arc<russh_sftp::client::SftpSession>,
}

// 严格主机密钥校验：expected 为表单已确认指纹；actual 经 Arc 带回供报错
#[derive(Clone)]
pub(crate) struct SshAccept {
    pub(crate) expected: Option<String>,
    pub(crate) actual: Arc<std::sync::Mutex<Option<String>>>,
}

impl russh::client::Handler for SshAccept {
    type Error = russh::Error;
    async fn check_server_key(
        &mut self,
        key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        // 证书取内层公钥；两种形态统一按 SHA256 指纹比对
        let fp = match key {
            russh::keys::PublicKeyOrCertificate::PublicKey { key, .. } => key_fingerprint(key),
            russh::keys::PublicKeyOrCertificate::Certificate(c) => key_fingerprint(&c.public_key().clone().into()),
        };
        *self.actual.lock().unwrap() = Some(fp.clone());
        Ok(self.expected.as_ref().map(|e| normalize_fp(e) == normalize_fp(&fp)).unwrap_or(false))
    }
}

// 指纹归一化：兼容用户去前缀粘贴（有/无 SHA256:、大小写、首尾空格都视为相同）
pub(crate) fn normalize_fp(s: &str) -> String {
    let t = s.trim();
    let body = if t.len() >= 7 && t[..7].eq_ignore_ascii_case("sha256:") {
        t[7..].trim()
    } else {
        t
    };
    body.to_string()
}

// 服务端公钥指纹：ssh-key 原生 SHA256 格式（"SHA256:..."）
fn key_fingerprint(key: &russh::keys::PublicKey) -> String {
    format!("{}", key.fingerprint(russh::keys::HashAlg::Sha256))
}

pub(crate) fn str_field(conn: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        for holder in [conn.get("external_config"), conn.get("config"), Some(conn)] {
            if let Some(v) = holder.and_then(|h| h.get(*key)).and_then(Value::as_str) {
                if !v.trim().is_empty() {
                    return Some(v.trim().to_string());
                }
            }
        }
    }
    None
}

// Secret 字段读取：宿主补齐 Secret 到 connection_secrets（官方协议），散装兼容 secret/external_config/config/顶层
pub(crate) fn secret_field(conn: &Value, key: &str) -> Option<String> {
    for holder in [conn.get("connection_secrets"), conn.get("secret"), conn.get("external_config"), conn.get("config"), Some(conn)] {
        if let Some(v) = holder.and_then(|h| h.get(key)).and_then(Value::as_str) {
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

// 表单解析 SSH 配置：ssh_host 留空=本地模式；端口读 number 或字符串；认证按 auth_method 取密码/密钥
pub(crate) fn ssh_conf(conn: &Value) -> Result<Option<SshConf>, PluginError> {
    let host = str_field(conn, &["ssh_host", "host"]).unwrap_or_default();
    if host.is_empty() {
        return Ok(None);
    }
    let port = [conn.get("external_config"), conn.get("config"), Some(conn)].iter()
        .filter_map(|h| h.and_then(|x| x.get("ssh_port")).or_else(|| h.and_then(|x| x.get("port"))))
        .filter_map(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())))
        .next().unwrap_or(22) as u16;
    let user = str_field(conn, &["ssh_user", "username"])
        .ok_or_else(|| PluginError::new(-32602, "SSH 模式需填写用户"))?;
    let method = str_field(conn, &["auth_method"]).unwrap_or_else(|| "password".to_string());
    let auth = if method == "key" {
        SshAuth::Key {
            content: secret_field(conn, "key_content"),
            path: str_field(conn, &["key_path", "private_key_path"]),
            passphrase: secret_field(conn, "passphrase"),
        }
    } else {
        SshAuth::Password(secret_field(conn, "ssh_password")
            .ok_or_else(|| PluginError::new(-32602, "密码认证需填写 SSH 密码"))?)
    };
    Ok(Some(SshConf { host, port, user, auth,
        fingerprint: str_field(conn, &["host_fingerprint"]) }))
}

// 会话构造器：测试用单根/多根快速建 Session（本地模式无 SSH，仅测试用）
#[cfg(test)]
pub(crate) fn test_session(dirs: Vec<String>) -> Session {
    Session { log_dirs: dirs, ssh: None }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 指纹归一化：有/无 SHA256: 前缀、大小写、首尾空格都视为相同（用户去前缀粘贴的兼容）
    #[test]
    fn normalize_fp_tolerates_prefix() {
        let full = "SHA256:UtSATc4UWxw23juuWenUpPqYhcSxb6nIt4FTBdpND3w";
        assert_eq!(normalize_fp(full), "UtSATc4UWxw23juuWenUpPqYhcSxb6nIt4FTBdpND3w");
        assert_eq!(normalize_fp("UtSATc4UWxw23juuWenUpPqYhcSxb6nIt4FTBdpND3w"), normalize_fp(full));
        assert_eq!(normalize_fp("  sha256:UtSATc4UWxw23juuWenUpPqYhcSxb6nIt4FTBdpND3w  "), normalize_fp(full));
        assert_ne!(normalize_fp("SHA256:other"), normalize_fp(full));
    }

    // SSH 表单解析：留空=本地；密码缺失报错；密钥分支取内容/路径/口令；端口读 number
    #[test]
    fn ssh_conf_parses_modes() {
        // 本地：无 ssh_host
        let local = serde_json::json!({ "log_dir": "/var/log" });
        assert!(ssh_conf(&local).unwrap().is_none());
        // 密码模式缺密码报错
        let nopw = serde_json::json!({ "ssh_host": "1.2.3.4", "ssh_user": "root", "auth_method": "password" });
        assert!(ssh_conf(&nopw).is_err());
        // 密码模式正常：端口默认 22
        let pw = serde_json::json!({ "ssh_host": "1.2.3.4", "ssh_user": "root",
            "auth_method": "password", "secret": { "ssh_password": "p" } });
        let c = ssh_conf(&pw).unwrap().unwrap();
        assert_eq!(c.port, 22);
        assert!(matches!(c.auth, SshAuth::Password(_)));
        // number 端口 + 指纹透传
        let pw2 = serde_json::json!({ "external_config": { "ssh_host": "1.2.3.4", "ssh_port": 2222 },
            "ssh_user": "root", "auth_method": "password", "secret": { "ssh_password": "p" },
            "config": { "host_fingerprint": "SHA256:abc" } });
        let c2 = ssh_conf(&pw2).unwrap().unwrap();
        assert_eq!(c2.port, 2222);
        assert_eq!(c2.fingerprint.as_deref(), Some("SHA256:abc"));
        // 密钥模式：内容优先于路径，口令可选
        let k = serde_json::json!({ "ssh_host": "1.2.3.4", "ssh_user": "root", "auth_method": "key",
            "secret": { "key_content": "KEY", "passphrase": "pp" } });
        let c3 = ssh_conf(&k).unwrap().unwrap();
        assert!(matches!(c3.auth, SshAuth::Key { .. }));
        // 密钥无内容无路径报错
        let knone = serde_json::json!({ "ssh_host": "1.2.3.4", "ssh_user": "root", "auth_method": "key" });
        let c4 = ssh_conf(&knone).unwrap().unwrap();
        assert!(matches!(c4.auth, SshAuth::Key { content: None, path: None, .. }));
    }
}
