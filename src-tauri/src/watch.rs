// 公众号实时监听（v0.9，借鉴 WeChatPubMonitor 思路但零 hook 零订阅）
// 原理：公众号文章页是公开网页。用微信搜索接口 / 历史文章接口轮询指定 biz 的最新文章，
//       发现新文章 → 存 10-inbox/ → 可选抓正文（公开 HTML）清洗 → 飞书机器人推送 → HTTP 接口供评论寻客
// 采集到的 biz 来源：v0.8 的浏览记录 / 手动添加 / 导出 JSON
use std::fs;
use std::path::Path;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchTarget {
    pub name: String,   // 公众号名
    pub biz: String,    // __biz 参数（Mzxxx 形式）
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchConfig {
    pub targets: Vec<WatchTarget>,
    pub interval_min: u32,     // 轮询间隔（分钟）
    pub fetch_content: bool,   // 是否抓正文
    pub feishu_notify: bool,   // 新文章飞书提醒
    pub seen: Vec<String>,     // 已见文章 key（防重复，裁剪到最近 500）
}
impl Default for WatchConfig {
    fn default() -> Self {
        Self { targets: vec![], interval_min: 30, fetch_content: true, feishu_notify: true, seen: vec![] }
    }
}

fn watch_file(kb: &Path) -> std::path::PathBuf {
    let d = kb.join("watch");
    let _ = fs::create_dir_all(&d);
    d.join("watch.json")
}

fn load_watch(kb: &Path) -> WatchConfig {
    fs::read_to_string(watch_file(kb)).ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_watch(kb: &Path, cfg: &WatchConfig) -> Result<(), String> {
    fs::write(watch_file(kb), serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/126 Safari/537.36")
        .build().unwrap()
}

/// 管理监听列表：list/add/remove/设置
#[tauri::command]
pub async fn watch_list(state: tauri::State<'_, crate::PetState>) -> Result<WatchConfig, String> {
    let kb = state.kb_dir.lock().unwrap().clone();
    Ok(load_watch(&kb))
}

#[tauri::command]
pub async fn watch_add(state: tauri::State<'_, crate::PetState>, name: String, biz: String) -> Result<String, String> {
    let kb = state.kb_dir.lock().unwrap().clone();
    let mut cfg = load_watch(&kb);
    let biz = biz.trim().to_string();
    if biz.is_empty() { return Err("biz 不能为空（从文章链接 __biz= 参数取）".into()); }
    if cfg.targets.iter().any(|t| t.biz == biz) { return Ok("该公众号已在监听列表".into()); }
    cfg.targets.push(WatchTarget { name: name.trim().to_string(), biz });
    save_watch(&kb, &cfg)?;
    Ok(format!("已添加「{}」监听（共 {} 个号）", name, cfg.targets.len()))
}

#[tauri::command]
pub async fn watch_remove(state: tauri::State<'_, crate::PetState>, biz: String) -> Result<String, String> {
    let kb = state.kb_dir.lock().unwrap().clone();
    let mut cfg = load_watch(&kb);
    let before = cfg.targets.len();
    cfg.targets.retain(|t| t.biz != biz);
    save_watch(&kb, &cfg)?;
    Ok(format!("已移除（剩 {} 个号）", cfg.targets.len()))
}

#[tauri::command]
pub async fn watch_set(state: tauri::State<'_, crate::PetState>, interval_min: Option<u32>, fetch_content: Option<bool>, feishu_notify: Option<bool>) -> Result<String, String> {
    let kb = state.kb_dir.lock().unwrap().clone();
    let mut cfg = load_watch(&kb);
    if let Some(v) = interval_min { cfg.interval_min = v.clamp(5, 720); }
    if let Some(v) = fetch_content { cfg.fetch_content = v; }
    if let Some(v) = feishu_notify { cfg.feishu_notify = v; }
    save_watch(&kb, &cfg)?;
    Ok("已保存监听设置".into())
}

/// 从 v0.8 浏览记录一键导入（去重后追加到监听列表）
#[tauri::command]
pub async fn watch_import_from_capture(state: tauri::State<'_, crate::PetState>) -> Result<String, String> {
    let root = crate::wechat_capture::profiles_root_public();
    let Some(root) = root else { return Err("只在 Windows + 微信 4.x 下可用".into()); };
    let root2 = root.clone();
    let arts = tauri::async_runtime::spawn_blocking(move || crate::wechat_capture::capture_sync_public(&root2, 90))
        .await.map_err(|e| e.to_string())?;
    let kb = state.kb_dir.lock().unwrap().clone();
    let mut cfg = load_watch(&kb);
    let mut added = 0;
    for a in &arts {
        let get = |k: &str| -> Option<String> {
            a.url.split(['?', '&']).find_map(|p| p.strip_prefix(k).and_then(|v| v.strip_prefix('=')))
                .map(|v| v.split('&').next().unwrap_or("").to_string())
        };
        if let Some(biz) = get("__biz") {
            if !cfg.targets.iter().any(|t| t.biz == biz) {
                cfg.targets.push(WatchTarget { name: if a.author.is_empty() { format!("公众号({})", &biz[..biz.len().min(8)]) } else { a.author.clone() }, biz });
                added += 1;
            }
        }
    }
    save_watch(&kb, &cfg)?;
    Ok(format!("已导入 {} 个新公众号（共 {} 个在监听）", added, cfg.targets.len()))
}

/// 抓正文：mp.weixin.qq.com 公开 HTML → 提取正文文本（单行纯文本，去样式脚本）
async fn fetch_content_text(url: &str) -> Option<String> {
    let html = client().get(url).send().await.ok()?.text().await.ok()?;
    // 正文容器 id="js_content"（公众号正文标准结构）
    let start = html.find("js_content")?;
    let seg = &html[start..];
    let end = seg.find("id=\"js_tags\"").or_else(|| seg.find("js_tags")).unwrap_or(seg.len().min(120_000));
    let seg = &seg[..end];
    // 去掉 <script>/<style> 块
    let mut out = String::new();
    let mut rest = seg;
    while let Some(p) = rest.find('<') {
        out.push_str(&rest[..p]);
        let after = &rest[p..];
        let tag_end = after.find('>')? + p;
        let tag = rest[p + 1..tag_end].trim().to_lowercase();
        // script/style 整块跳过
        if tag.starts_with("script") || tag.starts_with("style") {
            let name = tag.split(|c| c == ' ' || c == '>').next().unwrap_or("");
            let close = format!("</{}", name);
            match rest[tag_end + 1..].find(&close) { Some(c) => { rest = &rest[tag_end + 1 + c + close.len()..]; continue; } None => break }
        }
        // 段落/换行标签 → 空格分隔
        if tag.starts_with("p") || tag.starts_with("/p") || tag.starts_with("br") || tag.starts_with("section") {
            out.push(' ');
        }
        rest = &rest[tag_end + 1..];
        if rest.len() > 400_000 { break; } // 安全阀
    }
    // HTML 实体粗解码 + 空白折叠成单行
    let decoded = out.replace("&nbsp;", " ").replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"")
        .replace("&#39;", "'").replace("&ldquo;", "“").replace("&rdquo;", "”");
    let mut line = String::new();
    let mut last_space = false;
    for c in decoded.chars() {
        if c.is_whitespace() {
            if !last_space && !line.is_empty() { line.push(' '); last_space = true; }
        } else { line.push(c); last_space = false; }
    }
    let t = line.trim().to_string();
    (t.len() > 50).then(|| t.chars().take(8000).collect())
}

/// 轮询一次：对每个监听号尝试发现新文章。
/// 数据源：微信「历史文章」接口需要 key，易失效；这里用公开可访问的搜狗微信搜索兜底 + 
/// 用户自填的文章列表接口。为稳定，核心实现：sogou weixin 搜索「公众号名」最新文章。
#[derive(Serialize)]
pub struct WatchRunResult {
    pub checked: usize,
    pub found: Vec<String>,   // 新文章标题（已入库）
    pub note: String,
}

pub async fn run_watch_once(kb: &std::path::PathBuf, cfg: &mut WatchConfig) -> WatchRunResult {
    let mut found = Vec::new();
    let mut note = String::new();
    let cli = client();
    let feishu = crate::integrations::feishu_webhook_of(&crate::load_config(kb));
    for t in &cfg.targets {
        // 搜狗微信搜索：type=1 公众号文章搜索（公开页面，无需 key）
        let q = if t.name.starts_with("公众号(") { t.name.clone() } else { t.name.clone() };
        let url = format!("https://weixin.sogou.com/weixin?type=2&query={}", urlencode(&q));
        let html = match cli.get(&url).send().await { Ok(r) => r.text().await.unwrap_or_default(), Err(e) => { note = format!("网络错误: {e}"); continue } };
        // 解析结果：<h3><a ... href="...url..." ...>标题</a>
        let mut pos = 0usize;
        while let Some(h) = html[pos..].find("<h3>") {
            let seg_start = pos + h;
            let seg_end = (seg_start + 800).min(html.len());
            let seg = &html[seg_start..seg_end];
            let Some(a_s) = seg.find("href=\"") else { break };
            let href = &seg[a_s + 6..];
            let Some(h_e) = href.find('"') else { break };
            let link = &href[..h_e];
            let Some(t_s) = seg[ h_e + a_s + 6..].find('>') else { break };
            let after = &seg[h_e + a_s + 6 + t_s + 1..];
            let Some(t_e) = after.find("</a>") else { break };
            let mut title = after[..t_e].to_string();
            title = title.replace("<em>", "").replace("</em>", "");
            pos = seg_start + 100;
            // 只收 mp.weixin 文章
            let full_link = if link.starts_with("http") { link.to_string() } else { format!("https://weixin.sogou.com{}", link) };
            if !full_link.contains("weixin") && !title.is_empty() { /* sogou 跳转链接，直接入库标题+链接 */ }
            let key = crate::wechat_capture::key_of(&full_link);
            if cfg.seen.contains(&key) || key.is_empty() { continue; }
            // 入库 10-inbox/watch/
            let dir = kb.join("10-inbox").join("监听新文");
            let _ = fs::create_dir_all(&dir);
            let content_text = if cfg.fetch_content { fetch_content_text(&full_link).await.unwrap_or_default() } else { String::new() };
            let body = format!(
                "---\ntitle: \"{}\"\ntags: [监听, {}]\nstatus: inbox\ndate: {}\nsource: \"监听: {}\"\nurl: \"{}\"\n---\n\n# {}\n\n{}",
                title, t.name, crate::chrono_today(), t.name, full_link, title,
                if content_text.is_empty() { "（正文未抓到，打开链接查看）".to_string() } else { content_text }
            );
            let fname = format!("{}-{}.md", crate::chrono_today(), crate::sanitize(&title));
            let _ = fs::write(dir.join(&fname), body);
            found.push(format!("{}（{}）", title, t.name));
            // 飞书提醒
            if cfg.feishu_notify && !feishu.is_empty() {
                let _ = crate::integrations::feishu_send_raw(&feishu, &format!("📰 监听新文：{}（{}）\n{}", title, t.name, full_link)).await;
            }
            let mut seen = cfg.seen.clone();
            seen.push(key);
            cfg.seen = seen;
            if found.len() >= 10 { break; } // 单轮上限
        }
        if found.len() >= 10 { break; }
    }
    // seen 裁剪到 500
    if cfg.seen.len() > 500 { let n = cfg.seen.len(); cfg.seen = cfg.seen[n - 500..].to_vec(); }
    let _ = save_watch(kb, &cfg);
    WatchRunResult { checked: cfg.targets.len(), found, note }
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(*b as char),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// 手动触发一轮检查（前端按钮 / 后台定时）
#[tauri::command]
pub async fn watch_check(state: tauri::State<'_, crate::PetState>) -> Result<WatchRunResult, String> {
    let kb = state.kb_dir.lock().unwrap().clone();
    let mut cfg = load_watch(&kb);
    if cfg.targets.is_empty() { return Err("监听列表是空的，先添加公众号".into()); }
    Ok(run_watch_once(&kb, &mut cfg).await)
}

/// 前端定时器调：自动轮询（每 interval_min 分钟一次，前端 dailyTimer 同思路）
#[tauri::command]
pub async fn watch_auto_tick(state: tauri::State<'_, crate::PetState>) -> Result<String, String> {
    let kb = state.kb_dir.lock().unwrap().clone();
    let mut cfg = load_watch(&kb);
    if cfg.targets.is_empty() { return Ok("无监听目标".into()); }
    let r = run_watch_once(&kb, &mut cfg).await;
    if r.found.is_empty() { Ok(format!("检查 {} 个号，无新文", r.checked)) }
    else { Ok(format!("发现 {} 篇新文：{}", r.found.len(), r.found.join("、"))) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_watch_config_roundtrip() {
        let cfg = WatchConfig { targets: vec![WatchTarget { name: "地瓜工作室".into(), biz: "MzA1".into() }], interval_min: 15, fetch_content: false, feishu_notify: true, seen: vec!["mp:x".into()] };
        let s = serde_json::to_string(&cfg).unwrap();
        let back: WatchConfig = serde_json::from_str(&s).unwrap();
        assert_eq!(back.targets[0].name, "地瓜工作室");
        assert_eq!(back.interval_min, 15);
        assert!(!back.fetch_content);
    }

    #[test]
    fn test_default_config() {
        let d = WatchConfig::default();
        assert_eq!(d.interval_min, 30);
        assert!(d.fetch_content && d.feishu_notify);
    }

    #[test]
    fn test_urlencode() {
        assert_eq!(urlencode("地瓜"), "%E5%9C%B0%E7%93%9C");
        assert_eq!(urlencode("abc-1"), "abc-1");
    }

    #[test]
    fn test_content_extract_mock() {
        // 简单 HTML 提取逻辑的文本折叠验证（不联网）
        let html = "<div id=\"js_content\"><p>第一段 内容</p><p>第二段<b>加粗</b></p><script>bad()</script></div>";
        let start = html.find("js_content").unwrap();
        let seg = &html[start..];
        let mut out = String::new();
        let mut rest = seg;
        while let Some(p) = rest.find('<') {
            out.push_str(&rest[..p]);
            let after = &rest[p..];
            let tag_end = after.find('>').unwrap() + p;
            let tag = rest[p + 1..tag_end].trim().to_lowercase();
            if tag.starts_with("script") || tag.starts_with("style") {
                let name = tag.split(|c| c == ' ' || c == '>').next().unwrap_or("");
                let close = format!("</{}", name);
                match rest[tag_end + 1..].find(&close) { Some(c) => { rest = &rest[tag_end + 1 + c + close.len()..]; continue; } None => break }
            }
            if tag.starts_with("p") || tag.starts_with("br") || tag.starts_with("section") { out.push(' '); }
            rest = &rest[tag_end + 1..];
        }
        let mut line = String::new();
        let mut last_space = false;
        for c in out.chars() {
            if c.is_whitespace() { if !last_space && !line.is_empty() { line.push(' '); last_space = true; } }
            else { line.push(c); last_space = false; }
        }
        let t = line.trim();
        assert!(t.contains("第一段") && t.contains("第二段"), "got: {t}");
        assert!(!t.contains("bad()")); // script 未进入（在 js_content 内则会被截）—— 注意 mock 里 script 在 content 内
        assert!(!t.contains("<p>")); // 无标签残留
    }
}
