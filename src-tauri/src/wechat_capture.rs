// 微信文章采集器（借鉴 gitee yangfancms/wechat-url-capture，纯 Rust 重写）
// 数据源（全部只读）：
//   1. History ：%APPDATA%/Tencent/xwechat/radium/web/profiles/multitab_*/History（未加密 Chromium SQLite）
//   2. Share Data：同目录 Share Data SQLite（share_data_table：标题+公众号作者）
// 拿到 标题/URL/公众号/访问时间 → 去重 → 存 10-inbox/微信文章/ 或 AI 成卡存 30-notes/
use std::fs;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use chrono::Datelike;
#[allow(unused_imports)]
use chrono::TimeZone;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WxArticle {
    pub url: String,
    pub title: String,
    pub author: String,   // 公众号名（Share Data 补充）
    pub time: String,     // yyyy-MM-dd HH:MM
    pub visits: u32,
}

// ---------- profile 定位 ----------

#[cfg(target_os = "windows")]
fn profiles_root() -> Option<PathBuf> {
    let appdata = std::env::var("APPDATA").ok()?;
    let p = Path::new(&appdata).join("Tencent").join("xwechat").join("radium").join("web").join("profiles");
    p.exists().then_some(p)
}
#[cfg(not(target_os = "windows"))]
fn profiles_root() -> Option<PathBuf> { None } // 测试时注入

// 测试/非 Windows：允许显式指定 profiles 根目录
fn resolve_root(explicit: Option<&str>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        let pb = PathBuf::from(p);
        return pb.exists().then_some(pb);
    }
    profiles_root()
}

// profile 目录（multitab_xxx / webview_xxx / web_shell / game ...）全都要扫，
// 只挑"最新一个"会撞上空模板库或多账号的别人的库。
fn profile_dirs(root: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(rd) = fs::read_dir(root) {
        for d in rd.filter_map(|e| e.ok()) {
            let p = d.path();
            if p.is_dir() { v.push(p); }
        }
    }
    v
}

// 所有 profile 下同名文件，按体积从大到小（数据多的先读）
fn all_files(root: &Path, name: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = profile_dirs(root).into_iter().map(|d| d.join(name)).filter(|p| p.exists()).collect();
    v.sort_by_key(|p| std::cmp::Reverse(fs::metadata(p).map(|m| m.len()).unwrap_or(0)));
    v
}

// 明文 SQLite 才认；微信 4.x 新版本把 History 整库加密了（文件头不是 "SQLite format 3"）
fn is_sqlite(p: &Path) -> bool {
    use std::io::Read;
    let mut b = [0u8; 16];
    match fs::File::open(p).and_then(|mut f| f.read_exact(&mut b)) {
        Ok(_) => &b[..15] == b"SQLite format 3",
        Err(_) => false,
    }
}

