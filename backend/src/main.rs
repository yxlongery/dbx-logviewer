use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};
use std::sync::Arc;
use std::time::Duration;

use dbx_plugin_sdk::{PluginEmitter, PluginError, PluginHandler, PluginMetadata, PluginServer, RequestContext};
use serde_json::{json, Value};

// 会话只存非敏感信息：目录列表（逗号分隔多根）。密码/私钥由宿主保管，绝不落内存日志。
#[derive(Clone)]
struct Session {
    log_dirs: Vec<String>,
}

#[derive(Default)]
struct Plugin {
    sessions: Mutex<HashMap<String, Session>>,
    // streamId -> 停止旗标，tail 线程轮询该旗标退出
    tails: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

// 允许的日志扩展名：只展示 *.log 与 nohup.out
const ALLOWED_EXTS: [&str; 2] = ["log", "out"];
// 单次 search 最多返回行数：防 8MB JSON 上限，也防大文件全量进内存
const MAX_RETURN_LINES: usize = 20000;
// 单页上限：截图默认 100，前端可调
const MAX_PAGE_SIZE: usize = 500;
// tail 轮询间隔：本地 inotify 太重，轮询足够且无新依赖
const TAIL_POLL_MS: u64 = 500;

impl PluginHandler for Plugin {
    fn handle(
        &self,
        _context: RequestContext,
        method: &str,
        params: Value,
        emitter: &PluginEmitter,
    ) -> Result<Value, PluginError> {
        match method {
            "connection/test" => self.test(&params),
            "connection/connect" => self.connect(&params),
            "connection/disconnect" => self.disconnect(&params),
            "logs/browse" => self.browse(&params),
            "logs/search" => self.search(&params),
            "logs/tail" => self.tail(&params, emitter),
            "logs/stop" => self.stop(&params),
            "logs/downloadChunk" => self.download_chunk(&params),
            "dbx-logviewer/ping" => Ok(json!({
                "ok": true,
                "plugin": "io.github.yxlonger.logviewer",
                "language": "rust",
            })),
            _ => Err(PluginError::method_not_found(method)),
        }
    }
}

impl Plugin {
    // connection/test：只校验各根目录可读，不建会话
    fn test(&self, params: &Value) -> Result<Value, PluginError> {
        let conn = params.get("connection").cloned().unwrap_or_default();
        let raw = str_field(&conn, &["log_dir"]).ok_or_else(|| PluginError::new(-32602, "Missing log_dir"))?;
        let dirs = parse_roots(&raw)?;
        let mut total = 0;
        for d in &dirs {
            total += count_log_files(d)?;
        }
        Ok(json!({ "success": true, "message": format!("目录可读，共 {total} 个日志文件：{raw}") }))
    }

    // connection/connect：校验各根目录并注册会话
    fn connect(&self, params: &Value) -> Result<Value, PluginError> {
        let conn = params.get("connection").cloned().unwrap_or_default();
        let id = conn
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| PluginError::new(-32602, "Missing connection id"))?;
        let raw = str_field(&conn, &["log_dir"]).ok_or_else(|| PluginError::new(-32602, "Missing log_dir"))?;
        let dirs = parse_roots(&raw)?;
        for d in &dirs {
            count_log_files(d)?; // 提前暴露目录问题，不等首次 browse 才报错
        }
        // 根短名（basename）必须唯一，否则 browse 首段无法定位
        let mut seen = std::collections::HashSet::new();
        for d in &dirs {
            let n = root_name(d);
            if !seen.insert(n.clone()) {
                return Err(PluginError::new(-32602, format!("根目录重名：{n}")));
            }
        }
        self.sessions
            .lock()
            .map_err(|_| PluginError::new(-32000, "Session registry is poisoned"))?
            .insert(id.to_string(), Session { log_dirs: dirs });
        Ok(json!({ "success": true }))
    }

