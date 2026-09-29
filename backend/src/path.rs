use crate::session::Session;
use crate::ALLOWED_EXTS;
use dbx_plugin_sdk::PluginError;

// 路径防护：本地 canonicalize 与远端拼接判定，搜索正确性的根基

pub(crate) fn remote_join_ok(base_canon: &str, canon: &str) -> bool {
    if base_canon == "/" {
        return canon.starts_with('/'); // 全盘根：界内即全盘（退化为扩展名过滤）
    }
    canon == base_canon || canon.starts_with(&format!("{base_canon}/"))
}


pub(crate) fn count_log_files(dir: &str) -> Result<usize, PluginError> {
    let entries = std::fs::read_dir(dir).map_err(|e| PluginError::new(-32000, format!("无法读取目录 {dir}：{e}")))?;
    let mut n = 0;
    for entry in entries.flatten() {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        if p.extension()
            .and_then(|e| e.to_str())
            .map(|e| ALLOWED_EXTS.contains(&e.to_ascii_lowercase().as_str()))
            .unwrap_or(false)
        {
            n += 1;
        }
    }
    Ok(n)
}

// 多根解析：log_dir 逗号分隔（如 "/app/data,/logs"），去空去尾斜杠，至少一根；"/" 保留为全盘根
pub(crate) fn parse_roots(raw: &str) -> Result<Vec<String>, PluginError> {
    let dirs: Vec<String> = raw.split(',').map(|s| {
        let t = s.trim();
        let stripped = t.trim_end_matches('/');
        if stripped.is_empty() && t.starts_with('/') {
            "/".to_string() // "/"、"///" 归一为全盘根（原先被剥空误报 Missing）
        } else {
            stripped.to_string()
        }
    })
        .filter(|s| !s.is_empty()).collect();
    if dirs.is_empty() {
        return Err(PluginError::new(-32602, "Missing log_dir"));
    }
    for d in &dirs {
        if !d.starts_with('/') {
            return Err(PluginError::new(-32602, format!("根目录必须是绝对路径：{d}")));
        }
    }
    Ok(dirs)
}

// 根短名：取 basename（如 /app/data → data），browse 首段定位用；重名在 connect 拒绝
pub(crate) fn root_name(dir: &str) -> String {
    dir.rsplit('/').next().unwrap_or(dir).to_string()
}