// unix 秒 → "yyyy-MM-dd HH:MM"
fn unix_to_local(ts: i64) -> String {
    if ts <= 0 { return String::new(); }
    chrono::Local.timestamp_opt(ts, 0).single()
        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

// 微信文章 real_url 里的 clicktime= 就是点开时间（unix 秒）
fn clicktime_of(url: &str) -> Option<i64> {
    url.split(['?', '&']).find_map(|p| {
        p.strip_prefix("clicktime=").and_then(|v| v.split('&').next()).and_then(|v| v.parse::<i64>().ok())
    }).filter(|t| *t > 1_500_000_000 && *t < 2_500_000_000)
}

// Share Data 表没有时间列，但 id 与时间单调相关：用有 clicktime 的行做锚点插值
fn est_time(anchors: &[(i64, i64)], id: i64) -> Option<i64> {
    if anchors.is_empty() { return None; }
    match anchors.binary_search_by_key(&id, |x| x.0) {
        Ok(i) => Some(anchors[i].1),
        Err(0) => Some(anchors[0].1),
        Err(n) if n >= anchors.len() => Some(anchors[anchors.len() - 1].1),
        Err(n) => {
            let (i0, t0) = anchors[n - 1];
            let (i1, t1) = anchors[n];
            if i1 == i0 { return Some(t0); }
            let f = (id - i0) as f64 / (i1 - i0) as f64;
            Some(t0 + ((t1 - t0) as f64 * f) as i64)
        }
    }
}

// 同一篇文章多来源合并：作者/标题非空优先，时间取更晚的
fn upsert(map: &mut std::collections::HashMap<String, WxArticle>, url: String, title: String, author: String, time: String, visits: u32) {
    let k = key_of(&url);
    match map.get_mut(&k) {
        Some(a) => {
            if a.author.is_empty() && !author.is_empty() { a.author = author.clone(); }
            if (a.title.is_empty() || a.title == "(无标题)") && !title.is_empty() { a.title = title.clone(); }
            if time > a.time { a.time = time; }
            if visits > a.visits { a.visits = visits; }
        }
        None => { map.insert(k, WxArticle { url, title: if title.is_empty() { "(无标题)".into() } else { title }, author, time, visits }); }
    }
}

// 数据库被微信锁着 → 复制到临时目录再读
fn copy_db(src: &Path) -> Result<PathBuf, String> {
    let tmp = std::env::temp_dir().join("knowledge_pet_wx");
    fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let dst = tmp.join(format!("db_{}.sqlite", std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis()));
    fs::copy(src, &dst).map_err(|e| format!("历史库复制失败（微信可能未运行或权限不足）：{e}"))?;
    Ok(dst)
}

// Chromium 时间戳（100ns since 1601）→ "yyyy-MM-dd HH:MM"
fn chromium_to_local(lvt: i64) -> String {
    if lvt <= 0 { return String::new(); }
    let secs = lvt / 1_000_000 - 11_644_473_600;
    // 用 chrono（已在依赖树里 via tauri）
    use chrono::TimeZone;
    chrono::Local.timestamp_opt(secs, 0).single()
        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

// 小程序内部链接过滤
fn is_useful(url: &str) -> bool {
    let low = url.to_lowercase();
    if low.starts_with("https://wxa.wxs.qq.com") || low.starts_with("https://servicewechat.com") { return false; }
    low.starts_with("http")
}

// 公众号文章 URL 去重键：__biz+mid+idx+sn 四元组 / /s/xxx / 原样小写
pub(crate) fn key_of(url: &str) -> String {
    let mut u = url.trim().to_string();
    if let Some(h) = u.find('#') { u.truncate(h); }
    let get = |k: &str| -> Option<String> {
        u.split(['?', '&']).find_map(|p| p.strip_prefix(k).and_then(|v| v.strip_prefix('=')))
            .map(|v| v.split('&').next().unwrap_or("").to_string())
    };
    if let (Some(b), Some(m), Some(i), Some(s)) = (get("__biz"), get("mid"), get("idx"), get("sn")) {
        return format!("mp:{b}|{m}|{i}|{s}");
    }
    // /s/ 短链
    if let Some(p) = u.find("/s/") {
        let rest = &u[p + 3..];
        let id: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect();
        if id.len() >= 6 && !id.starts_with("index") { return format!("mps:{id}"); }
    }
    u.trim_end_matches('/').to_lowercase()
}

// 只挑公众号文章（有 __biz 或 /s/），避免把普通网页全灌进收件箱
fn is_mp_article(url: &str) -> bool {
    url.contains("__biz=") || {
        if let Some(p) = url.find("/s/") {
            let rest = &url[p + 3..];
            let id: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect();
            id.len() >= 6 && !id.starts_with("index")
        } else { false }
    }
}

// ---------- 第 0 期：抓取 ----------

#[derive(Serialize)]
pub struct CaptureResult {
    pub articles: Vec<WxArticle>,
    pub note: String,   // 人话说明（没装微信/找不到库等）
}

#[tauri::command]
pub async fn wx_capture(state: tauri::State<'_, crate::PetState>, days: Option<u32>, root: Option<String>) -> Result<CaptureResult, String> {
    let days = days.unwrap_or(7).clamp(1, 90);
    let root = resolve_root(root.as_deref());
    let Some(root) = root else {
        return Ok(CaptureResult { articles: vec![], note: "未找到微信 profile 目录（只在 Windows + 微信 4.x 下可用）".into() });
    };
    // rusqlite 阻塞操作丢到线程池，避免卡 UI
    let root2 = root.clone();
    let res = tauri::async_runtime::spawn_blocking(move || capture_sync(&root2, days))
        .await.map_err(|e| e.to_string())?;
    // 与知识库已有 url: 去重
    let kb = state.kb_dir.lock().unwrap().clone();
    let known = existing_urls(&kb);
    let mut result = res;
    result.articles.retain(|a| !known.contains(&key_of(&a.url)));
    Ok(result)
}

pub(crate) fn capture_sync_public(root: &Path, days: u32) -> Vec<WxArticle> { capture_sync(root, days).articles }

pub(crate) fn profiles_root_public() -> Option<PathBuf> { profiles_root() }

pub(crate) fn capture_sync(root: &Path, days: u32) -> CaptureResult {
    let mut map: std::collections::HashMap<String, WxArticle> = std::collections::HashMap::new();
    let mut hist_n = 0usize;      // 浏览库（History）条数
    let mut share_n = 0usize;     // 分享库（Share Data）条数
    let mut encrypted = 0usize;   // 被微信加密的库个数
    let mut locked = 0usize;      // 复制失败的库个数

    // ---- 来源 1：History（微信 4.0 明文可用；4.1+ 多为整库密文，文件头不是 SQLite）----
    for hist in all_files(root, "History") {
        if !is_sqlite(&hist) { encrypted += 1; continue; }
        let tmp = match copy_db(&hist) { Ok(t) => t, Err(_) => { locked += 1; continue; } };
        if let Ok(conn) = rusqlite::Connection::open(&tmp) {
            if let Ok(mut stmt) = conn.prepare("select url, title, visit_count, last_visit_time from urls order by last_visit_time desc limit 2000") {
                let rows: Vec<(String, String, i64, i64)> = stmt.query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0).unwrap_or_default(),
                        row.get::<_, String>(1).unwrap_or_default(),
                        row.get::<_, i64>(2).unwrap_or(0),
                        row.get::<_, i64>(3).unwrap_or(0),
                    ))
                }).map(|it| it.filter_map(|r| r.ok()).collect()).unwrap_or_default();
                for (url, title, vc, lvt) in rows {
                    if !is_useful(&url) || !is_mp_article(&url) { continue; }
                    hist_n += 1;
                    upsert(&mut map, url, title, String::new(), chromium_to_local(lvt), vc.max(0) as u32);
                }
            }
        }
        let _ = fs::remove_file(&tmp);
    }

    // ---- 来源 2：Share Data（明文 SQLite；微信 4.1+ 反而只剩这里能读）----
    for sd in all_files(root, "Share Data") {
        if !is_sqlite(&sd) { encrypted += 1; continue; }
        let tmp = match copy_db(&sd) { Ok(t) => t, Err(_) => { locked += 1; continue; } };
        if let Ok(conn) = rusqlite::Connection::open(&tmp) {
            // share_data 字段是 BLOB，cast 成 text 才好取；用 rowid 兼容不同版本的 id 列
            if let Ok(mut st) = conn.prepare("select rowid, url, real_url, author, cast(share_data as text) from share_data_table") {
                let rows: Vec<(i64, String, String, String, String)> = st.query_map([], |row| {
                    Ok((
                        row.get::<_, i64>(0).unwrap_or(0),
                        row.get::<_, String>(1).unwrap_or_default(),
                        row.get::<_, String>(2).unwrap_or_default(),
                        row.get::<_, String>(3).unwrap_or_default(),
                        row.get::<_, String>(4).unwrap_or_default(),
                    ))
                }).map(|it| it.filter_map(|r| r.ok()).collect()).unwrap_or_default();
                // 表里没有时间列：拿带 clicktime 的行做 id↔时间锚点，其余插值
                let mut anchors: Vec<(i64, i64)> = rows.iter()
                    .filter_map(|(id, u, real, _, _)| clicktime_of(real).or_else(|| clicktime_of(u)).map(|t| (*id, t)))
                    .collect();
                anchors.sort_by_key(|x| x.0);
                for (id, u, real, author, sd_text) in rows {
                    let url = if is_useful(&real) { real } else { u };
                    if !is_useful(&url) || !is_mp_article(&url) { continue; }
                    // Share Data 表本身没有标题列，标题/作者都在 share_data JSON 里
                    let jv = serde_json::from_str::<serde_json::Value>(&sd_text).ok();
                    let author = if author.is_empty() {
                        jv.as_ref().and_then(|j| j["brandName"].as_str().or_else(|| j["nick_name"].as_str())).map(String::from).unwrap_or_default()
                    } else { author };
                    let title = jv.as_ref()
                        .and_then(|j| j["title"].as_str())
                        .map(|s| s.trim().to_string())
                        .unwrap_or_default();
                    let time = clicktime_of(&url)
                        .or_else(|| est_time(&anchors, id))
                        .map(unix_to_local)
                        .unwrap_or_default();
                    share_n += 1;
                    upsert(&mut map, url, title, author, time, 1);
                }
            }
        }
        let _ = fs::remove_file(&tmp);
    }

    let mut arts: Vec<WxArticle> = map.into_values()
        .filter(|a| a.time.is_empty() || in_days(&a.time, days))
        .collect();
    arts.sort_by(|a, b| b.time.cmp(&a.time));
    // 时间推算不出来的放最后，最多留 200 条，别把列表灌爆
    let (mut timed, mut untimed): (Vec<WxArticle>, Vec<WxArticle>) = arts.into_iter().partition(|a| !a.time.is_empty());
    untimed.truncate(200);
    timed.extend(untimed);
    arts = timed;

    let mut note = format!("共 {} 篇公众号文章（最近 {} 天）", arts.len(), days);
    let mut src = Vec::new();
    if share_n > 0 { src.push(format!("分享库 {share_n} 条")); }
    if hist_n > 0 { src.push(format!("浏览库 {hist_n} 条")); }
    if !src.is_empty() { note.push_str(&format!("｜{}", src.join("、"))); }
    if encrypted > 0 { note.push_str(&format!("；{} 个记录库被微信加密，已跳过", encrypted)); }
    if locked > 0 { note.push_str(&format!("；{} 个库被占用未读", locked)); }
    CaptureResult { articles: arts, note }
}

