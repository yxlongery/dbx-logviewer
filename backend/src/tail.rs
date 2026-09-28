use crate::path::safe_join;
use crate::regex::SimpleRegex;
use crate::session::SshLive;
use crate::ssh::remote_resolve;
use crate::now_millis;
use crate::search::match_line;
use crate::session::rt;
use crate::ssh::{remote_len, ssh_live_of};
use crate::TAIL_POLL_MS;
use dbx_plugin_sdk::{PluginEmitter, PluginError};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use std::fs::File;
use std::io::{Seek, SeekFrom};

// 实时监控：后台线程轮询增量推送；本地 500ms，SSH 复用长连接 2s

impl crate::Plugin {
    // logs/tail：起后台线程轮询增量，新行经 emitter 事件推送；返回 streamId 供停止
    pub(crate) fn tail(&self, params: &Value, emitter: &PluginEmitter) -> Result<Value, PluginError> {
        let session = self.session(params)?;
        // SSH 模式走复用长连接
        if session.ssh.is_some() {
            return self.tail_ssh(params, emitter);
        }
        let file = params.get("file").and_then(Value::as_str).ok_or_else(|| PluginError::new(-32602, "Missing file"))?;
        let path = safe_join(&session, file)?;
        let keyword = params.get("keyword").and_then(Value::as_str).unwrap_or("").trim().to_string();
        let level = params.get("level").and_then(Value::as_str).unwrap_or("ALL").to_ascii_uppercase();
        let last_n = params.get("lastN").and_then(Value::as_u64).unwrap_or(200).min(1000) as usize;
        let use_regex = params.get("regex").and_then(Value::as_bool).unwrap_or(false);
        let compiled = if use_regex && !keyword.is_empty() {
            Some(SimpleRegex::compile(&keyword)
                .map_err(|e| PluginError::new(-32602, format!("正则表达式无效：{e}")))?)
        } else {
            None
        };

        let connection_id = params.get("connectionId").and_then(Value::as_str).unwrap_or("local");
        let stream_id = format!("{connection_id}:{file}:{}", now_millis());
        let stop = Arc::new(AtomicBool::new(false));
        self.tails
            .lock()
            .map_err(|_| PluginError::new(-32000, "Tail registry is poisoned"))?
            .insert(stream_id.clone(), stop.clone());

        let emitter = emitter.clone();
        let sid = stream_id.clone();
        let fname = file.to_string();
        std::thread::spawn(move || {
            // 先推末 lastN 行：打开即有内容，不白屏
            let mut offset = tail_from_end(&path, &sid, &emitter, &keyword, &level, last_n, &fname, compiled.as_ref());
            // 轮询增量：长度变小视为 rotate，从头重读
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(TAIL_POLL_MS));
                match std::fs::metadata(&path).map(|m| m.len()) {
                    Ok(len) if len < offset => offset = 0,
                    Ok(_) => {}
                    Err(_) => continue, // 文件短暂不可读（如 rotate 中），下轮再试
                }
                offset = read_new_lines(&path, offset, &sid, &emitter, &keyword, &level, &fname, compiled.as_ref());
            }
        });
        Ok(json!({ "streamId": stream_id }))
    }

    // logs/tail SSH 版：复用 SFTP 长连接读增量，轮询 2s（建连开销大，不用 500ms）
    pub(crate) fn tail_ssh(&self, params: &Value, emitter: &PluginEmitter) -> Result<Value, PluginError> {
        let session = self.session(params)?;
        let live = ssh_live_of(self, params, &session)?;
        let file = params.get("file").and_then(Value::as_str).ok_or_else(|| PluginError::new(-32602, "Missing file"))?;
        let path = rt().block_on(remote_resolve(&live, &session, file))?;
        let keyword = params.get("keyword").and_then(Value::as_str).unwrap_or("").trim().to_string();
        let level = params.get("level").and_then(Value::as_str).unwrap_or("ALL").to_ascii_uppercase();
        let last_n = params.get("lastN").and_then(Value::as_u64).unwrap_or(200).min(1000) as usize;
        let use_regex = params.get("regex").and_then(Value::as_bool).unwrap_or(false);
        let compiled = if use_regex && !keyword.is_empty() {
            Some(SimpleRegex::compile(&keyword)
                .map_err(|e| PluginError::new(-32602, format!("正则表达式无效：{e}")))?)
        } else {
            None
        };
        let connection_id = params.get("connectionId").and_then(Value::as_str).unwrap_or("local");
        let stream_id = format!("{connection_id}:{file}:{}", now_millis());
        let stop = Arc::new(AtomicBool::new(false));
        self.tails
            .lock()
            .map_err(|_| PluginError::new(-32000, "Tail registry is poisoned"))?
            .insert(stream_id.clone(), stop.clone());
        let emitter = emitter.clone();
        let sid = stream_id.clone();
        let fname = file.to_string();
        std::thread::spawn(move || {
            let mut offset = rt().block_on(tail_from_end_ssh(&live, &path, &sid, &emitter, &keyword, &level, last_n, &fname, compiled.as_ref()));
            // 轮询增量 2s：长度变小视为 rotate，从头重读
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(2000));
                let len = rt().block_on(remote_len(&live, &path)).unwrap_or(offset);
                if len < offset {
                    offset = 0;
                }
                offset = rt().block_on(read_new_ssh(&live, &path, offset, &sid, &emitter, &keyword, &level, &fname, compiled.as_ref()));
            }
        });
        Ok(json!({ "streamId": stream_id }))
    }

    // logs/stop：停掉一条 tail 流
    pub(crate) fn stop(&self, params: &Value) -> Result<Value, PluginError> {
        let sid = params.get("streamId").and_then(Value::as_str).ok_or_else(|| PluginError::new(-32602, "Missing streamId"))?;
        let removed = self
            .tails
            .lock()
            .map_err(|_| PluginError::new(-32000, "Tail registry is poisoned"))?
            .remove(sid);
        if let Some(flag) = removed {
            flag.store(true, Ordering::Relaxed);
        }
        Ok(json!({ "success": true }))
    }
}