// 路径约束：rel 首段为根短名（如 logs/autofeedemby/20260925.log），禁 .. ；"/" 根的 rel 以 / 开头（如 /etc/a.log，空首段匹配全盘根）
// 绝对路径仅 "/" 根放行，其余仍拒绝；拼接后 canonicalize 校验仍在所属根内（"/" 根退化为扩展名过滤）
pub(crate) fn safe_join(session: &Session, rel: &str) -> Result<String, PluginError> {
    let file = rel.trim();
    let has_slash_root = session.log_dirs.iter().any(|d| d == "/");
    if file.is_empty() || file.contains('\\') || file.contains("..") {
        return Err(PluginError::new(-32602, "非法文件名"));
    }
    if file.starts_with('/') && !has_slash_root {
        return Err(PluginError::new(-32602, "非法文件名"));
    }
    let (root_seg, _rest) = file.split_once('/').map(|(a, b)| (a, Some(b))).unwrap_or((file, None));
    let base = session.log_dirs.iter().find(|d| root_name(d) == root_seg)
        .ok_or_else(|| PluginError::new(-32602, "非法文件名"))?;
    let canon_base = std::path::Path::new(base).canonicalize()
        .map_err(|e| PluginError::new(-32000, format!("无法读取目录 {base}：{e}")))?;
    // 首段根短名映射回真实根后拼接余下部分
    let sub = file[root_seg.len()..].trim_start_matches('/');
    let joined = std::path::Path::new(base).join(sub);
    let canon = joined.canonicalize().map_err(|e| PluginError::new(-32000, format!("路径不存在 {file}：{e}")))?;
    if !canon.starts_with(&canon_base) {
        return Err(PluginError::new(-32602, "非法文件名"));
    }
    Ok(canon.to_string_lossy().to_string())
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::test_session;
    #[test]
    fn safe_join_blocks_traversal() {
        let s = test_session(vec!["/var/log/aiban".to_string()]);
        assert!(safe_join(&s, "../etc/passwd").is_err()); // 穿越
        assert!(safe_join(&s, "/etc/passwd").is_err()); // 绝对路径
        assert!(safe_join(&s, "sub\\dir.log").is_err()); // 反斜杠
        assert!(safe_join(&s, "").is_err()); // 空串
        assert!(safe_join(&s, "other/a.log").is_err()); // 未知根短名
    }

    // 子路径放行是目录浏览的前提：真实建目录验证 canonicalize 链路
    #[test]
    fn safe_join_allows_subdir_inside_base() {
        let base = std::env::temp_dir().join("dbx-logviewer-safejoin");
        let sub = base.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("a.log"), "x\n").unwrap();
        let b = base.to_str().unwrap().to_string();
        let root = root_name(&b);
        let s = test_session(vec![b]);
        assert!(safe_join(&s, &format!("{root}/sub/a.log")).is_ok()); // 子路径放行
        assert!(safe_join(&s, &format!("{root}/sub")).is_ok()); // 子目录同样放行（browse 用）
        assert!(safe_join(&s, "sub/../../evil").is_err()); // 拼接后越界（含 .. 直接拒绝）
        assert!(safe_join(&s, &format!("{root}/no-such-file.log")).is_err()); // 不存在
        std::fs::remove_dir_all(&base).ok(); // 测试收尾清理临时目录
    }

    // "/" 全盘根：parse_roots 保留（不再误报 Missing），safe_join 以 / 前缀相对全盘解析，穿越仍拒绝
    #[test]
    fn slash_root_resolves() {
        assert_eq!(crate::path::parse_roots("/").unwrap(), vec!["/".to_string()]);
        assert_eq!(crate::path::parse_roots("/,/var/log").unwrap(), vec!["/".to_string(), "/var/log".to_string()]);
        assert!(crate::path::parse_roots("").is_err());
        assert!(crate::path::remote_join_ok("/", "/etc/passwd"));
        assert!(!crate::path::remote_join_ok("/", "etc/passwd"));
        // 真文件系统验证：临时目录经 "/" 根相对解析放行，穿越/反斜杠拒绝
        let base = std::env::temp_dir().join("dbx-logviewer-slashroot");
        std::fs::create_dir_all(base.join("sub")).unwrap();
        std::fs::write(base.join("sub").join("a.log"), "x\n").unwrap();
        let s = test_session(vec!["/".to_string()]);
        let rel = format!("/{}/sub/a.log", base.strip_prefix("/").unwrap().to_str().unwrap());
        assert!(safe_join(&s, &rel).is_ok());
        assert!(safe_join(&s, "/../evil").is_err()); // 含 .. 拒绝
        assert!(safe_join(&s, "/sub\\a.log").is_err()); // 反斜杠拒绝
        // 无 "/" 根时绝对路径仍拒绝（旧语义不变）
        let s2 = test_session(vec!["/var/log".to_string()]);
        assert!(safe_join(&s2, "/etc/hostname").is_err());
        std::fs::remove_dir_all(&base).ok();
    }

    // browse 只列一层：子层与非日志不出，穿越/绝对拒绝；断裂链列出不整层失败

    // 远端路径拼接判定：根内放行，根外/相等外拒绝（canonicalize 走 SFTP，判定纯函数可测）
    #[test]
    fn remote_join_ok_guards_root() {
        assert!(remote_join_ok("/logs", "/logs/app/a.log"));
        assert!(remote_join_ok("/logs", "/logs"));
        assert!(!remote_join_ok("/logs", "/logs2/evil.log")); // 前缀欺骗
        assert!(!remote_join_ok("/logs", "/etc/passwd"));
    }
}