fn in_days(time: &str, days: u32) -> bool {
    if time.is_empty() { return false; }
    // "yyyy-MM-dd HH:MM" → y*372+m*31+d 天数近似比较
    let ymd = time.split(' ').next().unwrap_or("");
    let p: Vec<i64> = ymd.split('-').filter_map(|x| x.parse().ok()).collect();
    if p.len() != 3 { return false; }
    let (y, m, d) = (p[0], p[1], p[2]);
    let day_num = y * 372 + m * 31 + d;
    let now = chrono::Local::now();
    let today = now.year() as i64 * 372 + now.month() as i64 * 31 + now.day() as i64;
    today - day_num <= days as i64
}

// 已有知识库里的 url:（frontmatter）集合
fn existing_urls(kb: &Path) -> std::collections::HashSet<String> {
    let mut set = std::collections::HashSet::new();
    for dir in ["10-inbox", "30-notes"] {
        if let Ok(rd) = fs::read_dir(kb.join(dir)) {
            for e in rd.filter_map(|e| e.ok()) {
                if let Ok(text) = fs::read_to_string(e.path()) {
                    for line in text.lines().take(15) {
                        if let Some(u) = line.trim().strip_prefix("url:") {
                            set.insert(key_of(u.trim().trim_matches('"')));
                        }
                        if line.trim() == "---" && text.lines().take(15).position(|l| l.trim() == "---").map(|p| p > 0).unwrap_or(false) { break; }
                    }
                }
            }
        }
    }
    set
}

