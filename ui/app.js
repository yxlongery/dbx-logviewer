const $ = id => document.querySelector("#" + id);
const state = { dirs: [], files: [], dir: "", file: null, page: 1, total: 0, streamId: null, tailing: false, connId: null, hidden: [], fileFilter: "", fileSort: "time" };
// HTML 转义：日志原文直接插 innerHTML 会执行标签，先转义再高亮
const esc = s => s.replace(/&/g,"&amp;").replace(/</g,"&lt;").replace(/>/g,"&gt;");
const fmtSize = n => n > 1048576 ? (n/1048576).toFixed(2)+" MB" : n > 1024 ? (n/1024).toFixed(2)+" KB" : n+" B";
const fmtTime = t => t ? new Date(t*1000).toLocaleString("zh-CN",{hour12:false}) : "-";
// datetime-local(2026-08-30T11:04)→后端宽松解析(2026-08-30 11:04:20)：补秒+空格
const normTime = v => v ? v.replace("T"," ").slice(0,16)+":00" : "";

async function invoke(method, params, options={timeoutMs:60000}) {
  if (!state.connId) throw new Error("无连接上下文：请从连接入口打开工作台");
  return window.dbxPlugin.invoke(method, { connectionId: state.connId, ...params }, options);
}
function lineHtml(no, text, fresh, isCtx) {
  let h = esc(text);
  // 结构化着色先行：命中片段进占位 stash，关键字高亮切不到 NUL 占位符，最后换回
  const stash = [];
  if (fx.struct) h = structHtml(h, stash);
  const kw = $("keyword").value.trim();
  // 正则模式下 kw 是表达式，字面 split 无意义则跳过高亮（后端已按正则过滤）
  if (kw && !$("regexCk").checked) h = h.split(esc(kw)).join("<mark>"+esc(kw)+"</mark>");
  if (fx.struct) h = h.replace(/\x00(\d+)\x00/g, (_, i) => stash[+i]);
  const cls = /ERROR|FATAL/i.test(text) ? "err" : /WARN/i.test(text) ? "warn" : "";
  // fresh 实时行滑入动画由 CSS .fresh 承载；ctx 上下文行淡显
  return `<div class="${cls}${fresh ? " fresh" : ""}${isCtx ? " ctx" : ""}"><span class="ln">${no}</span>${h}</div>`;
}
// 结构化着色（输入已转义）：行首时间戳 + [LEVEL] token + 其他方括号段；命中片段压 stash 返回占位
function structHtml(h, stash) {
  const tok = (cls, m) => { stash.push(`<span class="${cls}">${m}</span>`); return "\x00" + (stash.length - 1) + "\x00"; };
  h = h.replace(/^\d{4}[-/]\d{1,2}[-/]\d{1,2}[T ]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})?/,
    m => tok("tk-t", m));
  h = h.replace(/\[(INF|DBG|WRN|ERR|TRACE|DEBUG|INFO|WARN|ERROR|FATAL)\]/gi, m => {
    const c = /ERR|FATAL/i.test(m) ? "tk-le" : /WARN/i.test(m) ? "tk-lw" : /INFO|INF/i.test(m) ? "tk-li" : "tk-lo";
    return tok(c, m);
  });
  h = h.replace(/\[[^\][<>]{1,40}\]/g, m => tok("tk-b", m)); // 线程/类名等方括号段淡色
  return h;
}
function render(lines) {
  $("logs").innerHTML = lines.map(l => lineHtml(l.no, l.text, false, l.match === false)).join("") || '<div class="empty"><span class="big">🔎</span>无匹配日志，换个关键字试试</div>';
  if ($("autoscroll").checked) $("logs").scrollTop = $("logs").scrollHeight;
  const pages = Math.max(1, Math.ceil(state.total / +$("pageSize").value));
  $("pageinfo").textContent = `第 ${state.page} / ${pages} 页，共 ${state.total} 行`;
  renderDistbar(lines); // 当前页级别占比（近似分布，不含实时追加行）
}
// 级别分布条：按当前页统计 err/warn/其余占比；空结果则隐藏
function renderDistbar(lines) {
  const bar = $("distbar");
  if (!bar || !fx.dist || !lines.length) { if (bar) bar.hidden = true; return; }
  let e = 0, w = 0;
  for (const l of lines) {
    if (/ERROR|FATAL/i.test(l.text)) e++;
    else if (/WARN/i.test(l.text)) w++;
  }
  const n = lines.length, pct = v => (v / n * 100).toFixed(1) + "%";
  const segs = bar.children;
  segs[0].style.width = pct(e); segs[1].style.width = pct(w); segs[2].style.width = pct(n - e - w);
  bar.hidden = false;
  bar.title = `ERROR ${e} / WARN ${w} / 其他 ${n - e - w}（当前页）`;
}
async function loadBrowse(dir) {
  if (dir !== undefined) state.dir = dir;
  const r = await invoke("logs/browse", { dir: state.dir });
  state.dirs = r.dirs; state.files = r.entries;
  renderCrumb(); renderDirs(); renderFiles();
}
// 面包屑：根 + 各级，可点回退；换目录时清空选中
function renderCrumb() {
  const parts = state.dir ? state.dir.split("/") : [];
  let h = `<span class="crumb" data-d="">根</span>`;
  let acc = "";
  parts.forEach(p => { acc = acc ? acc + "/" + p : p; h += ` / <span class="crumb" data-d="${esc(acc)}">${esc(p)}</span>`; });
  $("crumb").innerHTML = h;
  document.querySelectorAll(".crumb").forEach(el => el.onclick = () => {
    state.file = null; loadBrowse(el.dataset.d).catch(e => $("info").textContent = "浏览失败：" + e.message);
  });
}
// 子目录：点进入
function renderDirs() {
  $("dirs").innerHTML = state.dirs.map(d =>
    `<span class="dir" data-d="${esc(state.dir ? state.dir + "/" + d.name : d.name)}">📁 ${esc(d.name)}</span>`).join("");
  document.querySelectorAll(".dir").forEach(el => el.onclick = () => {
    state.file = null; loadBrowse(el.dataset.d).catch(e => $("info").textContent = "浏览失败：" + e.message);
  });
}
// 文件卡片：隐藏名单过滤 + 文件名过滤 + 排序；选中态在刷新后保留
function visibleFiles() {
  const kw = state.fileFilter.trim();
  const list = state.files.filter(f => !state.hidden.includes(f.name) && (!kw || f.name.includes(kw)));
  if (state.fileSort === "name") list.sort((a, b) => a.name < b.name ? -1 : 1);
  else if (state.fileSort === "size") list.sort((a, b) => b.size - a.size);
  else list.sort((a, b) => b.modified_at - a.modified_at);
  return list;
}
// 短名：相对路径只取末段展示，全路径放 title 与 tgt 行
const shortName = rel => rel.split("/").pop();
// Bento 分档：>10MB 大卡跨3列，>1MB 中卡，其余小卡（只定跨度，不改过滤排序）
const bento = f => f.size > 10485760 ? " bento-lg" : f.size > 1048576 ? " bento-md" : " bento-sm";
function renderFiles() {
  const list = visibleFiles();
  $("files").innerHTML = list.map(f =>
    `<div class="file${state.file === f.name ? " active" : ""}${bento(f)}" data-n="${esc(f.name)}"><span class="hide" data-n="${esc(f.name)}" title="仅在界面隐藏，文件保留">隐藏</span><b>${esc(shortName(f.name))}</b><span>${fmtSize(f.size)}　${fmtTime(f.modified_at)}</span><span class="tgt" title="${esc(f.name)}">${esc(f.name)}</span></div>`).join("")
    || '<div class="empty"><span class="big">📂</span>本目录下没有匹配的 .log / .out 文件</div>';
  document.querySelectorAll(".file").forEach(el => el.onclick = () => {
    document.querySelectorAll(".file").forEach(x => x.classList.remove("active"));
    el.classList.add("active"); state.file = el.dataset.n; state.page = 1; doSearch();
  });
  document.querySelectorAll(".file .hide").forEach(el => el.onclick = e => {
    e.stopPropagation(); doHide(el.dataset.n);
  });
  renderHiddenBar(); syncSums();
}
// 隐藏只藏界面、文件保留：名单按连接存 storage，可恢复
// 直接执行无 confirm：沙盒 iframe 无 allow-modals 时 confirm() 静默返回 false 导致点隐藏没反应；误点用恢复全部找回
async function doHide(name) {
  if (state.tailing && state.file === name) await toggleTail().catch(()=>{});
  if (!state.hidden.includes(name)) state.hidden.push(name);
  if (state.file === name) { state.file = null; $("logs").innerHTML = ""; $("loghead").innerHTML = '未选择文件<span id="rate"></span>'; $("heroSub").textContent = "选择左侧文件开始查看"; }
  await saveHidden(); renderFiles();
  $("info").textContent = `已隐藏 ${name}（文件保留，可恢复）`;
}
function renderHiddenBar() {
  $("hiddenbar").innerHTML = state.hidden.length
    ? `<span>已隐藏 ${state.hidden.length} 个（文件均保留）</span><button id="unhide" class="ghost">恢复全部</button>`
    : "";
  const b = $("unhide");
  if (b) b.onclick = async () => { state.hidden = []; await saveHidden(); renderFiles(); };
}
async function loadHidden() {
  try { if (window.dbxPlugin.capabilities.storage && state.connId) {
    const h = await window.dbxPlugin.storage.get("hidden:" + state.connId);
    if (Array.isArray(h)) state.hidden = h;
  } } catch {} // 存储失败不阻塞主流程
}
async function saveHidden() {
  try { if (window.dbxPlugin.capabilities.storage && state.connId)
    await window.dbxPlugin.storage.set("hidden:" + state.connId, state.hidden);
  } catch {} // 存储失败不阻塞主流程
}
// 快捷时间：填 start 为 now-分钟、end 留空（=至今），直接搜；手动改框不互斥
const pad2 = n => String(n).padStart(2, "0");
const toLocalInput = d => `${d.getFullYear()}-${pad2(d.getMonth()+1)}-${pad2(d.getDate())}T${pad2(d.getHours())}:${pad2(d.getMinutes())}`;
function setQuick(min) {
  $("start").value = toLocalInput(new Date(Date.now() - min*60000));
  $("end").value = "";
  state.page = 1; doSearch().catch(e => $("info").textContent = "搜索失败：" + e.message);
}
async function doSearch() {
  if (!state.file) { $("info").textContent = "请先选择日志文件"; return; }
  // 骨架屏：等待期间占位（关特效则直接空白等待）
  if (fx.skel) $("logs").innerHTML = '<div class="skel"></div>'.repeat(8);
  try {
    const r = await invoke("logs/search", { file: state.file, keyword: $("keyword").value,
      regex: $("regexCk").checked, context: +$("context").value,
      level: $("level").value, startTime: normTime($("start").value), endTime: normTime($("end").value),
      page: state.page, pageSize: +$("pageSize").value, sort: $("sort").value });
    state.total = r.total; render(r.lines);
    $("loghead").innerHTML = `${esc(state.file)}（共 ${r.total} 行${r.truncated ? "，已截断" : ""}）<span id="rate"></span>`;
    $("heroSub").textContent = `${state.file} · 共 ${r.total} 行`;
    $("info").textContent = "";
    pushHistKw($("keyword").value); // 成功搜索的关键字记入命令面板历史
    saveCond(); syncSums(); // 搜成功才记条件，避免脏条件污染
  } catch (e) { $("logs").innerHTML = ""; $("info").textContent = "搜索失败：" + e.message; }
}
// 多主题：id 存 storage 持久化；auto 跟随宿主，其余走 data-theme 变量覆盖
const THEMES = [
  { id: "dbx", name: "DBX 原生", icon: "🖥️", dot: "#fafafa" },
  { id: "auto", name: "跟随", icon: "🔄", dot: "linear-gradient(135deg,#6d5dfc,#2cc9a7)" },
  { id: "light", name: "纸白", icon: "☀️", dot: "#f4f4fb" },
  { id: "dark", name: "午夜", icon: "🌙", dot: "#13131a" },
  { id: "forest", name: "墨绿", icon: "🌲", dot: "#0d1512" },
  { id: "ocean", name: "深海", icon: "🌊", dot: "#0f1a2e" },
  { id: "sunset", name: "黄昏", icon: "🌇", dot: "#1c1410" },
  { id: "mint", name: "薄荷", icon: "🌿", dot: "#eef7f3" },
];
let theme = "dbx";
const themeById = id => THEMES.find(t => t.id === id);
function applyTheme(t) {
  if (!themeById(t)) t = "dbx";
  theme = t;
  if (t === "auto") document.body.removeAttribute("data-theme");
  else document.body.setAttribute("data-theme", t);
  const b = $("themeBtn"), m = themeById(t);
  if (b) { b.textContent = "🎨"; b.title = "选择主题：当前" + m.name; }
  renderThemePanel();
}
// 面板选项：色卡圆点 + 图标 + 名称，当前项加粗
// 面板选中态：静态选项只更新 .sel，不重渲染内容（渲染失败即空白，故静态化）
function renderThemePanel() {
  document.querySelectorAll("#themePanel .theme-opt").forEach(o =>
    o.classList.toggle("sel", o.dataset.t === theme));
}
function toggleThemePanel(show) {
  const p = $("themePanel");
  if (!p) return;
  const open = show !== undefined ? show : p.hidden;
  if (open) renderThemePanel();
  p.hidden = !open;
}
async function saveTheme() {
  try { if (window.dbxPlugin.capabilities.storage) await window.dbxPlugin.storage.set("theme", theme); } catch {}
}
async function loadTheme() {
  try { if (window.dbxPlugin.capabilities.storage) {
    const t = await window.dbxPlugin.storage.get("theme");
    if (themeById(t)) { applyTheme(t); return; }
  } } catch {} // 存储失败不阻塞主流程
  applyTheme("dbx"); // 默认 DBX 原生；老存量 auto/light/dark 照常兼容
}
// 特效开关：flow 流光 / dist 分布条 / skel 骨架屏 / keys 快捷键提示；存 storage 持久化
const FX_KEYS = ["flow", "dist", "skel", "keys", "struct"];
let fx = { flow: true, dist: true, skel: true, keys: true, struct: true };
function applyFx() {
  for (const k of FX_KEYS) {
    if (fx[k]) document.body.removeAttribute("data-fx-" + k);
    else document.body.setAttribute("data-fx-" + k, "off");
  }
  document.querySelectorAll("#themePanel .fx-opt").forEach(o =>
    o.classList.toggle("off", !fx[o.dataset.fx]));
}
async function saveFx() {
  try { if (window.dbxPlugin.capabilities.storage) await window.dbxPlugin.storage.set("fx", fx); } catch {}
}
async function loadFx() {
  try { if (window.dbxPlugin.capabilities.storage) {
    const s = await window.dbxPlugin.storage.get("fx");
    if (s) for (const k of FX_KEYS) if (typeof s[k] === "boolean") fx[k] = s[k];
  } } catch {} // 存储失败不阻塞主流程
  applyFx();
}
// 命令面板历史关键字（内存，最多 5 个，供面板快捷搜索）
const histKw = [];
function pushHistKw(kw) {
  kw = (kw || "").trim();
  if (!kw) return;
  const i = histKw.indexOf(kw);
  if (i >= 0) histKw.splice(i, 1);
  histKw.unshift(kw);
  if (histKw.length > 5) histKw.length = 5;
}
// 子序列模糊匹配：命中返回分数（连续命中加权），未命中返回 -1
function fuzzy(q, s) {
  q = (q || "").toLowerCase(); s = (s || "").toLowerCase();
  if (!q) return 0;
  let qi = 0, score = 0, last = -2;
  for (let i = 0; i < s.length && qi < q.length; i++) {
    if (s[i] === q[qi]) { score += (last === i - 1) ? 2 : 1; last = i; qi++; }
  }
  return qi === q.length ? score : -1;
}
let cmdItems = [], cmdSel = 0;
// 命令候选：快捷操作 + 历史关键字 + 可见文件；fuzzy 过滤取前 12
function renderCmd() {
  const q = $("cmdInput").value.trim(), pool = [];
  const ops = [
    { label: "🔍 搜 ERROR", sub: "操作", run: () => { $("level").value = "ERROR"; state.page = 1; doSearch(); } },
    { label: "🔍 搜 WARN", sub: "操作", run: () => { $("level").value = "WARN"; state.page = 1; doSearch(); } },
    { label: "⏱ 实时监控 开/关", sub: "操作", run: () => $("tail").click() },
    { label: "⬇ 下载日志", sub: "操作", run: () => $("download").click() },
    { label: "清空时间条件", sub: "操作", run: () => { $("start").value = ""; $("end").value = ""; } },
  ];
  for (const o of ops) { const s = fuzzy(q, o.label); if (s >= 0) pool.push({ ...o, s: s + 100 }); }
  for (const k of histKw) { const s = fuzzy(q, k); if (s >= 0)
    pool.push({ label: "🔎 " + k, sub: "历史", s: s + 50, run: ((kk) => () => {
      $("keyword").value = kk; state.page = 1; doSearch(); })(k) }); }
  for (const f of visibleFiles()) { const s = fuzzy(q, shortName(f.name)); if (s >= 0)
    pool.push({ label: "📄 " + shortName(f.name), sub: fmtSize(f.size), s, run: ((ff) => () => {
      state.file = ff.name; state.page = 1; renderFiles(); doSearch(); })(f) }); }
  pool.sort((a, b) => b.s - a.s);
  cmdItems = pool.slice(0, 12); cmdSel = 0;
  $("cmdList").innerHTML = cmdItems.map((it, i) =>
    `<div class="cmd-item${i === cmdSel ? " sel" : ""}" data-i="${i}">${esc(it.label)}<span class="sub">${esc(it.sub)}</span></div>`).join("")
    || '<div class="hint" style="padding:12px">无匹配命令</div>';
}
function toggleCmd(show) {
  const m = $("cmdMask");
  if (!m) return;
  const open = show !== undefined ? show : m.hidden;
  m.hidden = !open;
  if (open) { $("cmdInput").value = ""; renderCmd(); setTimeout(() => $("cmdInput").focus(), 0); }
}
function cmdMove(d) {
  if (!cmdItems.length) return;
  cmdSel = (cmdSel + d + cmdItems.length) % cmdItems.length;
  document.querySelectorAll("#cmdList .cmd-item").forEach((el, i) =>
    el.classList.toggle("sel", i === cmdSel));
  const sel = document.querySelector("#cmdList .cmd-item.sel");
  if (sel) sel.scrollIntoView({ block: "nearest" });
}
// 实时吞吐：1 秒窗口计数，LIVE 时显示 x 行/秒
let rateN = 0, rateShown = false;
setInterval(() => {
  const el = document.querySelector("#rate");
  if (!el) return;
  if (state.tailing && rateShown) el.textContent = rateN + " 行/秒";
  else if (!state.tailing) el.textContent = "";
  rateN = 0;
}, 1000);
function syncSortBtn() {
  const b = $("sortBtn");
  if (b) b.textContent = $("sort").value === "asc" ? "⇅ 最早在前" : "⇅ 最新在前";
}
function syncWrapBtn() {
  const on = $("logs").classList.contains("wrap"), b = $("wrapBtn");
  if (b) b.textContent = on ? "↵ 换行：开" : "↵ 换行：关";
}
// 折叠：双卡片默认收起，状态记 storage；摘要行展示关键状态
let coll = { file: true, search: true };
function applyColl() {
  $("fileCard").classList.toggle("collapsed", coll.file);
  $("searchCard").classList.toggle("collapsed", coll.search);
  syncSums();
}
function syncSums() {
  // 文件摘要：选中名或可见数；搜索摘要：关键字·级别·时间
  $("fileSum").textContent = state.file || `共 ${visibleFiles().length} 个`;
  const kw = $("keyword").value.trim() || "无关键字";
  $("searchSum").textContent = `${kw} · ${$("level").value} · ${$("start").value || "不限"}→${$("end").value || "至今"}`;
}
async function saveColl() {
  try { if (window.dbxPlugin.capabilities.storage) await window.dbxPlugin.storage.set("uicoll", coll); } catch {}
}
async function loadColl() {
  let had = false;
  try { if (window.dbxPlugin.capabilities.storage) {
    const c = await window.dbxPlugin.storage.get("uicoll");
    if (c && typeof c.file === "boolean") { coll = c; had = true; }
  } } catch {}
  applyColl();
  return had;
}
async function saveCond() {
  try { if (window.dbxPlugin.capabilities.storage)
    await window.dbxPlugin.storage.set("cond", { keyword: $("keyword").value, level: $("level").value,
      pageSize: $("pageSize").value, sort: $("sort").value, dir: state.dir, file: state.file,
      regex: $("regexCk").checked, context: $("context").value,
      wrap: $("logs").classList.contains("wrap") });
  } catch {} // 存储失败不阻塞主流程
}
async function loadCond() {
  try { if (window.dbxPlugin.capabilities.storage) {
    const c = await window.dbxPlugin.storage.get("cond");
    if (c) { $("keyword").value = c.keyword||""; $("level").value = c.level||"ALL";
      $("pageSize").value = c.pageSize||"100"; $("sort").value = c.sort||"desc";
      $("regexCk").checked = !!c.regex; $("context").value = c.context||"0";
      $("logs").classList.toggle("wrap", !!c.wrap); syncSortBtn(); syncWrapBtn();
      if (c.dir) state.dir = c.dir; state.resumeFile = c.file||null; }
  } } catch {}
}
async function toggleTail() {
  if (state.tailing) {
    await invoke("logs/stop", { streamId: state.streamId }).catch(()=>{});
    state.tailing = false; state.streamId = null;
    $("tail").textContent = "⏱ 实时监控";
    $("status").classList.remove("live"); $("statusText").textContent = "实时通道未连接";
    rateShown = false;
    return;
  }
  if (!state.file) { $("info").textContent = "请先选择日志文件"; return; }
  const r = await invoke("logs/tail", { file: state.file, keyword: $("keyword").value,
    regex: $("regexCk").checked,
    level: $("level").value, lastN: 200 }, { timeoutMs: 15000 });
  state.streamId = r.streamId; state.tailing = true;
  $("tail").textContent = "⏹ 停止监控";
  $("status").classList.add("live"); $("statusText").textContent = "实时监控中";
}
async function doDownload() {
  if (!state.file) { $("info").textContent = "请先选择日志文件"; return; }
  $("download").disabled = true;
  try {
    // 分块拉取后拼装：单块 JSON 不超限，大文件多轮拉
    const chunks = []; let offset = 0;
    for (let i = 0; i < 1000; i++) {
      const r = await invoke("logs/downloadChunk", { file: state.file, keyword: $("keyword").value,
        regex: $("regexCk").checked,
        level: $("level").value, offset, length: 2000 }, { timeoutMs: 60000 });
      chunks.push(r.lines.join("\n")); offset = r.nextOffset;
      $("info").textContent = `下载中…已拉取 ${offset} 行`;
      if (r.eof) break;
    }
    const text = chunks.join("\n"), ft = window.dbxPlugin.fileTransfer;
    if (ft) { // 桌面端：原生保存对话框 + 分块写入
      const t = await ft.beginSave({ name: state.file });
      if (!t) { $("info").textContent = "已取消保存"; return; }
      const data = new TextEncoder().encode(text);
      for (let o = 0; o < data.length; o += 256*1024)
        await ft.write(t.handleId, o, data.slice(o, o+256*1024));
      await ft.finish(t.handleId); $("info").textContent = `已保存 ${offset} 行`;
    } else { // Web 宿主：Blob 回退下载
      const a = document.createElement("a");
      a.href = URL.createObjectURL(new Blob([text], { type: "text/plain" }));
      a.download = state.file; a.click(); URL.revokeObjectURL(a.href);
    }
  } catch (e) { $("info").textContent = "下载失败：" + e.message; }
  finally { $("download").disabled = false; }
}
function onBackendEvent(e) {
  // 兼容不同宿主版本的事件包形：{method,params} 或扁平体
  const m = e && (e.method || e.type || e.event);
  const p = (e && (e.params || e.data)) || e || {};
  if (m === "logs/append" && p.streamId === state.streamId && Array.isArray(p.lines)) {
    const atBottom = $("logs").scrollTop + $("logs").clientHeight >= $("logs").scrollHeight - 40;
    // fresh 标记滑入动画；rateN 供吞吐计数
    $("logs").insertAdjacentHTML("beforeend", p.lines.map(t => lineHtml("", t, true)).join(""));
    rateN += p.lines.length; rateShown = true;
    if ($("autoscroll").checked || atBottom) $("logs").scrollTop = $("logs").scrollHeight;
  }
}
window.addEventListener("DOMContentLoaded", async () => {
  $("search").onclick = () => { state.page = 1; doSearch().catch(e => $("info").textContent = "搜索失败：" + e.message); };
  $("refresh").onclick = () => loadBrowse().catch(e => $("info").textContent = "刷新失败：" + e.message);
  $("fileFilter").oninput = e => { state.fileFilter = e.target.value; renderFiles(); };
  $("fileSort").onchange = e => { state.fileSort = e.target.value; renderFiles(); };
  document.querySelectorAll("#quickrow [data-min]").forEach(b => b.onclick = () => setQuick(+b.dataset.min));
  $("clearTime").onclick = () => { $("start").value = ""; $("end").value = ""; };
  $("prev").onclick = () => { if (state.page > 1) { state.page--; doSearch().catch(e => $("info").textContent = e.message); } };
  $("next").onclick = () => { state.page++; doSearch().catch(e => { state.page--; $("info").textContent = e.message; }); };
  $("tail").onclick = () => toggleTail().catch(e => $("info").textContent = "实时监控失败：" + e.message);
  $("download").onclick = doDownload;
  // 日志头开关：排序与下拉双向同步并重搜；换行切 class 并随条件记住
  syncSortBtn(); syncWrapBtn();
  $("sortBtn").onclick = () => { $("sort").value = $("sort").value === "asc" ? "desc" : "asc";
    syncSortBtn(); state.page = 1; doSearch().catch(e => $("info").textContent = e.message); };
  $("sort").onchange = syncSortBtn;
  $("wrapBtn").onclick = () => { $("logs").classList.toggle("wrap"); syncWrapBtn(); saveCond(); };
  $("fileHead").onclick = () => { coll.file = !coll.file; applyColl(); saveColl(); };
  $("searchHead").onclick = () => { coll.search = !coll.search; applyColl(); saveColl(); };
  $("themeBtn").onclick = e => { e.stopPropagation(); toggleThemePanel(); };
  $("themePanel").onclick = e => { const o = e.target.closest(".theme-opt");
    if (o) { applyTheme(o.dataset.t); saveTheme(); toggleThemePanel(false); } };
  // 点面板外关闭（按钮自身已 stopPropagation，不冲突）
  document.addEventListener("click", e => { const p = $("themePanel");
    if (p && !p.hidden && !e.target.closest("#themePanel,#themeBtn")) toggleThemePanel(false); });
  // 外观面板特效开关：切换 fx + 持久化，不关闭面板
  document.querySelectorAll("#themePanel .fx-opt").forEach(o => o.onclick = e => {
    e.stopPropagation(); fx[o.dataset.fx] = !fx[o.dataset.fx]; applyFx(); saveFx();
  });
  // 命令面板：按钮 + 输入过滤 + 上下回车 + 遮罩点击关闭
  $("cmdBtn").onclick = e => { e.stopPropagation(); toggleCmd(); };
  $("cmdInput").oninput = renderCmd;
  $("cmdInput").onkeydown = e => {
    if (e.key === "ArrowDown") { e.preventDefault(); cmdMove(1); }
    else if (e.key === "ArrowUp") { e.preventDefault(); cmdMove(-1); }
    else if (e.key === "Enter") { const it = cmdItems[cmdSel]; toggleCmd(false); if (it) it.run(); }
  };
  $("cmdList").onclick = e => { const o = e.target.closest(".cmd-item");
    if (o) { const it = cmdItems[+o.dataset.i]; toggleCmd(false); if (it) it.run(); } };
  $("cmdMask").onclick = e => { if (e.target === $("cmdMask")) toggleCmd(false); };
  // 全局快捷键：Ctrl+K 命令，Esc 关闭浮层；输入框内只响应 Esc
  document.addEventListener("keydown", e => {
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "k") { e.preventDefault(); toggleCmd(); return; }
    if (e.key === "Escape") { toggleCmd(false); toggleThemePanel(false); return; }
    const tag = document.activeElement && document.activeElement.tagName;
    if (tag === "INPUT" || tag === "SELECT" || tag === "TEXTAREA") return;
    if (e.key === "/") { e.preventDefault(); $("keyword").focus(); }
    else if (e.key === "t" && !e.ctrlKey && !e.metaKey) { $("tail").click(); }
    else if (e.key === "ArrowLeft") { $("prev").click(); }
    else if (e.key === "ArrowRight") { $("next").click(); }
  });
  if (!window.dbxPlugin) { $("info").textContent = "不在 DBX 宿主内：请经 DBX 工作台或 dbx-plugin dev 打开"; return; }
  await window.dbxPlugin.ready;
  state.connId = window.dbxPlugin.context && window.dbxPlugin.context.connectionId;
  if (!state.connId) { $("info").textContent = "无连接上下文：请从连接入口打开工作台";
    for (const id of ["refresh","search","download","tail","prev","next"]) { const b = $(id); if (b) b.disabled = true; }
    return; }
  try { window.dbxPlugin.onEvent(onBackendEvent); } catch {} // 旧宿主无该 API 则实时推送不可用
  window.addEventListener("dbx-plugin-env", () => {}); // 主题经 CSS 变量自动跟随，无需手动刷新
  await loadCond();
  await loadTheme();
  await loadFx();
  await loadHidden();
  const hadColl = await loadColl();
  await loadBrowse().catch(e => $("info").textContent = "加载文件列表失败：" + e.message);
  // 首次无历史选中：文件卡片自动展开一次，新人找得到入口
  if (!state.resumeFile && !hadColl) { coll.file = false; applyColl(); }
  // 上次选中的文件若仍在当前目录，自动恢复选中并搜索
  if (state.resumeFile && state.files.some(f => f.name === state.resumeFile) && !state.hidden.includes(state.resumeFile)) {
    state.file = state.resumeFile; state.page = 1;
    renderFiles();
    await doSearch().catch(e => $("info").textContent = "搜索失败：" + e.message);
  }
  state.resumeFile = null;
});