async fn tail_from_end_ssh(live: &SshLive, path: &str, sid: &str, emitter: &PluginEmitter, keyword: &str, level: &str, last_n: usize, fname: &str, re: Option<&SimpleRegex>) -> u64 {
    use tokio::io::AsyncSeekExt;
    let len = remote_len(live, path).await.unwrap_or(0);
    if last_n == 0 {
        return len;
    }
    // 读末 256KB 兜底：lastN 行一般落在此区间
    let probe = len.min(256 * 1024);
    let mut lines: Vec<String> = Vec::new();
    if let Ok(mut f) = live.sftp.open(path).await {
        if f.seek(std::io::SeekFrom::Start(len - probe)).await.is_ok() {
            let mut buf = vec![0u8; probe as usize];
            if tokio::io::AsyncReadExt::read_exact(&mut f, &mut buf).await.is_ok() {
                let text = String::from_utf8_lossy(&buf);
                let mut all: Vec<&str> = text.lines().collect();
                if probe < len {
                    all.remove(0); // 首行可能被截半，丢弃
                }
                let total = all.len();
                let from = total.saturating_sub(last_n);
                for t in &all[from..] {
                    if match_line(t, keyword, level, None, None, re) {
                        lines.push(t.to_string());
                    }
                }
            }
        }
    }
    if !lines.is_empty() {
        let _ = emitter.event("logs/append", json!({ "streamId": sid, "file": fname, "lines": lines, "reset": true }));
    }
    len
}