// ---------- 入库 ----------

/// 勾选的文章存 10-inbox/微信文章/
#[tauri::command]
pub async fn wx_save_to_inbox(state: tauri::State<'_, crate::PetState>, articles: Vec<WxArticle>) -> Result<String, String> {
    let kb = state.kb_dir.lock().unwrap().clone();
    let dir = kb.join("10-inbox").join("微信文章");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let known = existing_urls(&kb);
    let mut saved = Vec::new();
    for a in &articles {
        let k = key_of(&a.url);
        if known.contains(&k) { continue; }
        let title = crate::sanitize(&a.title);
        let fname = format!("{}-{}.md", crate::chrono_today(), title);
        let body = format!(
            "---\ntitle: \"{}\"\ntags: [微信, 公众号]\nstatus: inbox\ndate: {}\nsource: \"微信浏览\"\nurl: \"{}\"\nauthor: \"{}\"\nvisited: \"{}\"\n---\n\n# {}\n\n（原文尚未抓取；从微信浏览记录采集，URL 见上）\n",
            a.title, crate::chrono_today(), a.url, a.author, a.time, a.title
        );
        fs::write(dir.join(&fname), body).map_err(|e| e.to_string())?;
        saved.push(fname);
    }
    if saved.is_empty() { Ok("所选文章都已存在，未重复入库".into()) }
    else { Ok(format!("已存 {} 篇到 10-inbox/微信文章/：{}", saved.len(), saved.join("、"))) }
}

