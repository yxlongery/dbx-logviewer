use crate::path::{remote_join_ok, root_name};
use crate::session::{Session, SshAccept, SshAuth, SshConf, SshLive, normalize_fp};
use crate::{ALLOWED_EXTS, Plugin};
use dbx_plugin_sdk::PluginError;
use serde_json::Value;

// SSH 基建：严格指纹建连、SFTP 会话复用、远端路径解析与增量读写

// SSH 建连全链：TCP(15s)→指纹严格校验→密码/密钥认证→SFTP 子系统；返回复用会话
// 指纹未确认时报错附服务端 SHA256，引导用户填入 host_fingerprint 后重试
pub(crate) async fn connect_ssh(conf: &SshConf) -> Result<SshLive, PluginError> {
    use tokio::time::{timeout, Duration};
    let e = |m: String| PluginError::new(-32000, m);
    let cfg = std::sync::Arc::new(russh::client::Config { nodelay: true, ..Default::default() });
    let acc = SshAccept { expected: conf.fingerprint.clone(), actual: Default::default() };
    let actual = acc.actual.clone();
    let mut sess = timeout(Duration::from_secs(15),
            russh::client::connect(cfg, (conf.host.as_str(), conf.port), acc))
        .await.map_err(|_| e("SSH 连接超时（15s）".to_string()))?
        .map_err(|er| {
            // 握手失败且拿到了服务端指纹→未确认或不匹配，报指纹引导确认
            if let Some(fp) = actual.lock().ok().and_then(|g| g.clone()) {
                if conf.fingerprint.as_ref().map(|x| normalize_fp(x) != normalize_fp(&fp)).unwrap_or(true) {
                    return PluginError::new(-32602, format!(
                        "主机密钥未确认：服务端指纹 {fp}，请核对后将含 SHA256: 前缀的完整指纹填入 host_fingerprint 再连接"));
                }
            }
            e(format!("SSH 连接失败：{er:?}"))
        })?;
    // 认证：密码直验；密钥内容优先（网页上传），无内容读路径文件；口令可选
    let r = match &conf.auth {
        SshAuth::Password(p) => timeout(Duration::from_secs(15),
            sess.authenticate_password(conf.user.clone(), p.clone()))
            .await.map_err(|_| e("SSH 认证超时".to_string()))?
            .map_err(|er| e(format!("SSH 密码认证失败：{er:?}")))?,
        SshAuth::Key { content, path, passphrase } => {
            let raw = match (content, path) {
                (Some(c), _) if !c.trim().is_empty() => c.clone(),
                (_, Some(p)) => std::fs::read_to_string(p)
                    .map_err(|er| e(format!("读取私钥文件失败 {p}：{er}")))?,
                _ => return Err(PluginError::new(-32602, "密钥认证需提供私钥文件或内容")),
            };
            let raw_key = russh::keys::decode_secret_key(&raw, passphrase.clone().as_deref())
                .map_err(|er| e(format!("私钥解析失败：{er}")))?;
            let key = russh::keys::PrivateKeyWithHashAlg::new(std::sync::Arc::new(raw_key), None);
            timeout(Duration::from_secs(15),
                sess.authenticate_publickey(conf.user.clone(), key))
                .await.map_err(|_| e("SSH 认证超时".to_string()))?
                .map_err(|er| e(format!("SSH 密钥认证失败：{er:?}")))?
        }
    };
    if !r.success() {
        return Err(PluginError::new(-32602, "SSH 认证被拒绝：请检查用户/密码/密钥"));
    }
    // SFTP 子系统：开 session 通道挂载，句柄复用供后续 browse/search/tail/download
    let ch = timeout(Duration::from_secs(15), sess.channel_open_session())
        .await.map_err(|_| e("SFTP 通道超时".to_string()))?
        .map_err(|er| e(format!("SFTP 通道失败：{er:?}")))?;
    ch.request_subsystem(true, "sftp").await.map_err(|er| e(format!("SFTP 子系统失败：{er:?}")))?;
    let sftp = russh_sftp::client::SftpSession::new(ch.into_stream()).await
        .map_err(|er| e(format!("SFTP 初始化失败：{er:?}")))?;
    Ok(SshLive { sftp: std::sync::Arc::new(sftp) })
}

