use crate::path::{count_log_files, parse_roots, root_name};
use crate::session::{rt, ssh_conf, str_field, Session};
use crate::ssh::{connect_ssh, count_remote_logs};
use dbx_plugin_sdk::PluginError;
use serde_json::{json, Value};
use std::sync::atomic::Ordering;

// 连接生命周期：test 校验、connect 建会话（含 SSH 复用）、disconnect 释放

impl crate::Plugin {
    // connection/test：只校验各根目录可读，不建会话
    pub(crate) fn test(&self, params: &Value) -> Result<Value, PluginError> {
        let conn = params.get("connection").cloned().unwrap_or_default();
        let raw = str_field(&conn, &["log_dir"]).ok_or_else(|| PluginError::new(-32602, "Missing log_dir"))?;
        // SSH 模式：建连并校验远端各根可读
        if let Some(sc) = ssh_conf(&conn)? {
            let live = rt().block_on(connect_ssh(&sc))?;
            let dirs = parse_roots(&raw)?;
            let mut total = 0;
            for d in &dirs {
                total += rt().block_on(count_remote_logs(&live, d))?;
            }
            return Ok(json!({ "success": true, "message": format!("SSH 目录可读，共 {total} 个日志文件：{raw}") }));
        }
        let dirs = parse_roots(&raw)?;
        let mut total = 0;
        for d in &dirs {
            total += count_log_files(d)?;
        }
        Ok(json!({ "success": true, "message": format!("目录可读，共 {total} 个日志文件：{raw}") }))
    }

    // connection/connect：校验各根目录并注册会话
    pub(crate) fn connect(&self, params: &Value) -> Result<Value, PluginError> {
        let conn = params.get("connection").cloned().unwrap_or_default();
        let id = conn
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| PluginError::new(-32602, "Missing connection id"))?;
        let raw = str_field(&conn, &["log_dir"]).ok_or_else(|| PluginError::new(-32602, "Missing log_dir"))?;
        let dirs = parse_roots(&raw)?;
        // SSH 模式跳过本地校验，远端可读性由复用会话校验（connect_ssh 后）
        let ssh_pre = ssh_conf(&conn)?;
        if ssh_pre.is_none() {
            for d in &dirs {
                count_log_files(d)?; // 提前暴露目录问题，不等首次 browse 才报错
            }
        }
        // 根短名（basename）必须唯一，否则 browse 首段无法定位
        let mut seen = std::collections::HashSet::new();
        for d in &dirs {
            let n = root_name(d);
            if !seen.insert(n.clone()) {
                return Err(PluginError::new(-32602, format!("根目录重名：{n}")));
            }
        }
        // SSH 模式：建连复用会话（指纹/认证失败直接报错，不注册半会话）
        // 远端各根可读性在此校验（本地 count 已跳过）
        let ssh = ssh_pre;
        if let Some(ref sc) = ssh {
            let live = rt().block_on(connect_ssh(sc))?;
            // 远端各根可读：提前暴露目录问题（test 同口径）
            for d in &dirs {
                rt().block_on(count_remote_logs(&live, d))?;
            }
            self.ssh_lives
                .lock()
                .map_err(|_| PluginError::new(-32000, "Session registry is poisoned"))?
                .insert(id.to_string(), live);
        }
        self.sessions
            .lock()
            .map_err(|_| PluginError::new(-32000, "Session registry is poisoned"))?
            .insert(id.to_string(), Session { log_dirs: dirs, ssh });
        Ok(json!({ "success": true }))
    }

    // connection/disconnect：删会话并停掉该连接的所有 tail 线程
    pub(crate) fn disconnect(&self, params: &Value) -> Result<Value, PluginError> {
        let id = params
            .get("connection")
            .and_then(|c| c.get("id"))
            .and_then(Value::as_str)
            .ok_or_else(|| PluginError::new(-32602, "Missing connection id"))?;
        self.sessions
            .lock()
            .map_err(|_| PluginError::new(-32000, "Session registry is poisoned"))?
            .remove(id);
        // SSH 复用会话随连接释放（SftpSession drop 即关）
        self.ssh_lives
            .lock()
            .map_err(|_| PluginError::new(-32000, "Tail registry is poisoned"))?
            .remove(id);
        // streamId 形如 "<connectionId>:<file>:<seq>"，按前缀停
        let prefix = format!("{id}:");
        let mut tails = self.tails.lock().map_err(|_| PluginError::new(-32000, "Tail registry is poisoned"))?;
        let stale: Vec<String> = tails.keys().filter(|k| k.starts_with(&prefix)).cloned().collect();
        for k in stale {
            if let Some(flag) = tails.remove(&k) {
                flag.store(true, Ordering::Relaxed);
            }
        }
        Ok(json!({ "success": true }))
    }

    pub(crate) fn session(&self, params: &Value) -> Result<Session, PluginError> {
        let id = params.get("connectionId").and_then(Value::as_str).ok_or_else(|| PluginError::new(-32602, "Missing connectionId"))?;
        self.sessions
            .lock()
            .map_err(|_| PluginError::new(-32000, "Session registry is poisoned"))?
            .get(id)
            .cloned()
            .ok_or_else(|| PluginError::new(-32000, "连接未建立：请先连接后再操作"))
    }
}