/// 一键 AI 成卡（存 30-notes/）
#[tauri::command]
pub async fn wx_make_cards(state: tauri::State<'_, crate::PetState>, articles: Vec<WxArticle>) -> Result<String, String> {
    let cfg = state.config.lock().unwrap().clone();
    if cfg.api_key.is_empty() { return Err("AI 未配置（设置→AI 模型）".into()); }
    let kb = state.kb_dir.lock().unwrap().clone();
    let dir = kb.join("30-notes");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let known = existing_urls(&kb);
    let mut done = Vec::new();
    for a in &articles {
        if known.contains(&key_of(&a.url)) { continue; }
        let author = if a.author.is_empty() { "未知".to_string() } else { a.author.clone() };
        let prompt = if cfg.prompt_wechat.trim().is_empty() {
            format!(
            "你是知识库管理员。下面是一篇微信公众号文章的标题和链接（没有正文）。基于标题推测主题，写一张「待补全」知识卡。\n输出 frontmatter + 正文，格式严格如下（YAML）:\n---\ntitle: 卡片标题（10字内）\ntags: [微信, 公众号, 待补全]\nstatus: seed\nsource: \"微信: {}\"\nurl: \"{}\"\n---\n\n# 标题说了什么\n\n（从标题推断的核心主题，50字内，注明「基于标题推测，待读原文补全」）\n\n## 下一步行动\n\n- [ ] 打开原文阅读并补全此卡\n\n文章标题：{}\n公众号：{}",
            a.url, a.url, a.title, &author
        )
        } else {
            cfg.prompt_wechat
                .replace("{title}", &a.title)
                .replace("{author}", &author)
                .replace("{url}", &a.url)
        };
        let content = crate::ai_chat(&cfg, &prompt).await?;
        let body = match content.find("\n---\n").or_else(|| content.starts_with("---").then(|| 0)) {
            Some(p) => &content[p.max(0)..],
            None => &content[..],
        };
        let title2 = body.lines().find(|l| l.starts_with("title:")).map(|l| l[6..].trim().trim_matches('"').to_string()).unwrap_or_else(|| a.title.clone());
        let fname = format!("{}-{}.md", crate::chrono_today(), crate::sanitize(&title2));
        fs::write(dir.join(&fname), body.trim_start()).map_err(|e| e.to_string())?;
        done.push(fname);
    }
    if done.is_empty() { Ok("所选文章都已存在，未生成".into()) }
    else { Ok(format!("已生成 {} 张知识卡：{}", done.len(), done.join("、"))) }
}

