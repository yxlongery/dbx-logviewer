use crate::path::{is_log_name, root_name, safe_join};
use crate::session::{rt, Session, SshLive};
use crate::ssh::{remote_resolve, ssh_live_of};
use crate::ALLOWED_EXTS;
use dbx_plugin_sdk::PluginError;
use serde_json::{json, Value};

// 目录浏览：空串列根短名，非空下钻；SSH 版经 SFTP（软链跟随，失败不断层）

impl crate::Plugin {
    // logs/browse：空串列各根短名；非空首段为根短名，余下为该根下相对目录；点选下钻代替手输路径
    pub(crate) fn browse(&self, params: &Value) -> Result<Value, PluginError> {
        let session = self.session(params)?;
        // SSH 模式走 SFTP（复用会话，block_on 桥接）
        if session.ssh.is_some() {
            let live = ssh_live_of(self, params, &session)?;
            let dir_rel = params.get("dir").and_then(Value::as_str).unwrap_or("").trim().to_string();
            return rt().block_on(Self::browse_ssh(&live, &session, &dir_rel));
        }
        let dir_rel = params.get("dir").and_then(Value::as_str).unwrap_or("").trim().to_string();
        // 空串为根：直接列各根短名，不读盘
        if dir_rel.is_empty() {
            let mut dirs: Vec<Value> = session.log_dirs.iter().map(|d| json!({ "name": if d == "/" { "/".to_string() } else { root_name(d) } })).collect();
            dirs.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
            return Ok(json!({ "dir": "", "dirs": dirs, "entries": [] }));
        }
        // 非空走同一防穿越约束（.. / 绝对一律拒绝，拼接后 canonicalize 校验仍在所属根内）
        let dir_abs = safe_join(&session, &dir_rel)?;
        if !std::fs::metadata(&dir_abs).map(|m| m.is_dir()).unwrap_or(false) {
            return Err(PluginError::new(-32602, "不是目录"));
        }
        let mut dirs = Vec::new();
        let mut entries = Vec::new();
        let rd = std::fs::read_dir(&dir_abs)
            .map_err(|e| PluginError::new(-32000, format!("无法读取目录 {dir_rel}：{e}")))?;
        for entry in rd.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue; // 隐藏文件/目录不展示
            }
            if path.is_dir() {
                dirs.push(json!({ "name": name }));
                continue;
            }
            if !path.is_file() {
                continue;
            }
            if !path.file_name().and_then(|s| s.to_str()).map(is_log_name).unwrap_or(false) {
                continue;
            }
            // 断裂软链等坏条目：列出但大小时间为 0，不整层失败（点选时 search 报路径不存在）
            let (size, modified_at) = entry.metadata().map(|m| (m.len(), m.modified().ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs()).unwrap_or(0))).unwrap_or((0, 0));
            // 链接目标：普通文件为 null，软链返回目标绝对路径（文本节点展示用）
            let link_target = std::fs::read_link(&path).ok().map(|p| p.to_string_lossy().to_string());
            // 相对路径透给前端：search/tail/download 直接用它，不再拼；"/" 根的 rel 带 / 前缀（如 /etc/a.log）
            let rel = if dir_rel.is_empty() { name } else if dir_rel == "/" { format!("/{name}") } else { format!("{dir_rel}/{name}") };
            entries.push(json!({
                "name": rel,
                "size": size,
                "modified_at": modified_at,
                "target": link_target,
            }));
        }
        dirs.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        entries.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        Ok(json!({ "dir": dir_rel, "dirs": dirs, "entries": entries }))
    }

    // logs/browse 远端版：SFTP 列单层；软链按跟随 metadata，失败则大小时间为 0（不断层）
async fn browse_ssh(live: &SshLive, session: &Session, dir_rel: &str) -> Result<Value, PluginError> {
    if dir_rel.is_empty() {
        let mut dirs: Vec<Value> = session.log_dirs.iter().map(|d| json!({ "name": if d == "/" { "/".to_string() } else { root_name(d) } })).collect();
        dirs.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        return Ok(json!({ "dir": "", "dirs": dirs, "entries": [] }));
    }
    let dir_abs = remote_resolve(live, session, dir_rel).await?;
    let meta = live.sftp.metadata(&dir_abs).await
        .map_err(|e| PluginError::new(-32602, format!("无法读取目录 {dir_rel}：{e:?}")))?;
    if !meta.is_dir() {
        return Err(PluginError::new(-32602, "不是目录"));
    }
    let rd = live.sftp.read_dir(&dir_abs).await
        .map_err(|e| PluginError::new(-32000, format!("无法读取目录 {dir_rel}：{e:?}")))?;
    let mut dirs = Vec::new();
    let mut entries = Vec::new();
    for entry in rd {
        let name = entry.file_name();
        if name.starts_with('.') {
            continue;
        }
        let m = entry.metadata();
        if m.is_dir() {
            dirs.push(json!({ "name": name }));
            continue;
        }
        if !m.is_regular() && !m.is_symlink() {
            continue;
        }
        if !is_log_name(&name) {
            continue;
        }
        let rel = if dir_rel == "/" { format!("/{name}") } else { format!("{dir_rel}/{name}") };
        entries.push(json!({
            "name": rel,
            "size": m.size.unwrap_or(0),
            "modified_at": m.mtime.unwrap_or(0),
            "target": Value::Null,
        }));
    }
    dirs.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    entries.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    Ok(json!({ "dir": dir_rel, "dirs": dirs, "entries": entries }))
}
}