// 远端路径解析：首段根短名→真实根；canonicalize 校验不出根（防穿越/软链逃逸，与本地 safe_join 同语义）
// 纯拼接判定可单测；canonicalize 走 SFTP
pub(crate) async fn remote_resolve(live: &SshLive, session: &Session, rel: &str) -> Result<String, PluginError> {
    let file = rel.trim();
    let has_slash_root = session.log_dirs.iter().any(|d| d == "/");
    if file.is_empty() || file.contains('\\') || file.contains("..") {
        return Err(PluginError::new(-32602, "非法文件名"));
    }
    if file.starts_with('/') && !has_slash_root {
        return Err(PluginError::new(-32602, "非法文件名"));
    }
    let (root_seg, _) = file.split_once('/').map(|(a, b)| (a, Some(b))).unwrap_or((file, None));
    let base = session.log_dirs.iter().find(|d| root_name(d) == root_seg)
        .ok_or_else(|| PluginError::new(-32602, "非法文件名"))?;
    let canon_base = live.sftp.canonicalize(base).await
        .map_err(|e| PluginError::new(-32000, format!("无法读取目录 {base}：{e:?}")))?;
    let sub = file[root_seg.len()..].trim_start_matches('/');
    // 全盘根 canon_base 为 "/" 时直接拼，避免 "//etc" 双斜杠（remote_join_ok 对 "/" 按单斜杠判定）
    let joined = if sub.is_empty() { canon_base.clone() }
        else if canon_base == "/" { format!("/{sub}") }
        else { format!("{canon_base}/{sub}") };
    let canon = live.sftp.canonicalize(&joined).await
        .map_err(|e| PluginError::new(-32000, format!("路径不存在 {file}：{e:?}")))?;
    if !remote_join_ok(&canon_base, &canon) {
        return Err(PluginError::new(-32602, "非法文件名"));
    }
    Ok(canon)
}

// 远端日志计数：根下 .log/.out 文件数（test/connect 提前校验用）
pub(crate) async fn count_remote_logs(live: &SshLive, dir: &str) -> Result<usize, PluginError> {
    let canon = live.sftp.canonicalize(dir).await
        .map_err(|e| PluginError::new(-32000, format!("无法读取目录 {dir}：{e:?}")))?;
    let rd = live.sftp.read_dir(&canon).await
        .map_err(|e| PluginError::new(-32000, format!("无法读取目录 {dir}：{e:?}")))?;
    let mut n = 0;
    for entry in rd {
        let name = entry.file_name();
        if name.starts_with('.') || !entry.metadata().is_regular() {
            continue;
        }
        if name.rsplit('.').next().map(|e| ALLOWED_EXTS.contains(&e.to_ascii_lowercase().as_str())).unwrap_or(false) {
            n += 1;
        }
    }
    Ok(n)
}

// 取复用 SFTP 会话：无则报连接未建立（disconnect 后或 connect 失败）
pub(crate) fn ssh_live_of(plugin: &Plugin, params: &Value, session: &Session) -> Result<SshLive, PluginError> {
    if session.ssh.is_none() {
        return Err(PluginError::new(-32000, "非 SSH 会话"));
    }
    let id = params.get("connectionId").and_then(Value::as_str).unwrap_or("").to_string();
    plugin.ssh_lives
        .lock()
        .map_err(|_| PluginError::new(-32000, "Session registry is poisoned"))?
        .get(&id)
        .cloned()
        .ok_or_else(|| PluginError::new(-32000, "连接未建立：请先连接后再操作"))
}

// 目录下 .log/.out 文件计数，用于 test/connect 提前校验

pub(crate) async fn remote_len(live: &SshLive, path: &str) -> Result<u64, PluginError> {
    live.sftp.metadata(path).await
        .map(|m| m.size.unwrap_or(0))
        .map_err(|e| PluginError::new(-32000, format!("无法读取文件 {path}：{e:?}")))
}

// tail 启动远端版：先推末 lastN 行，返回当前长度作增量起点
