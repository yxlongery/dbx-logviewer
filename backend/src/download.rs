use crate::path::safe_join;
use crate::regex::SimpleRegex;
use crate::search::match_line;
use crate::session::{rt, Session, SshLive};
use crate::ssh::remote_resolve;
use crate::ssh::ssh_live_of;
use dbx_plugin_sdk::PluginError;
use serde_json::{json, Value};
use std::fs::File;
use std::io::{BufRead, BufReader};

// 分块下载：按行跳过/截取，前端拼装后经 fileTransfer 落盘

impl crate::Plugin {
    // logs/downloadChunk：按行分块返回文本，前端循环拼装后经 fileTransfer 落盘（避开 base64 新依赖）
    pub(crate) fn download_chunk(&self, params: &Value) -> Result<Value, PluginError> {
        let session = self.session(params)?;
        // SSH 模式走 SFTP 流读分块
        if session.ssh.is_some() {
            let live = ssh_live_of(self, params, &session)?;
            return rt().block_on(download_chunk_ssh(&live, &session, params));
        }
        let file = params.get("file").and_then(Value::as_str).ok_or_else(|| PluginError::new(-32602, "Missing file"))?;
        let offset = params.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
        let length = (params.get("length").and_then(Value::as_u64).unwrap_or(2000) as usize).clamp(1, 5000);
        let keyword = params.get("keyword").and_then(Value::as_str).unwrap_or("").trim().to_string();
        let level = params.get("level").and_then(Value::as_str).unwrap_or("ALL").to_ascii_uppercase();
        let use_regex = params.get("regex").and_then(Value::as_bool).unwrap_or(false);
        let compiled = if use_regex && !keyword.is_empty() {
            Some(SimpleRegex::compile(&keyword)
                .map_err(|e| PluginError::new(-32602, format!("正则表达式无效：{e}")))?)
        } else {
            None
        };

        let path = safe_join(&session, file)?;
        let f = File::open(&path).map_err(|e| PluginError::new(-32000, format!("无法打开文件 {file}：{e}")))?;
        let mut lines = Vec::new();
        let mut skipped = 0;
        let mut eof = true;
        for line in BufReader::new(f).lines() {
            let text = match line {
                Ok(t) => t,
                Err(_) => continue,
            };
            if !match_line(&text, &keyword, &level, None, None, compiled.as_ref()) {
                continue;
            }
            if skipped < offset {
                skipped += 1;
                continue;
            }
            lines.push(text);
            if lines.len() >= length {
                eof = false; // 还有下一块
                break;
            }
        }
        Ok(json!({ "lines": lines, "nextOffset": offset + lines.len(), "eof": eof }))
    }
}


// logs/downloadChunk 远端版：SFTP 流读过滤 + 按行跳过/截取（语义与本地一致）
async fn download_chunk_ssh(live: &SshLive, session: &Session, params: &Value) -> Result<Value, PluginError> {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let file = params.get("file").and_then(Value::as_str).ok_or_else(|| PluginError::new(-32602, "Missing file"))?;
    let offset = params.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
    let length = (params.get("length").and_then(Value::as_u64).unwrap_or(2000) as usize).clamp(1, 5000);
    let keyword = params.get("keyword").and_then(Value::as_str).unwrap_or("").trim().to_string();
    let level = params.get("level").and_then(Value::as_str).unwrap_or("ALL").to_ascii_uppercase();
    let use_regex = params.get("regex").and_then(Value::as_bool).unwrap_or(false);
    let compiled = if use_regex && !keyword.is_empty() {
        Some(SimpleRegex::compile(&keyword)
            .map_err(|e| PluginError::new(-32602, format!("正则表达式无效：{e}")))?)
    } else {
        None
    };
    let path = remote_resolve(live, session, file).await?;
    let e = |m: String| PluginError::new(-32000, m);
    let f = live.sftp.open(&path).await.map_err(|er| e(format!("无法打开文件 {file}：{er:?}")))?;
    let mut lines_out = Vec::new();
    let mut skipped = 0;
    let mut eof = true;
    let mut lines = BufReader::new(f).lines();
    while let Some(text) = lines.next_line().await.map_err(|er| e(format!("读取文件失败 {file}：{er:?}")))? {
        if !match_line(&text, &keyword, &level, None, None, compiled.as_ref()) {
            continue;
        }
        if skipped < offset {
            skipped += 1;
            continue;
        }
        lines_out.push(text);
        if lines_out.len() >= length {
            eof = false;
            break;
        }
    }
    Ok(json!({ "lines": lines_out, "nextOffset": offset + lines_out.len(), "eof": eof }))
}