// ---------- 第 2 期：导出给评论寻客 ----------

#[derive(Serialize)]
pub struct GzhSourceExport { pub name: String, pub biz: String }

/// 导出采集到的公众号（biz+名字）为 JSON 文件，供评论寻客「导入来源」用
#[tauri::command]
pub async fn wx_export_gzh(state: tauri::State<'_, crate::PetState>) -> Result<String, String> {
    let root = resolve_root(None);
    let Some(root) = root else { return Err("只在 Windows + 微信 4.x 下可用".into()); };
    let root2 = root.clone();
    let arts = tauri::async_runtime::spawn_blocking(move || capture_sync(&root2, 90).articles)
        .await.map_err(|e| e.to_string())?;
    // 从 URL 提取 biz → 公众号（作者名优先，空则用 biz 前缀）
    let mut map: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for a in &arts {
        let get = |k: &str| -> Option<String> {
            a.url.split(['?', '&']).find_map(|p| p.strip_prefix(k).and_then(|v| v.strip_prefix('=')))
                .map(|v| v.split('&').next().unwrap_or("").to_string())
        };
        if let Some(biz) = get("__biz") {
            let name = if a.author.is_empty() { format!("公众号({})", &biz[..biz.len().min(8)]) } else { a.author.clone() };
            map.entry(biz).or_insert(name);
        }
    }
    if map.is_empty() { return Ok("最近 90 天没采集到公众号文章".into()); }
    let list: Vec<GzhSourceExport> = map.into_iter().map(|(biz, name)| GzhSourceExport { name, biz }).collect();
    let kb = state.kb_dir.lock().unwrap().clone();
    let out = kb.join("exports");
    fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let path = out.join("gzh-sources.json");
    fs::write(&path, serde_json::to_string_pretty(&list).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    Ok(format!("已导出 {} 个公众号到 {}（评论寻客 → 添加来源 → 从文件导入）", list.len(), path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_of() {
        assert_eq!(key_of("https://mp.weixin.qq.com/s?__biz=MzA1&mid=2&idx=1&sn=abc#rd"), "mp:MzA1|2|1|abc");
        assert_eq!(key_of("https://mp.weixin.qq.com/s/AbCdEf12"), "mps:AbCdEf12");
        assert_eq!(key_of("https://mp.weixin.qq.com/s?__biz=MzA1&mid=2&idx=1&sn=abc"), key_of("https://mp.weixin.qq.com/s?__biz=MzA1&mid=2&idx=1&sn=abc&from=timeline"));
    }

    #[test]
    fn test_is_mp_article() {
        assert!(is_mp_article("https://mp.weixin.qq.com/s?__biz=MzA1&mid=2&idx=1&sn=abc"));
        assert!(is_mp_article("https://mp.weixin.qq.com/s/AbCdEf12"));
        assert!(!is_mp_article("https://www.zhihu.com/question/123"));
        assert!(!is_mp_article("https://wxa.wxs.qq.com/cgi-bin/xxx"));
    }

    #[test]
    fn test_chromium_to_local() {
        // 2026-11-07 00:00:00 UTC ≈ 133700000000000000 (近似值只验证格式)
        let s = chromium_to_local(133_700_000_000_000_000);
        assert!(s.len() == 16 && s.chars().nth(4) == Some('-') && s.contains(':'), "got {s}");
        assert_eq!(chromium_to_local(0), "");
    }

    #[test]
    fn test_in_days() {
        let now = chrono::Local::now();
        let recent = now.format("%Y-%m-%d %H:%M").to_string();
        assert!(in_days(&recent, 7));
        assert!(!in_days("2020-01-01 10:00", 7));
        assert!(!in_days("", 7));
    }

    #[test]
    fn test_capture_sync_mock() {
        // 造 mock profile（模拟微信 multitab_1/History + Share Data）
        let base = std::env::temp_dir().join(format!("wx_mock_{}", std::process::id()));
        let prof = base.join("multitab_1");
        fs::create_dir_all(&prof).unwrap();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        let lvt = |secs_ago: i64| (now - secs_ago) * 1_000_000 + 11_644_473_600 * 1_000_000;
        let con = rusqlite::Connection::open(prof.join("History")).unwrap();
        con.execute_batch(&format!(r#"
            create table urls (url text, title text, visit_count integer, last_visit_time integer);
            insert into urls values ('https://mp.weixin.qq.com/s?__biz=MzA1&mid=2&idx=1&sn=abc', '公众号获客 3 阶段', 3, {recent});
            insert into urls values ('https://mp.weixin.qq.com/s/AbCdEf12', '靠内容翻身', 1, {recent2});
            insert into urls values ('https://mp.weixin.qq.com/s?__biz=MzA1&mid=2&idx=1&sn=abc&from=timeline', '转发版', 1, {recent});
            insert into urls values ('https://wxa.wxs.qq.com/cgi-bin/x', '小程序', 1, {recent});
            insert into urls values ('https://www.zhihu.com/q', '知乎', 1, {recent});
            insert into urls values ('https://mp.weixin.qq.com/s?__biz=MzB2&mid=1&idx=1&sn=old', '30天前的文章', 1, {old});
        "#, recent = lvt(7200), recent2 = lvt(7200 * 24 * 3), old = lvt(7200 * 24 * 30))).unwrap();
        drop(con);
        let con2 = rusqlite::Connection::open(prof.join("Share Data")).unwrap();
        con2.execute_batch(r#"
            create table share_data_table (url text, real_url text, author text, share_data text);
            insert into share_data_table values ('https://mp.weixin.qq.com/s?__biz=MzA1&mid=2&idx=1&sn=abc', '', '', '{"nick_name":"地瓜工作室"}');
            insert into share_data_table values ('', 'https://mp.weixin.qq.com/s/AbCdEf12', '姜胡说', '{}');
        "#).unwrap();
        drop(con2);

        let r = capture_sync(&base, 7);
        assert!(r.articles.len() == 2, "expect 2 articles, got {}: {:?}", r.articles.len(), r.articles);
        // 四元组去重（转发版被合并）
        let a1 = r.articles.iter().find(|a| a.url.contains("sn=abc")).unwrap();
        assert_eq!(a1.title, "公众号获客 3 阶段");
        assert_eq!(a1.author, "地瓜工作室"); // share_data JSON nick_name
        let a2 = r.articles.iter().find(|a| a.url.contains("/s/AbCdEf12")).unwrap();
        assert_eq!(a2.author, "姜胡说"); // 明文 author
        // 30 天前的被 7 天窗口过滤
        assert!(r.articles.iter().all(|a| !a.title.contains("30天前")));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn test_existing_urls() {
        // 简单自测：临时目录建卡后 existing_urls 能匹配
        let kb = std::env::temp_dir().join(format!("kpw_test_{}", std::process::id()));
        let dir = kb.join("30-notes"); fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.md"), "---\ntitle: t\nurl: \"https://mp.weixin.qq.com/s?__biz=MzA1&mid=2&idx=1&sn=abc\"\n---\n\n正文").unwrap();
        let set = existing_urls(&kb);
        assert!(set.contains("mp:MzA1|2|1|abc"));
        let _ = fs::remove_dir_all(&kb);
    }
}
