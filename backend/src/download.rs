use crate::log_file::LogFile;
use crate::regex::SimpleRegex;
use crate::search::match_line;
use dbx_plugin_sdk::PluginError;
use serde_json::{json, Value};

// 分块下载：按行跳过/截取，前端拼装后经 fileTransfer 落盘

impl crate::Plugin {
    // logs/downloadChunk：按行分块返回文本，前端循环拼装后经 fileTransfer 落盘（避开 base64 新依赖）
    pub(crate) fn download_chunk(&self, params: &Value) -> Result<Value, PluginError> {
        let session = self.session(params)?;
        let file = LogFile::resolve(self, &session, params)?;
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

        let mut reader = file.open()?;
        let mut lines = Vec::new();
        let mut skipped = 0;
        let mut eof = true;
        while let Some((_, text)) = reader.next_line()? {
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