    // connection/disconnect：删会话并停掉该连接的所有 tail 线程
    fn disconnect(&self, params: &Value) -> Result<Value, PluginError> {
        let id = params
            .get("connection")
            .and_then(|c| c.get("id"))
            .and_then(Value::as_str)
            .ok_or_else(|| PluginError::new(-32602, "Missing connection id"))?;
        self.sessions
            .lock()
            .map_err(|_| PluginError::new(-32000, "Session registry is poisoned"))?
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

    // logs/browse：空串列各根短名；非空首段为根短名，余下为该根下相对目录；点选下钻代替手输路径
    fn browse(&self, params: &Value) -> Result<Value, PluginError> {
        let session = self.session(params)?;
        let dir_rel = params.get("dir").and_then(Value::as_str).unwrap_or("").trim().to_string();
        // 空串为根：直接列各根短名，不读盘
        if dir_rel.is_empty() {
            let mut dirs: Vec<Value> = session.log_dirs.iter().map(|d| json!({ "name": root_name(d) })).collect();
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
            let ext_ok = path.extension().and_then(|e| e.to_str())
                .map(|e| ALLOWED_EXTS.contains(&e.to_ascii_lowercase().as_str())).unwrap_or(false);
            if !ext_ok {
                continue;
            }
            // 断裂软链等坏条目：列出但大小时间为 0，不整层失败（点选时 search 报路径不存在）
            let (size, modified_at) = entry.metadata().map(|m| (m.len(), m.modified().ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs()).unwrap_or(0))).unwrap_or((0, 0));
            // 链接目标：普通文件为 null，软链返回目标绝对路径（文本节点展示用）
            let link_target = std::fs::read_link(&path).ok().map(|p| p.to_string_lossy().to_string());
            // 相对路径透给前端：search/tail/download 直接用它，不再拼
            let rel = if dir_rel.is_empty() { name } else { format!("{dir_rel}/{name}") };
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

    // logs/search：逐行流读 + 关键字模糊 + 级别 + 时间范围 + 排序 + 分页
    fn search(&self, params: &Value) -> Result<Value, PluginError> {
        let session = self.session(params)?;
        let file = params.get("file").and_then(Value::as_str).ok_or_else(|| PluginError::new(-32602, "Missing file"))?;
        let keyword = params.get("keyword").and_then(Value::as_str).unwrap_or("").trim().to_string();
        let level = params.get("level").and_then(Value::as_str).unwrap_or("ALL").to_ascii_uppercase();
        let start = params.get("startTime").and_then(Value::as_str).unwrap_or("").to_string();
        let end = params.get("endTime").and_then(Value::as_str).unwrap_or("").to_string();
        let page = params.get("page").and_then(Value::as_u64).unwrap_or(1).max(1) as usize;
        let page_size = (params.get("pageSize").and_then(Value::as_u64).unwrap_or(100) as usize).clamp(1, MAX_PAGE_SIZE);
        let newest_first = params.get("sort").and_then(Value::as_str).unwrap_or("desc") != "asc";
        let use_regex = params.get("regex").and_then(Value::as_bool).unwrap_or(false);
        let context = params.get("context").and_then(Value::as_u64).unwrap_or(0).min(20) as usize;

        // 路径约束：首段根短名 + canonicalize 校验在所属根内
        let path = safe_join(&session, file)?;
        let start_num = if start.trim().is_empty() { None } else { parse_time_num(&start) };
        let end_num = if end.trim().is_empty() { None } else { parse_time_num(&end) };
        // 正则预编译：非法直接报错（前端透出提示），空关键字按全匹配处理
        let compiled = if use_regex && !keyword.is_empty() {
            Some(SimpleRegex::compile(&keyword)
                .map_err(|e| PluginError::new(-32602, format!("正则表达式无效：{e}")))?)
        } else {
            None
        };

        let f = File::open(&path).map_err(|e| PluginError::new(-32000, format!("无法打开文件 {file}：{e}")))?;
        let mut hits: Vec<(usize, String)> = Vec::new();
        let mut truncated = false;
        for (idx, line) in BufReader::new(f).lines().enumerate() {
            let text = match line {
                Ok(t) => t,
                Err(_) => continue, // 坏行跳过：日志常有截断写，不整体失败
            };
            if !match_line(&text, &keyword, &level, start_num, end_num, compiled.as_ref()) {
                continue;
            }
            hits.push((idx + 1, text)); // 行号 1-based，排序后仍指向原文位置
            if hits.len() >= MAX_RETURN_LINES {
                truncated = true;
                break;
            }
        }
        // total 按命中计数；分页先切命中，页内再展开上下文（lines 可能超 pageSize，属预期）
        let total = hits.len();
        if newest_first {
            hits.reverse();
        }
        let start_idx = (page - 1) * page_size;
        let page_hits: Vec<(usize, String)> = hits.into_iter().skip(start_idx).take(page_size).collect();
        let lines: Vec<Value> = if context == 0 || page_hits.is_empty() {
            page_hits.into_iter().map(|(no, text)| json!({ "no": no, "text": text, "match": true })).collect()
        } else {
            expand_context(&path, &page_hits, context, newest_first)
        };
        Ok(json!({ "total": total, "page": page, "pageSize": page_size, "truncated": truncated, "lines": lines }))
    }

    // logs/tail：起后台线程轮询增量，新行经 emitter 事件推送；返回 streamId 供停止
    fn tail(&self, params: &Value, emitter: &PluginEmitter) -> Result<Value, PluginError> {
        let session = self.session(params)?;
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

    // logs/stop：停掉一条 tail 流
    fn stop(&self, params: &Value) -> Result<Value, PluginError> {
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

    // logs/downloadChunk：按行分块返回文本，前端循环拼装后经 fileTransfer 落盘（避开 base64 新依赖）
    fn download_chunk(&self, params: &Value) -> Result<Value, PluginError> {
        let session = self.session(params)?;
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

    fn session(&self, params: &Value) -> Result<Session, PluginError> {
        let id = params.get("connectionId").and_then(Value::as_str).ok_or_else(|| PluginError::new(-32602, "Missing connectionId"))?;
        self.sessions
            .lock()
            .map_err(|_| PluginError::new(-32000, "Session registry is poisoned"))?
            .get(id)
            .cloned()
            .ok_or_else(|| PluginError::new(-32000, "连接未建立：请先连接后再操作"))
    }
}

// connection 负载里取字符串字段：兼容 external_config / config / 顶层三种位置
fn str_field(conn: &Value, keys: &[&str]) -> Option<String> {
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

// 目录下 .log/.out 文件计数，用于 test/connect 提前校验
fn count_log_files(dir: &str) -> Result<usize, PluginError> {
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

// 多根解析：log_dir 逗号分隔（如 "/app/data,/logs"），去空去尾斜杠，至少一根
fn parse_roots(raw: &str) -> Result<Vec<String>, PluginError> {
    let dirs: Vec<String> = raw.split(',').map(|s| s.trim().trim_end_matches('/').to_string())
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
fn root_name(dir: &str) -> String {
    dir.rsplit('/').next().unwrap_or(dir).to_string()
}

// 路径约束：rel 首段为根短名（如 logs/autofeedemby/20260925.log），禁绝对/..；拼接后 canonicalize 校验仍在所属根内
fn safe_join(session: &Session, rel: &str) -> Result<String, PluginError> {
    let file = rel.trim();
    if file.is_empty() || file.starts_with('/') || file.contains('\\') || file.contains("..") {
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

fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

// 轻量正则子集（纯 std，零新依赖）：字面量 . * + ? | ( ) [ ] ^ $ \d \w \s \D \W \S {m,n}
// 回溯解释器：日志行短、无灾难性回溯风险；超长行由 match_line 回退子串匹配
#[derive(Clone, Debug)]
enum Rx {
    Empty,
    Lit(char),
    Dot,
    Class { neg: bool, rs: Vec<(char, char)> },
    Cat(Vec<Rx>),
    Alt(Vec<Rx>),
    Rep(Box<Rx>, usize, usize), // 最少、最多（{m,} 上限截断防爆炸）
    Begin,
    End,
}

#[derive(Clone, Debug)]
struct SimpleRegex {
    root: Rx,
}

impl SimpleRegex {
    fn compile(pat: &str) -> Result<Self, String> {
        let cs: Vec<char> = pat.chars().collect();
        let mut p = RxParser { cs: &cs, pos: 0 };
        let root = p.parse_alt()?;
        if p.pos != cs.len() {
            return Err(format!("位置 {} 处有多余字符", p.pos));
        }
        Ok(SimpleRegex { root })
    }

    // 搜索语义：行内任意位置可作起点（除非 ^ 锚定行首）
    fn is_match(&self, text: &str) -> bool {
        let cs: Vec<char> = text.chars().collect();
        let anchored = matches!(&self.root, Rx::Cat(v) if v.first().map(|n| matches!(n, Rx::Begin)).unwrap_or(false))
            || matches!(&self.root, Rx::Begin);
        if anchored {
            return Self::match_at(&self.root, &cs, 0).iter().any(|&e| e == cs.len() || Self::tail_ok(&self.root, &cs, e));
        }
        // 非锚定：逐个起点试；Alt/Cat 内部的 $ 由解释器按行尾判定
        for start in 0..=cs.len() {
            for e in Self::match_at(&self.root, &cs, start) {
                if e == cs.len() || !Self::needs_end(&self.root) {
                    return true;
                }
            }
        }
        false
    }

    // 根是否要求匹配到行尾（顶层以 End 结尾）：搜索语义下仍需 e==len
    fn needs_end(node: &Rx) -> bool {
        match node {
            Rx::End => true,
            Rx::Cat(v) => v.last().map(Self::needs_end).unwrap_or(false),
            Rx::Alt(v) => !v.is_empty() && v.iter().all(Self::needs_end),
            _ => false,
        }
    }

    // 锚定起点时：结束位置合法 = 行尾，或根不要求到行尾
    fn tail_ok(node: &Rx, cs: &[char], e: usize) -> bool {
        e == cs.len() || !Self::needs_end(node)
    }

    // 返回从 pos 出发所有可能的结束位置（回溯集合）
    fn match_at(node: &Rx, cs: &[char], pos: usize) -> Vec<usize> {
        match node {
            Rx::Empty => vec![pos],
            Rx::Lit(c) => {
                if pos < cs.len() && cs[pos] == *c {
                    vec![pos + 1]
                } else {
                    vec![]
                }
            }
            Rx::Dot => {
                if pos < cs.len() {
                    vec![pos + 1]
                } else {
                    vec![]
                }
            }
            Rx::Class { neg, rs } => {
                if pos < cs.len() {
                    let hit = rs.iter().any(|(a, b)| *a <= cs[pos] && cs[pos] <= *b);
                    if hit != *neg {
                        return vec![pos + 1];
                    }
                }
                vec![]
            }
            Rx::Begin => {
                if pos == 0 {
                    vec![0]
                } else {
                    vec![]
                }
            }
            Rx::End => {
                if pos == cs.len() {
                    vec![pos]
                } else {
                    vec![]
                }
            }
            Rx::Cat(v) => {
                let mut cur = vec![pos];
                for n in v {
                    let mut next = Vec::new();
                    for p in cur {
                        next.extend(Self::match_at(n, cs, p));
                    }
                    cur = next;
                    if cur.is_empty() {
                        break;
                    }
                }
                cur
            }
            Rx::Alt(v) => {
                let mut out = Vec::new();
                for n in v {
                    out.extend(Self::match_at(n, cs, pos));
                }
                out
            }
            Rx::Rep(inner, lo, hi) => {
                // 必需部分 + 可选部分（贪婪：从多到少试，保证回溯完备）
                let mut sets: Vec<Vec<usize>> = vec![vec![pos]];
                for _ in 0..*hi {
                    let prev = sets.last().unwrap().clone();
                    let mut nx = Vec::new();
                    for p in prev {
                        for e in Self::match_at(inner, cs, p) {
                            if e != p && !nx.contains(&e) {
                                nx.push(e); // 空推进直接丢弃，防无限循环
                            }
                        }
                    }
                    if nx.is_empty() {
                        break;
                    }
                    sets.push(nx);
                }
                let mut out = Vec::new();
                // 从 lo 轮开始取：lo==0 含起点本身（sets[0]），lo>=1 只取推进后的位置
                let start_i = (*lo).min(sets.len());
                for i in (start_i..sets.len()).rev() {
                    for e in &sets[i] {
                        if !out.contains(e) {
                            out.push(*e);
                        }
                    }
                }
                out
            }
        }
    }
}

struct RxParser<'a> {
    cs: &'a [char],
    pos: usize,
}

impl<'a> RxParser<'a> {
    fn peek(&self) -> Option<char> {
        self.cs.get(self.pos).copied()
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn parse_alt(&mut self) -> Result<Rx, String> {
        let mut v = vec![self.parse_cat()?];
        while self.eat('|') {
            v.push(self.parse_cat()?);
        }
        Ok(if v.len() == 1 { v.pop().unwrap() } else { Rx::Alt(v) })
    }

    fn parse_cat(&mut self) -> Result<Rx, String> {
        let mut v = Vec::new();
        while let Some(c) = self.peek() {
            if c == ')' || c == '|' {
                break;
            }
            v.push(self.parse_rep()?);
        }
        Ok(match v.len() {
            0 => Rx::Empty,
            1 => v.pop().unwrap(),
            _ => Rx::Cat(v),
        })
    }

    fn parse_rep(&mut self) -> Result<Rx, String> {
        let atom = self.parse_atom()?;
        match self.peek() {
            Some('*') => {
                self.pos += 1;
                Ok(Rx::Rep(Box::new(atom), 0, usize::MAX / 2))
            }
            Some('+') => {
                self.pos += 1;
                Ok(Rx::Rep(Box::new(atom), 1, usize::MAX / 2))
            }
            Some('?') => {
                self.pos += 1;
                Ok(Rx::Rep(Box::new(atom), 0, 1))
            }
            Some('{') => self.parse_brace(atom),
            _ => Ok(atom),
        }
    }

    // {m} {m,} {m,n}：n 缺省与 m 同，{m,} 上限 m+255 防爆炸
    fn parse_brace(&mut self, atom: Rx) -> Result<Rx, String> {
        self.pos += 1; // 吃掉 {
        let m = self.parse_num()?;
        let (lo, hi) = if self.eat(',') {
            match self.parse_num_opt()? {
                Some(n) if n >= m => (m, n),
                Some(_) => return Err("{m,n} 要求 n>=m".to_string()),
                None => (m, m + 255),
            }
        } else {
            (m, m)
        };
        if !self.eat('}') {
            return Err("缺少 }".to_string());
        }
        Ok(Rx::Rep(Box::new(atom), lo, hi))
    }

    fn parse_num(&mut self) -> Result<usize, String> {
        self.parse_num_opt()?.ok_or_else(|| "期望数字".to_string())
    }

    fn parse_num_opt(&mut self) -> Result<Option<usize>, String> {
        let s = self.pos;
        while self.peek().map(|c| c.is_ascii_digit()).unwrap_or(false) {
            self.pos += 1;
        }
        if s == self.pos {
            return Ok(None);
        }
        self.cs[s..self.pos]
            .iter()
            .collect::<String>()
            .parse()
            .map(Some)
            .map_err(|_| "数字过大".to_string())
    }

    fn parse_atom(&mut self) -> Result<Rx, String> {
        match self.peek() {
            None => Err("表达式意外结束".to_string()),
            Some('(') => {
                self.pos += 1;
                let inner = self.parse_alt()?;
                if !self.eat(')') {
                    return Err("缺少 )".to_string());
                }
                Ok(inner)
            }
            Some('[') => self.parse_class(),
            Some('.') => {
                self.pos += 1;
                Ok(Rx::Dot)
            }
            Some('^') => {
                self.pos += 1;
                Ok(Rx::Begin)
            }
            Some('$') => {
                self.pos += 1;
                Ok(Rx::End)
            }
            Some('\\') => {
                self.pos += 1;
                match self.peek() {
                    Some('d') => {
                        self.pos += 1;
                        Ok(Rx::Class { neg: false, rs: vec![('0', '9')] })
                    }
                    Some('D') => {
                        self.pos += 1;
                        Ok(Rx::Class { neg: true, rs: vec![('0', '9')] })
                    }
                    Some('w') => {
                        self.pos += 1;
                        Ok(Rx::Class { neg: false, rs: vec![('0', '9'), ('A', 'Z'), ('a', 'z'), ('_', '_')] })
                    }
                    Some('W') => {
                        self.pos += 1;
                        Ok(Rx::Class { neg: true, rs: vec![('0', '9'), ('A', 'Z'), ('a', 'z'), ('_', '_')] })
                    }
                    Some('s') => {
                        self.pos += 1;
                        Ok(Rx::Class { neg: false, rs: vec![(' ', ' '), ('\t', '\t'), ('\n', '\n'), ('\r', '\r')] })
                    }
                    Some('S') => {
                        self.pos += 1;
                        Ok(Rx::Class { neg: true, rs: vec![(' ', ' '), ('\t', '\t'), ('\n', '\n'), ('\r', '\r')] })
                    }
                    Some(c) => {
                        self.pos += 1;
                        Ok(Rx::Lit(c)) // 未知转义按字面处理：\. \* \\ 等
                    }
                    None => Err("悬空 \\".to_string()),
                }
            }
            Some(c) if "*+?{|".contains(c) => Err(format!("位置 {} 的 {c} 缺少左操作数", self.pos)),
            Some(c) => {
                self.pos += 1;
                Ok(Rx::Lit(c))
            }
        }
    }

    // [...]：支持 ^ 取反、a-z 范围、\] \\ 转义与 \d \w \s
    fn parse_class(&mut self) -> Result<Rx, String> {
        self.pos += 1; // 吃掉 [
        let neg = self.eat('^');
        let mut rs: Vec<(char, char)> = Vec::new();
        let mut pending: Option<char> = None;
        // ] 紧跟 [ 或 [^ 时为字面
        if self.peek() == Some(']') {
            pending = Some(']');
            self.pos += 1;
        }
        loop {
            match self.peek() {
                None => return Err("字符类未闭合".to_string()),
                Some(']') => {
                    self.pos += 1;
                    if let Some(c) = pending.take() {
                        rs.push((c, c));
                    }
                    break;
                }
                Some('\\') => {
                    self.pos += 1;
                    let e = self.peek().ok_or_else(|| "悬空 \\".to_string())?;
                    self.pos += 1;
                    let mapped: Vec<(char, char)> = match e {
                        'd' => vec![('0', '9')],
                        'w' => vec![('0', '9'), ('A', 'Z'), ('a', 'z'), ('_', '_')],
                        's' => vec![(' ', ' '), ('\t', '\t'), ('\n', '\n'), ('\r', '\r')],
                        c => vec![(c, c)],
                    };
                    if let Some(p) = pending.take() {
                        rs.push((p, p));
                    }
                    // 范围判定延后：pending 保持 None，直接并入
                    rs.extend(mapped);
                }
                Some('-') if pending.is_some() => {
                    self.pos += 1;
                    // [a-] 或 [a-<结尾>：- 为字面；否则与后一字符组成范围
                    match self.peek() {
                        Some(']') | None => {
                            let lo = pending.take().unwrap();
                            rs.push((lo, lo));
                            rs.push(('-', '-'));
                        }
                        Some(e) => {
                            self.pos += 1;
                            let lo = pending.take().unwrap();
                            if lo > e {
                                return Err("字符范围倒置".to_string());
                            }
                            rs.push((lo, e));
                        }
                    }
                }
                Some(c) => {
                    self.pos += 1;
                    if let Some(p) = pending.take() {
                        rs.push((p, p));
                    }
                    pending = Some(c);
                }
            }
        }
        Ok(Rx::Class { neg, rs })
    }
}

// 行过滤：关键字（子串或正则）+ 级别包含 + 时间范围；解析不出时间的行：无时间条件保留，有条件跳过
// re 为 Some 时走正则分支（超长行 >100KB 回退子串，防回溯开销）；None 时 keyword 为子串
fn match_line(text: &str, keyword: &str, level: &str, start: Option<u64>, end: Option<u64>, re: Option<&SimpleRegex>) -> bool {
    match re {
        Some(r) => {
            if text.len() > 100_000 {
                if !keyword.is_empty() && !text.contains(keyword) {
                    return false;
                }
            } else if !r.is_match(text) {
                return false;
            }
        }
        None => {
            if !keyword.is_empty() && !text.contains(keyword) {
                return false;
            }
        }
    }
    if level != "ALL" && !text.to_ascii_uppercase().contains(level) {
        return false;
    }
    if start.is_none() && end.is_none() {
        return true;
    }
    match parse_time_num(text) {
        Some(n) => start.map(|s| n >= s).unwrap_or(true) && end.map(|e| n <= e).unwrap_or(true),
        None => false,
    }
}

// 上下文展开：页内命中行前后各取 context 行，第二遍扫文件按窗口取行
// 输出按行号升序（newest_first 则整体反转）；match 标记命中/上下文；坏行跳过致窗口缺行属预期
fn expand_context(path: &str, page_hits: &[(usize, String)], context: usize, newest_first: bool) -> Vec<Value> {
    let hit_nos: HashSet<usize> = page_hits.iter().map(|(no, _)| *no).collect();
    let mut win: HashSet<usize> = HashSet::new();
    for (no, _) in page_hits {
        let lo = no.saturating_sub(context).max(1);
        for n in lo..=no.saturating_add(context) {
            win.insert(n);
        }
    }
    let mut out: Vec<(usize, String, bool)> = Vec::new();
    if let Ok(f) = File::open(path) {
        for (idx, line) in BufReader::new(f).lines().enumerate() {
            let no = idx + 1;
            if win.remove(&no) {
                if let Ok(text) = line {
                    out.push((no, text, hit_nos.contains(&no)));
                }
                if win.is_empty() {
                    break; // 窗口取齐提前收工
                }
            }
        }
    }
    if newest_first {
        out.reverse();
    }
    out.into_iter().map(|(no, text, m)| json!({ "no": no, "text": text, "match": m })).collect()
}

// 秒后时区后缀解析：Z=UTC(偏移0)；±hh[:mm]/±hhmm 为秒级偏移；无后缀/格式不对返回 None（按本地直读）
fn tz_offset_after(b: &[u8], s: &str, mut p: usize) -> Option<i64> {
    if b.get(p) == Some(&b'.') {
        p += 1; // 跳过毫秒 .123
        while p < b.len() && b[p].is_ascii_digit() {
            p += 1;
        }
    }
    if b.get(p) == Some(&b'Z') {
        return Some(0);
    }
    if b.get(p) != Some(&b'+') && b.get(p) != Some(&b'-') {
        return None;
    }
    let neg = b[p] == b'-';
    p += 1;
    if p + 2 > b.len() || !b[p].is_ascii_digit() || !b[p + 1].is_ascii_digit() {
        return None;
    }
    let hh: i64 = s[p..p + 2].parse().ok()?;
    p += 2;
    if b.get(p) == Some(&b':') {
        p += 1; // +08:00 冒号
    }
    let mm: i64 = if p + 2 <= b.len() && b[p].is_ascii_digit() && b[p + 1].is_ascii_digit() {
        s[p..p + 2].parse().ok()?
    } else {
        0
    };
    if hh > 14 || mm > 59 {
        return None; // 非法偏移按无后缀处理
    }
    Some(if neg { -(hh * 3600 + mm * 60) } else { hh * 3600 + mm * 60 })
}

// 公历与 epoch 秒互转（Howard Hinnant 算法，纯算术无时区库依赖；本地口径固定 +8）
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn epoch_secs(y: u64, mo: u64, d: u64, hh: u64, mm: u64, ss: u64) -> i64 {
    days_from_civil(y as i64, mo as i64, d as i64) * 86400 + hh as i64 * 3600 + mm as i64 * 60 + ss as i64
}

// UTC epoch 秒按 +8 换回本地伪数字 yyyymmddhhmmss
fn num_from_epoch(epoch: i64) -> u64 {
    let local = epoch + 28800;
    let days = local.div_euclid(86400);
    let sod = local.rem_euclid(86400);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    if m <= 2 {
        y += 1;
    }
    (y as u64) * 10000000000 + (m as u64) * 100000000 + (d as u64) * 1000000
        + (sod as u64 / 3600) * 10000 + (sod as u64 % 3600 / 60) * 100 + (sod as u64 % 60)
}

// 宽松时间解析：抓行内首个 yyyy-MM-dd HH:mm:ss（分隔符 - 或 /，T 也可），转可比较数字
fn parse_time_num(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    let mut i = 0;
    while i + 10 <= b.len() {
        if b[i].is_ascii_digit() && b[i + 1].is_ascii_digit() && b[i + 2].is_ascii_digit() && b[i + 3].is_ascii_digit()
            && (b[i + 4] == b'-' || b[i + 4] == b'/')
        {
            let sep = b[i + 4];
            let mut j = i + 5;
            let m_start = j;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if j == m_start || j - m_start > 2 || j >= b.len() || b[j] != sep {
                i += 1;
                continue;
            }
            j += 1;
            let d_start = j;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if j == d_start || j - d_start > 2 {
                i += 1;
                continue;
            }
            let m_end = d_start - 1; // 月份区间 [m_start, m_end)
            // 日期后找 HH:mm:ss
            let mut k = j;
            while k + 8 <= b.len() {
                if b[k].is_ascii_digit() && b[k + 1].is_ascii_digit() && b[k + 2] == b':' && b[k + 3].is_ascii_digit()
                    && b[k + 4].is_ascii_digit() && b[k + 5] == b':' && b[k + 6].is_ascii_digit() && b[k + 7].is_ascii_digit()
                {
                    let y: u64 = s[i..i + 4].parse().ok()?;
                    let mo: u64 = s[m_start..m_end].parse().ok()?;
                    let d: u64 = s[d_start..j].parse().ok()?;
                    let hh: u64 = s[k..k + 2].parse().ok()?;
                    let mm: u64 = s[k + 3..k + 5].parse().ok()?;
                    let ss: u64 = s[k + 6..k + 8].parse().ok()?;
                    let base = y * 10000000000 + mo * 100000000 + d * 1000000 + hh * 10000 + mm * 100 + ss;
                    // 秒后跟 Z 或 ±hh[:mm] 时区后缀：按偏移换算到本地（+8）再比；无后缀按本地直读
                    return Some(match tz_offset_after(b, s, k + 8) {
                        Some(off) => num_from_epoch(epoch_secs(y, mo, d, hh, mm, ss) - off),
                        None => base,
                    });
                }
                k += 1;
                if k > j + 30 {
                    break; // 时间与日期离太远就放弃，避免误抓行尾数字
                }
            }
            i = j;
        } else {
            i += 1;
        }
    }
    None
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

fn main() -> std::io::Result<()> {
    let metadata = PluginMetadata::new("io.github.yxlonger.logviewer", env!("CARGO_PKG_VERSION")).with_capability("connections");
    PluginServer::new(metadata, Plugin::default()).serve()
}

// 纯函数单元测试：时间解析 / 行过滤 / 路径防护是搜索正确性的根基
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_time_supports_dash_slash_and_t() {
        assert_eq!(parse_time_num("2026-08-30 11:04:20 INFO ok"), Some(20260830110420));
        assert_eq!(parse_time_num("2026/8/30 11:04:20 INFO ok"), Some(20260830110420));
        // Z=UTC：按 +8 换算到本地，11:04Z -> 19:04 本地
        assert_eq!(parse_time_num("2026-08-30T11:04:20Z INFO ok"), Some(20260830190420));
        // 毫秒 + Z 同理
        assert_eq!(parse_time_num("timestamp=2026-09-25T14:31:29.250Z level=INFO"), Some(20260925223129));
        // 显式偏移换算到 +8：+00:00 与 Z 同值，+08:00 等于本地直读
        assert_eq!(parse_time_num("2026-08-30T11:04:20+00:00 x"), Some(20260830190420));
        assert_eq!(parse_time_num("2026-08-30T11:04:20+0800 x"), Some(20260830110420));
        assert_eq!(parse_time_num("2026-08-30 19:04:20+08 x"), Some(20260830190420));
        // 跨天进位：03:04Z -> 本地 11:04 同日；23:04Z+1天验证借位在 num_from_epoch 内
        assert_eq!(parse_time_num("2026-08-30T23:04:20Z x"), Some(20260831070420));
        assert_eq!(parse_time_num("no timestamp here"), None);
    }

    #[test]
    fn match_line_combines_keyword_level_and_range() {
        let line = "2026-08-30 11:04:20 ERROR aiban-file boom";
        assert!(match_line(line, "aiban", "ERROR", None, None, None));
        assert!(!match_line(line, "aiban", "WARN", None, None, None)); // 级别不符
        assert!(!match_line(line, "other", "ALL", None, None, None)); // 关键字不符
        assert!(match_line(line, "", "ALL", Some(20260830110000), Some(20260830110500), None));
        assert!(!match_line(line, "", "ALL", Some(20260830120000), None, None)); // 时间下限之外
        // 无时间戳的行：无时间条件保留，有条件跳过
        assert!(match_line("plain line", "", "ALL", None, None, None));
        assert!(!match_line("plain line", "", "ALL", Some(20260830110000), None, None));
    }

    #[test]
    fn regex_subset_matches_common_patterns() {
        let ok = |pat: &str, text: &str| {
            let r = SimpleRegex::compile(pat).expect("用例正则应合法");
            assert!(r.is_match(text), "应命中：{pat} vs {text}");
        };
        let no = |pat: &str, text: &str| {
            let r = SimpleRegex::compile(pat).expect("用例正则应合法");
            assert!(!r.is_match(text), "不应命中：{pat} vs {text}");
        };
        ok("ERROR", "2026-08-30 ERROR boom");
        ok("ERR.*boom", "ERROR big boom");
        no("ERR.*boom", "ERROR silence");
        ok("timeout:\\s*\\d+ms", "timeout:   42ms");
        no("timeout:\\s*\\d+ms", "timeout: slow");
        ok("a.c", "axc");
        no("a.c", "ac");
        ok("^2026-08-30", "2026-08-30 INFO ok");
        no("^INFO", "2026-08-30 INFO ok");
        ok("ok$", "all ok");
        no("ok$", "ok then more");
        ok("WARN|ERROR", "WARN here");
        ok("(WARN|ERROR) here", "ERROR here");
        ok("[0-9]{4}-[0-9]{2}", "on 2026-08 done");
        ok("\\d{4}", "year 2026");
        ok("[^0-9]+", "abc");
        no("[^0-9]+", "123");
        ok("colou?r", "color");
        ok("colou?r", "colour");
        // 非法表达式直接报错（前端透出提示）
        assert!(SimpleRegex::compile("a(b").is_err()); // 缺 )
        assert!(SimpleRegex::compile("*a").is_err()); // 缺左操作数
        assert!(SimpleRegex::compile("[z-a]").is_err()); // 范围倒置
        // 正则分支进 match_line：大小写敏感，超长行回退子串
        let r = SimpleRegex::compile("ERR.*boom").unwrap();
        assert!(match_line("ERROR big boom", "ERR.*boom", "ALL", None, None, Some(&r)));
        assert!(!match_line("error big boom", "ERR.*boom", "ALL", None, None, Some(&r)));
        let long = "x".repeat(100_001);
        assert!(match_line(&format!("{long}ERR"), "ERR", "ALL", None, None, Some(&r))); // 回退子串命中
        assert!(!match_line(&long, "ERR", "ALL", None, None, Some(&r)));
    }

    #[test]
    fn search_context_expands_window_with_match_flag() {
        let base = std::env::temp_dir().join("dbx-logviewer-context");
        std::fs::create_dir_all(&base).unwrap();
        let content = (1..=10).map(|i| {
            if i == 5 { "2026-08-30 11:04:20 ERROR boom\n".to_string() }
            else { format!("2026-08-30 11:04:{:02} INFO filler {i}\n", i) }
        }).collect::<String>();
        std::fs::write(base.join("c.log"), &content).unwrap();
        let plugin = Plugin::default();
        let root = root_name(base.to_str().unwrap());
        plugin.sessions.lock().unwrap().insert("ctx-conn".to_string(), test_session(vec![base.to_str().unwrap().to_string()]));
        let file = format!("{root}/c.log");
        // context=2：命中行 5，前后各 2 行，共 3..7
        let r = plugin.search(&serde_json::json!({ "connectionId": "ctx-conn",
            "file": file, "keyword": "boom", "context": 2, "page": 1, "pageSize": 10 })).expect("上下文搜索应成功");
        assert_eq!(r["total"], 1); // total 按命中计数
        let lines = r["lines"].as_array().unwrap();
        let nos: Vec<u64> = lines.iter().map(|l| l["no"].as_u64().unwrap()).collect();
        assert_eq!(nos, vec![7, 6, 5, 4, 3]); // 默认倒序
        assert_eq!(lines.iter().filter(|l| l["match"] == true).count(), 1);
        assert!(lines.iter().find(|l| l["no"] == 5).unwrap()["match"] == true);
        // context=0：只返回命中行，无 match=false
        let r0 = plugin.search(&serde_json::json!({ "connectionId": "ctx-conn",
            "file": file, "keyword": "boom", "context": 0, "page": 1, "pageSize": 10 })).expect("零上下文应成功");
        assert_eq!(r0["lines"].as_array().unwrap().len(), 1);
        // 顶部边界：命中行 1，窗口 clamp 到文件头
        let r1 = plugin.search(&serde_json::json!({ "connectionId": "ctx-conn",
            "file": file, "keyword": "filler 1", "context": 5, "page": 1, "pageSize": 10 })).expect("边界应成功");
        let nos1: Vec<u64> = r1["lines"].as_array().unwrap().iter().map(|l| l["no"].as_u64().unwrap()).collect();
        assert!(nos1.contains(&1) && !nos1.contains(&0));
        std::fs::remove_dir_all(&base).ok(); // 测试收尾清理临时目录
    }

    // 会话构造器：测试用单根/多根快速建 Session
    fn test_session(dirs: Vec<String>) -> Session {
        Session { log_dirs: dirs }
    }

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

    // 建链相关逻辑已下线（目录浏览代替手输路径，删除改为界面隐藏）：旧测试整组移除
}