// 远端增量读 [offset, len) 新行并推送，返回新 offset（块末半行回退下轮重读）
async fn read_new_ssh(live: &SshLive, path: &str, offset: u64, sid: &str, emitter: &PluginEmitter, keyword: &str, level: &str, fname: &str, re: Option<&SimpleRegex>) -> u64 {
    use tokio::io::AsyncSeekExt;
    let len = match remote_len(live, path).await {
        Ok(l) => l,
        Err(_) => return offset,
    };
    if len <= offset {
        return offset;
    }
    // 单轮最多 1MB：写入 burst 时分多轮推，不阻塞停止旗标
    let take = (len - offset).min(1024 * 1024);
    let mut buf = vec![0u8; take as usize];
    let mut f = match live.sftp.open(path).await {
        Ok(f) => f,
        Err(_) => return offset,
    };
    if f.seek(std::io::SeekFrom::Start(offset)).await.is_err() {
        return offset;
    }
    if tokio::io::AsyncReadExt::read_exact(&mut f, &mut buf).await.is_err() {
        return offset;
    }
    let new_off = offset + take;
    let text = String::from_utf8_lossy(&buf);
    let raw: Vec<&str> = text.lines().collect();
    let complete = text.ends_with('\n');
    let usable = if complete { raw.len() } else { raw.len().saturating_sub(1) };
    let backtrack: u64 = if complete { 0 } else { raw.last().map(|l| l.len() as u64 + 1).unwrap_or(0) };
    let send: Vec<String> = raw[..usable].iter().map(|s| s.to_string()).filter(|t| match_line(t, keyword, level, None, None, re)).collect();
    if !send.is_empty() {
        let _ = emitter.event("logs/append", json!({ "streamId": sid, "file": fname, "lines": send }));
    }
    new_off - backtrack
}

// tail 启动时先推末 lastN 行，返回当前文件长度作为增量起点

fn tail_from_end(path: &str, sid: &str, emitter: &PluginEmitter, keyword: &str, level: &str, last_n: usize, fname: &str, re: Option<&SimpleRegex>) -> u64 {
    let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if last_n == 0 {
        return len;
    }
    // 读末 256KB 兜底：lastN 行一般落在此区间，超了也只推最近部分
    let probe = len.min(256 * 1024);
    let mut buf = vec![0u8; probe as usize];
    let mut lines: Vec<String> = Vec::new();
    if let Ok(mut f) = File::open(path) {
        if f.seek(SeekFrom::Start(len - probe)).is_ok() && std::io::Read::read_exact(&mut f, &mut buf).is_ok() {
            let text = String::from_utf8_lossy(&buf);
            let mut all: Vec<&str> = text.lines().collect();
            if probe < len {
                all.remove(0); // 首行可能被截半，丢弃
            }
            let total = all.len();
            let from = total.saturating_sub(last_n);
            for t in &all[from..] {
                if match_line(t, keyword, level, None, None, re) {
                    lines.push(t.to_string());
                }
            }
        }
    }
    if !lines.is_empty() {
        let _ = emitter.event("logs/append", json!({ "streamId": sid, "file": fname, "lines": lines, "reset": true }));
    }
    len
}

// 增量读 [offset, len) 区间的新行并推送，返回新 offset
fn read_new_lines(path: &str, offset: u64, sid: &str, emitter: &PluginEmitter, keyword: &str, level: &str, fname: &str, re: Option<&SimpleRegex>) -> u64 {
    let len = match std::fs::metadata(path).map(|m| m.len()) {
        Ok(l) => l,
        Err(_) => return offset,
    };
    if len <= offset {
        return offset;
    }
    // 单轮最多 1MB：写入 burst 时分多轮推，不阻塞停止旗标
    let take = (len - offset).min(1024 * 1024);
    let mut buf = vec![0u8; take as usize];
    let mut f = match File::open(path) {
        Ok(f) => f,
        Err(_) => return offset,
    };
    if f.seek(SeekFrom::Start(offset)).is_err() {
        return offset;
    }
    if std::io::Read::read_exact(&mut f, &mut buf).is_err() {
        return offset;
    }
    let new_off = offset + take;
    let text = String::from_utf8_lossy(&buf);
    // 块末行可能被切半：complete 为 false 时末行回退字节，下轮重读整行
    let raw: Vec<&str> = text.lines().collect();
    let complete = text.ends_with('\n');
    let usable = if complete { raw.len() } else { raw.len().saturating_sub(1) };
    let backtrack: u64 = if complete { 0 } else { raw.last().map(|l| l.len() as u64 + 1).unwrap_or(0) };
    let send: Vec<String> = raw[..usable].iter().map(|s| s.to_string()).filter(|t| match_line(t, keyword, level, None, None, re)).collect();
    if !send.is_empty() {
        let _ = emitter.event("logs/append", json!({ "streamId": sid, "file": fname, "lines": send }));
    }
    new_off - backtrack
}

// logs/downloadChunk 远端版：SFTP 流读过滤 + 按行跳过/截取（语义与本地一致）