#[cfg(test)]
mod tests {
    use crate::session::test_session;
    use crate::{path::root_name, Plugin};
    // browse 只列一层：子层与非日志不出，穿越/绝对拒绝；断裂链列出不整层失败
    #[test]
    fn browse_lists_one_level() {
        let base = std::env::temp_dir().join("dbx-logviewer-browse");
        let logs = base.join("logs");
        let sub = logs.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(logs.join("top.log"), "t\n").unwrap();
        std::fs::write(sub.join("a.log"), "a\n").unwrap();
        std::fs::write(logs.join("note.txt"), "n\n").unwrap(); // 非日志不列
        let plugin = Plugin::default();
        let conn_id = "browse-test-conn";
        let root = root_name(logs.to_str().unwrap());
        plugin.sessions.lock().unwrap().insert(conn_id.to_string(),
            test_session(vec![logs.to_str().unwrap().to_string()]));
        let r = plugin.browse(&serde_json::json!({ "connectionId": conn_id })).expect("根应成功");
        assert!(r["dirs"].as_array().unwrap().iter().any(|x| x["name"] == root)); // 根聚合成根短名
        let s = plugin.browse(&serde_json::json!({ "connectionId": conn_id, "dir": root })).expect("根短名应成功");
        assert!(s["dirs"].as_array().unwrap().iter().any(|x| x["name"] == "sub"));
        let names: Vec<String> = s["entries"].as_array().unwrap().iter()
            .map(|x| x["name"].as_str().unwrap().to_string()).collect();
        assert!(names.contains(&format!("{root}/top.log")));
        assert!(!names.iter().any(|n| n.contains("note.txt") || n.contains("a.log"))); // 子层与非日志不出
        let s2 = plugin.browse(&serde_json::json!({ "connectionId": conn_id, "dir": format!("{root}/sub") })).expect("子目录应成功");
        assert_eq!(s2["entries"].as_array().unwrap().len(), 1);
        assert_eq!(s2["entries"][0]["name"], format!("{root}/sub/a.log"));
        assert!(plugin.browse(&serde_json::json!({ "connectionId": conn_id, "dir": "../" })).is_err()); // 穿越
        assert!(plugin.browse(&serde_json::json!({ "connectionId": conn_id, "dir": "/etc" })).is_err()); // 绝对
        std::fs::remove_dir_all(&base).ok(); // 测试收尾清理临时目录
    }

    // 多根：逗号分隔建会话，根间隔离（A 根拼不出 B 根文件），重名拒绝
    #[test]
    fn multi_root_isolation() {
        let base = std::env::temp_dir().join("dbx-logviewer-multiroot");
        let a = base.join("dataA");
        let b = base.join("dataB");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("a.log"), "a\n").unwrap();
        std::fs::write(b.join("b.log"), "b\n").unwrap();
        let plugin = Plugin::default();
        let raw = format!("{},{}", a.to_str().unwrap(), b.to_str().unwrap());
        let r = plugin.connect(&serde_json::json!({ "connection": { "id": "multi-conn", "log_dir": raw } }));
        assert!(r.is_ok());
        let ra = root_name(a.to_str().unwrap());
        let rb = root_name(b.to_str().unwrap());
        let root = plugin.browse(&serde_json::json!({ "connectionId": "multi-conn" })).expect("根应成功");
        let dirs: Vec<String> = root["dirs"].as_array().unwrap().iter()
            .map(|x| x["name"].as_str().unwrap().to_string()).collect();
        assert!(dirs.contains(&ra) && dirs.contains(&rb));
        let s = plugin.search(&serde_json::json!({ "connectionId": "multi-conn",
            "file": format!("{rb}/b.log"), "page": 1, "pageSize": 10 })).expect("跨根搜索应成功");
        assert_eq!(s["total"], 1);
        assert!(plugin.search(&serde_json::json!({ "connectionId": "multi-conn",
            "file": format!("{ra}/../dataB/b.log"), "page": 1, "pageSize": 10 })).is_err()); // 含 .. 拒绝
        // 同名根拒绝：x/logs 与 y/logs 短名同为 logs
        let x = base.join("x").join("logs");
        let y = base.join("y").join("logs");
        std::fs::create_dir_all(&x).unwrap();
        std::fs::create_dir_all(&y).unwrap();
        let dup = plugin.connect(&serde_json::json!({ "connection": { "id": "dup-conn",
            "log_dir": format!("{},{}", x.to_str().unwrap(), y.to_str().unwrap()) } }));
        assert!(dup.is_err());
        std::fs::remove_dir_all(&base).ok(); // 测试收尾清理临时目录
    }

    // 移除 SSH 选项后的兼容：旧连接残留 mode=ssh 不再被拒绝，按本地目录处理
    #[test]
    fn connect_ignores_legacy_ssh_mode() {
        let dir = std::env::temp_dir().join("dbx-logviewer-legacy-ssh");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.log"), "hello\n").unwrap();
        let plugin = Plugin::default();
        let params = serde_json::json!({
            "connection": { "id": "legacy-ssh-conn", "mode": "ssh", "log_dir": dir.to_str().unwrap() }
        });
        let r = plugin.connect(&params).expect("旧 ssh 负载应按本地目录处理");
        assert_eq!(r["success"], true);
        std::fs::remove_dir_all(&dir).ok(); // 测试收尾清理临时目录
    }
}
