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
pub(crate) struct SimpleRegex {
    root: Rx,
}

impl SimpleRegex {
    pub(crate) fn compile(pat: &str) -> Result<Self, String> {
        let cs: Vec<char> = pat.chars().collect();
        let mut p = RxParser { cs: &cs, pos: 0 };
        let root = p.parse_alt()?;
        if p.pos != cs.len() {
            return Err(format!("位置 {} 处有多余字符", p.pos));
        }
        Ok(SimpleRegex { root })
    }

    // 搜索语义：行内任意位置可作起点（除非 ^ 锚定行首）
    pub(crate) fn is_match(&self, text: &str) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;
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
        // 正则分支进 match_line 的断言随 match_line 搬 search 模块（批2补回）
    }

}
