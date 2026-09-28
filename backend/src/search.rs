use crate::path::safe_join;
use crate::regex::SimpleRegex;
use crate::session::{rt, Session, SshLive};
use crate::ssh::{remote_resolve, ssh_live_of};
use dbx_plugin_sdk::PluginError;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader};

// 搜索：关键字/级别/时间过滤 + 分页 + 上下文；SSH 版两遍扫语义一致

impl crate::Plugin {
// logs/search：逐行流读 + 关键字模糊 + 级别 + 时间范围 + 排序 + 分页
    pub(crate) fn search(&self, params: &Value) -> Result<Value, PluginError> {
        let session = self.session(params)?;
        // SSH 模式走 SFTP 两遍扫（复用会话）
        if session.ssh.is_some() {
            let live = ssh_live_of(self, params, &session)?;
            return rt().block_on(search_ssh(&live, &session, params));
        }
        let file = params.get("file").and_then(Value::as_str).ok_or_else(|| PluginError::new(-32602, "Missing file"))?;
        let keyword = params.get("keyword").and_then(Value::as_str).unwrap_or("").trim().to_string();
        let level = params.get("level").and_then(Value::as_str).unwrap_or("ALL").to_ascii_uppercase();
        let start = params.get("startTime").and_then(Value::as_str).unwrap_or("").to_string();
        let end = params.get("endTime").and_then(Value::as_str).unwrap_or("").to_string();
        let page = params.get("page").and_then(Value::as_u64).unwrap_or(1).max(1) as usize;
        let page_size = (params.get("pageSize").and_then(Value::as_u64).unwrap_or(100) as usize).clamp(1, crate::MAX_PAGE_SIZE);
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
            if hits.len() >= crate::MAX_RETURN_LINES {
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
}

// 轻量正则子集（纯 std，零新依赖）：字面量 . * + ? | ( ) [ ] ^ $ \d \w \s \D \W \S {m,n}
// 回溯解释器：日志行短、无灾难性回溯风险；超长行由 match_line 回退子串匹配
// re 为 Some 时走正则分支（超长行 >100KB 回退子串，防回溯开销）；None 时 keyword 为子串
pub(crate) fn match_line(text: &str, keyword: &str, level: &str, start: Option<u64>, end: Option<u64>, re: Option<&SimpleRegex>) -> bool {
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

// 远端文件长度（SFTP metadata size，缺失按 0）

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::test_session;
    use crate::{path::root_name, regex::SimpleRegex, Plugin};
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
}

// logs/search 远端版：SFTP 两遍扫（命中收集→分页→窗口重扫），过滤/分页语义与本地一致
async fn search_ssh(live: &SshLive, session: &Session, params: &Value) -> Result<Value, PluginError> {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let file = params.get("file").and_then(Value::as_str).ok_or_else(|| PluginError::new(-32602, "Missing file"))?;
    let keyword = params.get("keyword").and_then(Value::as_str).unwrap_or("").trim().to_string();
    let level = params.get("level").and_then(Value::as_str).unwrap_or("ALL").to_ascii_uppercase();
    let start = params.get("startTime").and_then(Value::as_str).unwrap_or("").to_string();
    let end = params.get("endTime").and_then(Value::as_str).unwrap_or("").to_string();
    let page = params.get("page").and_then(Value::as_u64).unwrap_or(1).max(1) as usize;
    let page_size = (params.get("pageSize").and_then(Value::as_u64).unwrap_or(100) as usize).clamp(1, crate::MAX_PAGE_SIZE);
    let newest_first = params.get("sort").and_then(Value::as_str).unwrap_or("desc") != "asc";
    let use_regex = params.get("regex").and_then(Value::as_bool).unwrap_or(false);
    let context = params.get("context").and_then(Value::as_u64).unwrap_or(0).min(20) as usize;
    let path = remote_resolve(live, session, file).await?;
    let start_num = if start.trim().is_empty() { None } else { parse_time_num(&start) };
    let end_num = if end.trim().is_empty() { None } else { parse_time_num(&end) };
    let compiled = if use_regex && !keyword.is_empty() {
        Some(SimpleRegex::compile(&keyword)
            .map_err(|e| PluginError::new(-32602, format!("正则表达式无效：{e}")))?)
    } else {
        None
    };
    let e = |m: String| PluginError::new(-32000, m);
    // 第一遍：收集命中（行号+文本，上限截断同本地）
    let f = live.sftp.open(&path).await.map_err(|er| e(format!("无法打开文件 {file}：{er:?}")))?;
    let mut hits: Vec<(usize, String)> = Vec::new();
    let mut truncated = false;
    let mut lines = BufReader::new(f).lines();
    let mut no = 0;
    while let Some(text) = lines.next_line().await.map_err(|er| e(format!("读取文件失败 {file}：{er:?}")))? {
        no += 1;
        if !match_line(&text, &keyword, &level, start_num, end_num, compiled.as_ref()) {
            continue;
        }
        hits.push((no, text));
        if hits.len() >= crate::MAX_RETURN_LINES {
            truncated = true;
            break;
        }
    }
    let total = hits.len();
    if newest_first {
        hits.reverse();
    }
    let start_idx = (page - 1) * page_size;
    let page_hits: Vec<(usize, String)> = hits.into_iter().skip(start_idx).take(page_size).collect();
    // 第二遍：无上下文直接返回；有则窗口重扫（坏行跳过致缺行属预期）
    let lines_out: Vec<Value> = if context == 0 || page_hits.is_empty() {
        page_hits.into_iter().map(|(no, text)| json!({ "no": no, "text": text, "match": true })).collect()
    } else {
        let hit_nos: HashSet<usize> = page_hits.iter().map(|(n, _)| *n).collect();
        let mut win: HashSet<usize> = HashSet::new();
        for (n, _) in &page_hits {
            for w in n.saturating_sub(context).max(1)..=n.saturating_add(context) {
                win.insert(w);
            }
        }
        let f2 = live.sftp.open(&path).await.map_err(|er| e(format!("无法打开文件 {file}：{er:?}")))?;
        let mut out: Vec<(usize, String, bool)> = Vec::new();
        let mut lines2 = BufReader::new(f2).lines();
        let mut no2 = 0;
        while let Some(text) = lines2.next_line().await.map_err(|er| e(format!("读取文件失败 {file}：{er:?}")))? {
            no2 += 1;
            if win.remove(&no2) {
                out.push((no2, text, hit_nos.contains(&no2)));
                if win.is_empty() {
                    break;
                }
            }
        }
        if newest_first {
            out.reverse();
        }
        out.into_iter().map(|(n, t, m)| json!({ "no": n, "text": t, "match": m })).collect()
    };
    Ok(json!({ "total": total, "page": page, "pageSize": page_size, "truncated": truncated, "lines": lines_out }))
}

