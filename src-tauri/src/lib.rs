// Knowledge Pet 知识收藏桌宠
// 核心：托盘+桌宠、单击收剪贴板→inbox→冒泡、URL抓取队列、AI整理、设置(api/model)
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, State};

mod integrations;
mod wechat_capture;
mod watch;

#[derive(Serialize, Clone)]
pub struct Item {
    pub id: String,
    pub file: String,      // 相对知识库的路径
    pub title: String,
    pub kind: String,      // url | text
    pub url: String,
    pub status: String,    // seed(新收) | fetched(已爬正文) | read(已读) | carded(已成卡)
    pub created_at: String,
    pub error: String,
    #[serde(default)]
    pub keywords: String,  // 逗号分隔（frontmatter keywords，AI 提炼）
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct ProviderCfg {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub image_model: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct AiConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    #[serde(default = "default_hotkey")]
    pub hotkey: String,          // 主动抓取热键，默认 Ctrl+Alt+C
    #[serde(default)]
    pub kb_dir_override: String, // 知识库目录（空=默认 ~/knowledge-base）
    // ---- 集成（v0.4）----
    #[serde(default)]
    pub feishu_webhook: String,      // 飞书自定义机器人 webhook
    #[serde(default)]
    pub feishu_push_daily: bool,     // 每日冒泡推送开关
    #[serde(default)]
    pub notion_token: String,        // Notion internal connection token
    #[serde(default)]
    pub notion_db_id: String,        // Notion 数据库 ID（收集箱）
    // ---- 成卡/创作预设（v0.9.3：每类内容专属模板，空=内置默认）----
    #[serde(default)]
    pub prompt_article: String,   // 文章成卡模板 {url} {content}
    #[serde(default)]
    pub prompt_wechat: String,    // 微信文章成卡模板 {url} {content}
    #[serde(default)]
    pub prompt_quote: String,     // 引用创作模板 {goal} {snapshot}
    #[serde(default)]
    pub prompt_topics: String,    // 跨卡选题模板 {digest}
    #[serde(default)]
    pub prompt_daily: String,     // 每日冒泡文案模板 {date} {title} {conclusion} {todos}
    // ---- 多厂商模型配置（v0.9.5：厂商卡片+文本/图片模型分组）----
    #[serde(default)]
    pub providers: Vec<ProviderCfg>,
    #[serde(default)]
    pub active_provider: usize,
    #[serde(default)]
    pub image_provider: i64,
    #[serde(default)]
    pub image_model: String,
}

fn default_hotkey() -> String { "Ctrl+Alt+C".into() }

impl Default for AiConfig {
    fn default() -> Self {
        AiConfig { base_url: String::new(), api_key: String::new(), model: String::new(), hotkey: default_hotkey(), kb_dir_override: String::new(), feishu_webhook: String::new(), feishu_push_daily: false, notion_token: String::new(), notion_db_id: String::new(), prompt_article: String::new(), prompt_wechat: String::new(), prompt_quote: String::new(), prompt_topics: String::new(), prompt_daily: String::new(), providers: Vec::new(), active_provider: 0, image_provider: -1, image_model: String::new() }
    }
}

// 常用网址收藏
#[derive(Serialize, Deserialize, Clone)]
pub struct Bookmark { pub title: String, pub url: String }

fn load_bookmarks(kb: &PathBuf) -> Vec<Bookmark> {
    fs::read_to_string(kb.join(".pet-bookmarks.json")).ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_bookmarks(kb: &PathBuf, b: &[Bookmark]) {
    let _ = fs::write(kb.join(".pet-bookmarks.json"), serde_json::to_string_pretty(b).unwrap());
}

// 抓取队列断点续传：queue 落盘 .pet-queue.json，重启自动恢复
fn save_queue(state: &PetState) {
    let kb = state.kb_dir.lock().unwrap().clone();
    let q = state.queue.lock().unwrap().clone();
    let _ = fs::write(kb.join(".pet-queue.json"), serde_json::to_string(&q).unwrap_or_default());
}
fn load_queue(kb: &PathBuf) -> Vec<String> {
    fs::read_to_string(kb.join(".pet-queue.json")).ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub struct PetState {
    pub kb_dir: Mutex<PathBuf>,   // 可迁移，加锁
    pub config: Mutex<AiConfig>,
    pub queue: Mutex<Vec<String>>, // 待爬 URL 队列（id）
}

fn now_id() -> String {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
    format!("kp{}", t)
}

fn load_config(kb: &PathBuf) -> AiConfig {
    let mut c: AiConfig = fs::read_to_string(kb.join(".pet-config.json")).ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    // 旧单组配置 → 迁移成第一个厂商（无缝升级）
    if c.providers.is_empty() && (!c.base_url.is_empty() || !c.api_key.is_empty()) {
        c.providers.push(ProviderCfg {
            name: "默认厂商".into(),
            base_url: c.base_url.clone(),
            api_key: c.api_key.clone(),
            model: c.model.clone(),
            image_model: String::new(),
        });
    }
    if c.active_provider >= c.providers.len() { c.active_provider = 0; }
    if c.image_provider >= c.providers.len() as i64 { c.image_provider = -1; }
    c
}

/// 当前生效的文本模型厂商配置（providers 空=旧字段）
pub fn active_cfg(cfg: &AiConfig) -> (&String, &String, &String) {
    if let Some(p) = cfg.providers.get(cfg.active_provider) {
        (&p.base_url, &p.api_key, &p.model)
    } else {
        (&cfg.base_url, &cfg.api_key, &cfg.model)
    }
}

fn save_config(kb: &PathBuf, c: &AiConfig) {
    let _ = fs::write(kb.join(".pet-config.json"), serde_json::to_string_pretty(c).unwrap());
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct WinPos { pub x: i32, pub y: i32 }

fn load_win_pos(kb: &PathBuf) -> Option<WinPos> {
    fs::read_to_string(kb.join(".pet-window.json")).ok()
        .and_then(|s| serde_json::from_str(&s).ok())
}
fn save_win_pos(kb: &PathBuf, p: &WinPos) {
    let _ = fs::write(kb.join(".pet-window.json"), serde_json::to_string(p).unwrap());
}

fn chrono_today() -> String {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() + 8 * 3600;
    let (y, m, d) = civil_from_days((t / 86400) as i64);
    format!("{:04}{:02}{:02}", y, m, d)
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub(crate) fn sanitize(s: &str) -> String {
    s.chars().filter(|c| !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')).take(40).collect()
}

// 极简 frontmatter 字段读取
fn fm_field(text: &str, key: &str) -> Option<String> {
    let mut in_fm = false;
    for line in text.lines() {
        if line.trim() == "---" { if in_fm { break; } in_fm = true; continue; }
        if in_fm {
            if let Some(v) = line.strip_prefix(&format!("{}:", key)) {
                let v = v.trim().trim_matches('"').trim_matches('\'').to_string();
                if !v.is_empty() { return Some(v); }
            }
        }
    }
    None
}

// 写 frontmatter 字段（无 frontmatter 则补一个）
fn set_fm_field(path: &PathBuf, key: &str, val: &str) {
    let text = fs::read_to_string(path).unwrap_or_default();
    let mut out = String::new();
    let mut in_fm = false;
    let mut fm_done = false;
    let mut replaced = false;
    for line in text.lines() {
        if line.trim() == "---" {
            out.push_str("---\n");
            if in_fm {
                if !replaced { out.push_str(&format!("{}: {}\n", key, val)); }
                in_fm = false; fm_done = true;
            } else { in_fm = true; }
            continue;
        }
        if in_fm && line.starts_with(&format!("{}:", key)) {
            out.push_str(&format!("{}: {}\n", key, val)); replaced = true;
        } else {
            out.push_str(line); out.push('\n');
        }
    }
    if !fm_done { out = format!("---\n{}: {}\n---\n\n", key, val) + &out; }
    let _ = fs::write(path, out);
}

// 扫描 10-inbox 全部条目
#[tauri::command]
fn list_items(state: State<PetState>) -> Vec<Item> {
    let kb = state.kb_dir.lock().unwrap().clone();
    scan_items(&kb)
}

fn scan_items(kb: &PathBuf) -> Vec<Item> {
    let inbox = kb.join("10-inbox");
    let mut out = Vec::new();
    if let Ok(rd) = fs::read_dir(&inbox) {
        let entries: Vec<_> = rd.filter_map(|e| e.ok()).collect();
        let mut files: Vec<_> = entries.iter().map(|e| e.path())
            .filter(|p| p.extension().map_or(false, |x| x == "md"))
            .chain(entries.iter().filter(|e| e.path().is_dir())
                .filter_map(|d| fs::read_dir(d.path()).ok()
                    .and_then(|rd2| rd2.filter_map(|e| e.ok())
                        .find(|e| e.file_name() == "index.md").map(|e| e.path()))))
            .collect();
        files.sort();
        for p in files {
            let rel = p.strip_prefix(&inbox).unwrap_or(&p).to_string_lossy().to_string();
            let name = rel.trim_end_matches(".md").to_string();
            let text = fs::read_to_string(&p).unwrap_or_default();
            let status = fm_field(&text, "status").unwrap_or_else(|| "seed".into());
            let kind = fm_field(&text, "kind").unwrap_or_else(|| "text".into());
            let url = fm_field(&text, "url").unwrap_or_default();
            let title = text.lines()
                .find(|l| l.starts_with("# ") && !l.starts_with("#!"))
                .map(|l| l[2..].trim().to_string())
                .unwrap_or_else(|| p.file_name().unwrap().to_string_lossy().trim_end_matches(".md").to_string());
            let keywords = fm_field(&text, "keywords").unwrap_or_default().trim_matches(|c| c == '[' || c == ']').to_string();
            out.push(Item {
                id: name.clone(),
                file: format!("10-inbox/{}", rel),
                title, kind, url, status,
                created_at: name.chars().take(12).collect(),
                error: String::new(),
                keywords,
            });
        }
    }
    out.reverse();
    out
}

// 收藏：文本或 URL → 10-inbox。去重 + URL 入抓取队列
#[tauri::command]
fn capture(state: State<PetState>, text: String) -> Result<Item, String> {
    let text = text.trim().to_string();
    if text.is_empty() { return Err("剪贴板是空的".into()); }
    let kb = state.kb_dir.lock().unwrap().clone();
    let is_url = text.starts_with("http://") || text.starts_with("https://");
    for it in scan_items(&kb) {
        if (is_url && it.url == text) || (!is_url && it.title == text.lines().next().unwrap_or("")) {
            return Err(format!("已收过：{}", it.title));
        }
    }
    let id = now_id();
    let kind = if is_url { "url" } else { "text" };
    let first = text.lines().next().unwrap_or("").chars().take(60).collect::<String>();
    let fname = if is_url {
        format!("{}-网页剪藏.md", id)
    } else {
        format!("{}-{}.md", id, sanitize(&first))
    };
    let body = if is_url {
        format!("---\nkind: url\nurl: {}\nstatus: seed\ncreated: {}\n---\n\n# 网页剪藏\n\n{}", text, chrono_today(), first)
    } else {
        format!("---\nkind: text\nstatus: seed\ncreated: {}\n---\n\n# {}\n\n{}", chrono_today(), first, text)
    };
    let full = kb.join("10-inbox").join(&fname);
    fs::create_dir_all(full.parent().unwrap()).map_err(|e| e.to_string())?;
    fs::write(&full, body).map_err(|e| e.to_string())?;
    if is_url { state.queue.lock().unwrap().push(id.clone()); save_queue(&state); }
    Ok(Item {
        id: fname.trim_end_matches(".md").to_string(), // 与 scan_items 一致（文件名 id）
        file: format!("10-inbox/{}", fname),
        title: if is_url { "网页剪藏".into() } else { first },
        kind: kind.into(),
        url: if is_url { text } else { String::new() },
        status: "seed".into(),
        created_at: chrono_today(),
        error: String::new(),
        keywords: String::new(),
    })
}

// 抓取队列：取一条 URL → 拉 → 提正文 → 写回 → status=fetched
#[tauri::command]
async fn process_queue(state: State<'_, PetState>) -> Result<String, String> {
    let id = state.queue.lock().unwrap().pop().ok_or("队列空")?;
    // 断点续传：pop 后立即落盘剩余队列
    save_queue(&state);
    eprintln!("[pet] process_queue pop id={}", id);
    let kb = state.kb_dir.lock().unwrap().clone();
    let cfg = state.config.lock().unwrap().clone();
    let items = scan_items(&kb);
    eprintln!("[pet] items: {:?}", items.iter().map(|i| i.id.clone()).collect::<Vec<_>>());
    let it = items.iter().find(|i| i.id == id || i.id.starts_with(&id)).ok_or(format!("条目不存在：{}", id))?;
    let url = it.url.clone();
    let path = kb.join(&it.file);
    eprintln!("[pet] fetching url={:?} path={:?}", url, path);
    let resp = reqwest::get(&url).await.map_err(|e| {
        eprintln!("[pet] fetch error: {}", e);
        set_fm_field(&path, "status", "fetch-failed");
        format!("抓取失败：{}", e)
    })?;
    eprintln!("[pet] fetched status={}", resp.status());
    let html = resp.text().await.map_err(|e| e.to_string())?;
    eprintln!("[pet] html len={}", html.len());
    let (title, content) = extract_article(&html);
    eprintln!("[pet] extracted title={:?} content_len={} writing...", title, content.len());
    // 每篇文章单独文件夹：10-inbox/[作者/]日期-标题/index.md + images/（借鉴数海丹心下载工具+AISet 素材库按作者归档）
    // 微信文章从 html 里抠公众号名（var nickname = "xx" 或 window.nickname）
    let author = html.find("nickname").and_then(|p| {
        let seg = &html[p..(p + 120).min(html.len())];
        // nickname = "xx" 或 nickname: "xx" —— 跳过 = / : 后找引号
        let after = seg.find(|c| c == '=' || c == ':')?;
        let rest = &seg[after..];
        let q1 = rest.find('"')?; let rest2 = &rest[q1 + 1..]; let q2 = rest2.find('"')?;
        let n = rest2[..q2].trim();
        (!n.is_empty() && n.len() < 60).then(|| n.to_string())
    }).unwrap_or_default();
    let folder_rel = if author.is_empty() {
        sanitize(&format!("{}-{}", chrono_today(), title.chars().take(40).collect::<String>()))
    } else {
        format!("{}/{}", sanitize(&author), sanitize(&format!("{}-{}", chrono_today(), title.chars().take(40).collect::<String>())))
    };
    let dir = kb.join("10-inbox").join(&folder_rel);
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // 下载文中图片（还原懒加载 data-src），失败不阻塞
    let img_dir = dir.join("images");
    let mut saved_imgs = 0;
    let lower = html.to_lowercase();
    let mut img_urls: Vec<String> = Vec::new();
    for attr in ["data-src", "src"] {
        let mut from = 0;
        while let Some(p) = lower[from..].find(&format!("{}=\"{}", attr, "http")) {
            let start = from + p + attr.len() + 2;
            let end = html[start..].find('"').map(|x| start + x).unwrap_or(start);
            let u = html[start..end.min(html.len())].to_string();
            if (u.starts_with("http://") || u.starts_with("https://")) && !img_urls.contains(&u) {
                let tail = u.rsplit('.').next().unwrap_or("png").to_lowercase();
                if ["png","jpg","jpeg","gif","webp"].contains(&tail.as_str()) { img_urls.push(u); }
            }
            from = end.max(from + 1);
            if img_urls.len() >= 30 { break; }
        }
    }
    let cli = reqwest::Client::new();
    let mut idx = 1;
    for u in &img_urls {
        if let Ok(bytes) = cli.get(u).send().await {
            if let Ok(b) = bytes.bytes().await {
                if b.len() > 1024 {
                    let ext = u.rsplit('.').next().unwrap_or("png").split('?').next().unwrap_or("png");
                    let _ = fs::create_dir_all(&img_dir);
                    if fs::write(img_dir.join(format!("img-{:02}.{}", idx, ext)), &b).is_ok() { saved_imgs += 1; idx += 1; }
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    let new_text = if author.is_empty() {
        format!(
            "---\nkind: url\nurl: {}\nstatus: fetched\ncreated: {}\n---\n\n# {}\n\n> 来源：{}\n> 附件：images/ 共 {} 张\n\n{}",
            url, chrono_today(), title, url, saved_imgs, content
        )
    } else {
        format!(
            "---\nkind: url\nurl: {}\nauthor: {}\nstatus: fetched\ncreated: {}\n---\n\n# {}\n\n> 来源：{}（{}）\n> 附件：images/ 共 {} 张\n\n{}",
            url, author, chrono_today(), title, url, author, saved_imgs, content
        )
    };
    fs::write(dir.join("index.md"), &new_text).map_err(|e| { eprintln!("[pet] write err: {}", e); e.to_string() })?;
    // AI 提炼关键词写进 frontmatter（失败不影响主流程）
    let mut kw_msg = String::new();
    let (_, key, mdl) = active_cfg(&cfg);
    if !key.is_empty() && !mdl.is_empty() {
        let kw_prompt = format!("读下面的文章，提炼 3-5 个主题关键词。只输出关键词，逗号分隔，不要任何解释。\n\n标题：{}\n{}",
            title, content.chars().take(3000).collect::<String>());
        if let Ok(kw) = ai_chat(&cfg, &kw_prompt).await {
            let kws: Vec<String> = kw.split(|c: char| c == ',' || c == '，' || c == '\n' || c == '、')
                .map(|x| x.trim().trim_start_matches('#').trim().to_string())
                .filter(|x| !x.is_empty() && x.chars().count() <= 20).take(5).collect();
            if !kws.is_empty() {
                let _ = fs::write(dir.join("index.md"), new_text.replacen(
                    "---\n", &format!("---\nkeywords: [{}]\n", kws.join(", ")), 1));
                kw_msg = format!("，关键词：{}", kws.join("/"));
            }
        }
    }
    // 删掉旧的 seed 占位文件（scan_items 会把文件夹下的 index.md 当新条目）
    let _ = fs::remove_file(&path);
    eprintln!("[pet] write ok {} bytes, {} imgs", new_text.len(), saved_imgs);
    Ok(format!("已抓取：{}（附 {} 张图）{}", title, saved_imgs, kw_msg))
}

// 极简正文提取
fn extract_article(html: &str) -> (String, String) {
    let title = html.split("<title>").nth(1)
        .and_then(|s| s.split("</title>").next())
        .unwrap_or("无标题").trim().to_string();
    let mut s = html.to_string();
    for tag in ["script", "style", "nav", "header", "footer", "aside"] {
        let open = format!("<{}", tag);
        let close = format!("</{}>", tag);
        loop {
            let lower = s.to_lowercase();
            let a = match lower.find(&open) { Some(x) => x, None => break };
            let c = lower[a..].find(&close).map(|x| a + x + close.len()).unwrap_or((a + 20000).min(s.len()));
            if c > s.len() { break; }
            s.replace_range(a..c, "");
        }
    }
    let mut out = String::new();
    let mut depth = 0usize;
    for ch in s.chars() {
        match ch {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    let mut lines: Vec<&str> = out.lines().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
    lines.truncate(400);
    (title, lines.join("\n\n"))
}

// AI 整理 → 30-notes 知识卡，原条目 status=carded
#[tauri::command]
async fn ai_make_card(state: State<'_, PetState>, id: String) -> Result<String, String> {
    let cfg = state.config.lock().unwrap().clone();
    if cfg.api_key.is_empty() || cfg.base_url.is_empty() { return Err("请先在设置里配置 API".into()); }
    let kb = state.kb_dir.lock().unwrap().clone();
    let items = scan_items(&kb);
    let it = items.iter().find(|i| i.id == id).ok_or("条目不存在")?;
    let text = fs::read_to_string(kb.join(&it.file)).map_err(|e| e.to_string())?;
    let prompt = if cfg.prompt_article.trim().is_empty() {
        format!(
            "你是知识库管理员。把下面的素材整理成一张知识卡。\n输出 frontmatter + 正文，格式严格如下（YAML）:\n---\ntitle: 卡片标题（10字内）\ntags: [标签1, 标签2]\nstatus: seed\nsource: \"{}\"\n---\n\n# 一句话结论\n\n（最核心的判断一句话）\n\n## 展开\n\n（要点、例子，markdown 列表，300字内）\n\n## 下一步行动\n\n- [ ] 一个可执行的下一步\n\n素材：\n{}",
            it.url, text
        )
    } else {
        cfg.prompt_article.replace("{url}", &it.url).replace("{content}", &text)
    };
    let cli = reqwest::Client::new();
    let resp = cli.post(format!("{}/chat/completions", cfg.base_url.trim_end_matches('/')))
        .bearer_auth(&cfg.api_key)
        .json(&serde_json::json!({
            "model": cfg.model,
            "messages": [{"role":"user","content":prompt}]
        }))
        .send().await.map_err(|e| format!("AI 请求失败：{}", e))?;
    let j: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let content = j.pointer("/choices/0/message/content").and_then(|v| v.as_str())
        .ok_or_else(|| format!("AI 返回异常：{}", serde_json::to_string(&j).unwrap_or_default().chars().take(200).collect::<String>()))?
        .to_string();
    let cards_dir = kb.join("30-notes");
    fs::create_dir_all(&cards_dir).map_err(|e| e.to_string())?;
    // AI 有时会包 ```yaml 围栏 / 前置闲话——定位真正的 frontmatter 起点
    let body = match content.find("\n---\n").or_else(|| content.starts_with("---").then(|| 0)) {
        Some(p) => &content[p.max(0)..],
        None => &content[..],
    };
    let title = body.lines().find(|l| l.starts_with("title:")).map(|l| l[6..].trim().trim_matches('"').to_string()).unwrap_or_else(|| it.title.clone());
    let fname = format!("{}-{}.md", chrono_today(), sanitize(&title));
    fs::write(cards_dir.join(&fname), body.trim_start()).map_err(|e| e.to_string())?;
    set_fm_field(&kb.join(&it.file), "status", "carded");
    Ok(format!("知识卡已生成：{}", fname))
}

#[tauri::command]
fn mark_read(state: State<PetState>, id: String) -> Result<(), String> {
    let kb = state.kb_dir.lock().unwrap().clone();
    let items = scan_items(&kb);
    let it = items.iter().find(|i| i.id == id).ok_or("条目不存在")?;
    set_fm_field(&kb.join(&it.file), "status", "read");
    Ok(())
}

// 删除收藏条目（移到知识库 90-archive 下 .trash-<日期>/，可找回）
#[tauri::command]
fn delete_item(state: State<PetState>, id: String) -> Result<String, String> {
    let kb = state.kb_dir.lock().unwrap().clone();
    let items = scan_items(&kb);
    let it = items.iter().find(|i| i.id == id).ok_or("条目不存在")?;
    let trash = kb.join("90-archive").join(format!(".trash-{}", chrono_today()));
    fs::create_dir_all(&trash).map_err(|e| e.to_string())?;
    let fname = it.file.split('/').next_back().unwrap_or("unknown.md").to_string();
    let dest = trash.join(&fname);
    // 同名冲突加序号
    let mut dest = dest.clone();
    let mut n = 1;
    while dest.exists() { n += 1; dest = trash.join(format!("{}.{}.md", fname.trim_end_matches(".md"), n)); }
    fs::rename(kb.join(&it.file), &dest).map_err(|e| e.to_string())?;
    Ok(format!("已删除：{}", it.title))
}

// ---- 常用网址 ----
#[tauri::command]
fn list_bookmarks(state: State<PetState>) -> Vec<Bookmark> {
    let kb = state.kb_dir.lock().unwrap().clone();
    load_bookmarks(&kb)
}

#[tauri::command]
fn add_bookmark(state: State<PetState>, title: String, url: String) -> Result<(), String> {
    let url = url.trim().to_string();
    if !url.starts_with("http://") && !url.starts_with("https://") { return Err("网址要以 http(s):// 开头".into()); }
    let kb = state.kb_dir.lock().unwrap().clone();
    let mut b = load_bookmarks(&kb);
    if b.iter().any(|x| x.url == url) { return Err("已在常用网址里".into()); }
    let title = if title.trim().is_empty() { url.trim_start_matches("https://").trim_start_matches("http://").chars().take(30).collect() } else { title.trim().to_string() };
    b.insert(0, Bookmark { title, url });
    save_bookmarks(&kb, &b);
    Ok(())
}

#[tauri::command]
fn remove_bookmark(state: State<PetState>, url: String) -> Result<(), String> {
    let kb = state.kb_dir.lock().unwrap().clone();
    let mut b = load_bookmarks(&kb);
    b.retain(|x| x.url != url);
    save_bookmarks(&kb, &b);
    Ok(())
}

#[tauri::command]
fn get_config(state: State<PetState>) -> AiConfig { state.config.lock().unwrap().clone() }

#[tauri::command]
fn set_config(state: State<PetState>, base_url: String, api_key: String, model: String, hotkey: Option<String>,
    prompt_article: Option<String>, prompt_wechat: Option<String>, prompt_quote: Option<String>,
    prompt_topics: Option<String>, prompt_daily: Option<String>,
    providers: Option<Vec<ProviderCfg>>, active_provider: Option<usize>,
    image_provider: Option<i64>, image_model: Option<String>) {
    let mut c = state.config.lock().unwrap();
    c.base_url = base_url; c.api_key = api_key; c.model = model;
    if let Some(h) = hotkey { if !h.trim().is_empty() { c.hotkey = h.trim().to_string(); } }
    // 模板：传 None=不改；传 Some("")=清空恢复默认
    if let Some(t) = prompt_article { c.prompt_article = t; }
    if let Some(t) = prompt_wechat { c.prompt_wechat = t; }
    if let Some(t) = prompt_quote { c.prompt_quote = t; }
    if let Some(t) = prompt_topics { c.prompt_topics = t; }
    if let Some(t) = prompt_daily { c.prompt_daily = t; }
    // 多厂商：传 None=不改
    if let Some(ps) = providers { c.providers = ps; }
    if let Some(i) = active_provider { if i < c.providers.len().max(1) { c.active_provider = i; } }
    if let Some(i) = image_provider { c.image_provider = i; }
    if let Some(m) = image_model { c.image_model = m; }
    // providers 有值时把文本厂商的值回写旧字段（兼容所有旧调用点）
    if let Some(p) = c.providers.get(c.active_provider).cloned() {
        c.base_url = p.base_url; c.api_key = p.api_key; c.model = p.model;
    }
    let kb = state.kb_dir.lock().unwrap().clone();
    save_config(&kb, &c);
}

// 知识库目录迁移：把 10-inbox/30-notes/90-archive + 配置搬到新目录，新目录若不存在则创建
#[tauri::command]
fn move_kb_dir(app: AppHandle, state: State<'_, PetState>, new_dir: String) -> Result<String, String> {
    let new_dir = new_dir.trim().to_string();
    if new_dir.is_empty() { return Err("目录不能为空".into()); }
    let nd = PathBuf::from(&new_dir);
    let old_kb = state.kb_dir.lock().unwrap().clone();
    if nd == old_kb { return Ok("目录未变化".into()); }
    fs::create_dir_all(nd.join("10-inbox")).map_err(|e| format!("创建目录失败：{}", e))?;
    fs::create_dir_all(nd.join("30-notes")).ok();
    fs::create_dir_all(nd.join("90-archive")).ok();
    // 迁移三个内容目录（不存在就跳过）
    let mut moved = Vec::new();
    for sub in ["10-inbox", "30-notes", "90-archive"] {
        let s = old_kb.join(sub);
        if !s.exists() { continue; }
        let d = nd.join(sub);
        if d.exists() {
            // 合并：逐文件搬，冲突加序号
            if let Ok(rd) = fs::read_dir(&s) {
                for e in rd.filter_map(|e| e.ok()) {
                    let f = e.file_name().to_string_lossy().to_string();
                    let mut dest = d.join(&f);
                    let mut n = 1;
                    while dest.exists() { n += 1; dest = d.join(format!("{}_{}.md", f.trim_end_matches(".md"), n)); }
                    let _ = fs::rename(e.path(), &dest);
                }
            }
            let _ = fs::remove_dir(&s);
        } else {
            fs::rename(&s, &d).map_err(|e| format!("移动 {} 失败：{}", sub, e))?;
        }
        moved.push(sub);
        use tauri::Emitter;
        let _ = Emitter::emit_to(&app, "main", "kb-move-progress", format!("{}|{}", moved.len(), 3));
    }
    // 配置文件跟过去（.pet-config.json / .pet-bookmarks.json）
    for cfg in [".pet-config.json", ".pet-bookmarks.json"] {
        let s = old_kb.join(cfg);
        if s.exists() { let _ = fs::rename(&s, nd.join(cfg)); }
    }
    let mut c = state.config.lock().unwrap();
    c.kb_dir_override = new_dir.clone();
    save_config(&nd, &c);
    *state.kb_dir.lock().unwrap() = nd;
    Ok(format!("知识库已迁移到 {}（{}）", new_dir, moved.join("、")))
}

#[tauri::command]
fn stats(state: State<PetState>) -> serde_json::Value {
    let kb = state.kb_dir.lock().unwrap().clone();
    let items = scan_items(&kb);
    let n = |s: &str| items.iter().filter(|i| i.status == s).count();
    serde_json::json!({
        "total": items.len(),
        "seed": n("seed"),
        "fetched": n("fetched"),
        "fetch-failed": n("fetch-failed"),
        "read": n("read"),
        "carded": n("carded"),
        "queue": state.queue.lock().unwrap().len(),
        "notes": fs::read_dir(kb.join("30-notes")).map(|rd| rd.count()).unwrap_or(0),
    })
}

#[tauri::command]
fn bubble(app: AppHandle, msg: String) {
    let _ = Emitter::emit(&app, "pet-bubble", msg);
}

#[tauri::command]
fn open_kb(state: State<PetState>) {
    let kb = state.kb_dir.lock().unwrap().clone();
    #[cfg(target_os = "windows")]
    { let _ = std::process::Command::new("explorer").arg(kb.as_os_str()).spawn(); }
    #[cfg(not(target_os = "windows"))]
    { let _ = std::process::Command::new("xdg-open").arg(kb.as_os_str()).spawn(); }
}

#[tauri::command]
fn read_clipboard_text(app: AppHandle) -> String {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    app.clipboard().read_text().unwrap_or_default()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 知识库目录：默认 = exe 所在目录/knowledge-pet（软件启动位置），支持设置页迁移
    let mut kb = exe_dir().join("knowledge-pet");
    // 目录与 exe 同名（Linux 下 exe 无 .exe 后缀会撞名）时回退用户目录
    if fs::create_dir_all(kb.join("10-inbox")).is_err() {
        kb = dirs_home().join("knowledge-pet");
        fs::create_dir_all(kb.join("10-inbox")).ok();
    }
    fs::create_dir_all(kb.join("20-sources")).ok();
    fs::create_dir_all(kb.join("30-notes")).ok();
    fs::create_dir_all(kb.join("40-sop")).ok();
    fs::create_dir_all(kb.join("50-drafts")).ok();
    fs::create_dir_all(kb.join("90-archive")).ok();
    let cfg = load_config(&kb);
    if !cfg.kb_dir_override.is_empty() {
        let od = PathBuf::from(&cfg.kb_dir_override);
        if od.exists() {
            kb = od;
            fs::create_dir_all(kb.join("10-inbox")).ok();
            fs::create_dir_all(kb.join("30-notes")).ok();
            fs::create_dir_all(kb.join("90-archive")).ok();
        }
    }
    tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(PetState { kb_dir: Mutex::new(kb.clone()), config: Mutex::new(cfg), queue: Mutex::new(load_queue(&kb)) })
        .invoke_handler(tauri::generate_handler![
            list_items, capture, process_queue, capture_text_urls, ai_make_card, mark_read, delete_item, fetch_models,
            get_config, set_config, move_kb_dir, list_bookmarks, add_bookmark, remove_bookmark,
            stats, bubble, open_kb, read_clipboard_text,
            list_cards, quote_create, list_drafts, gen_topics, daily_review,
            integrations::feishu_test, integrations::feishu_push_daily,
            integrations::notion_test, integrations::notion_preview,
            integrations::save_integrations,
            integrations::notion_sync_to_cards, integrations::push_draft_to_notion,
            integrations::feishu_push_weekly,
            wechat_capture::wx_capture, wechat_capture::wx_save_to_inbox,
            wechat_capture::wx_make_cards, wechat_capture::wx_export_gzh,
            watch::watch_list, watch::watch_add, watch::watch_remove,
            watch::watch_set, watch::watch_import_from_capture,
            watch::watch_check, watch::watch_auto_tick
        ])
        .setup(|app| {
            use tauri::menu::{Menu, MenuItem};
            use tauri::tray::TrayIconBuilder;
            // 全局热键：主动抓取（收剪贴板）。热键可配置，默认 Ctrl+Alt+C
            {
                use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
                let cfg = load_config(&app.handle().state::<PetState>().kb_dir.lock().unwrap().clone());
                let hotkey: Shortcut = cfg.hotkey.parse().unwrap_or_else(|_| "ctrl+alt+c".parse().unwrap());
                app.global_shortcut().on_shortcut(hotkey, |app, _sc, ev| {
                    if ev.state == ShortcutState::Pressed {
                        // 触发前端 petClick：往 pet 窗发事件（前端监听后走同一收集逻辑）
                        use tauri::Emitter;
                        let _ = Emitter::emit(app, "hotkey-capture", ());
                    }
                }).ok();
            }
            let show = MenuItem::with_id(app, "show", "打开管理面板", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;
            TrayIconBuilder::with_id("pet-tray")
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_tray_icon_event(|tray, ev| {
                    if matches!(ev, tauri::tray::TrayIconEvent::DoubleClick { .. }) {
                        let app = tray.app_handle();
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.unminimize(); let _ = w.show(); let _ = w.set_focus();
                            use tauri::Emitter; let _ = Emitter::emit_to(app, "main", "switch-view", "panel");
                        }
                    }
                })
                .on_menu_event(|app, ev| match ev.id.as_ref() {
                    "show" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.unminimize(); let _ = w.show(); let _ = w.set_focus();
                            use tauri::Emitter; let _ = Emitter::emit_to(app, "main", "switch-view", "panel");
                        }
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;
            // main 面板点 X / 最小化到关闭 = 回到桌宠（隐藏面板 + 显示宠物），退出走托盘菜单
            let handle = app.handle().clone();
            if let Some(w) = app.get_webview_window("main") {
                w.on_window_event(move |ev| {
                    match ev {
                        // 点 X = 回到桌宠（隐藏面板+亮出宠物），退出走托盘菜单
                        tauri::WindowEvent::CloseRequested { .. } => {
                            if let Some(pet) = handle.get_webview_window("pet") {
                                let _ = pet.show();
                                let _ = pet.set_focus();
                            }
                            let _ = handle.get_webview_window("main").map(|m| m.hide());
                        }
                        _ => {}
                    }
                });
            }
            // pet 窗口：启动恢复上次位置；移动结束（拖完松手）保存位置
            if let Some(pet) = app.get_webview_window("pet") {
                let kb = app.handle().state::<PetState>().kb_dir.lock().unwrap().clone();
                if let Some(pos) = load_win_pos(&kb) {
                    let _ = pet.set_position(tauri::PhysicalPosition::new(pos.x, pos.y));
                }
                let handle2 = app.handle().clone();
                pet.on_window_event(move |ev| {
                    if let tauri::WindowEvent::Moved(pos) = ev {
                        let kb = handle2.state::<PetState>().kb_dir.lock().unwrap().clone();
                        save_win_pos(&kb, &WinPos { x: pos.x, y: pos.y });
                    }
                });
            }
            // 右键菜单「退出」：前端 emit pet-quit → 整个应用退出
            {
                use tauri::Listener;
                let h3 = app.handle().clone();
                app.listen("pet-quit", move |_ev| { h3.exit(0); });
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running knowledge-pet");
}

// 默认知识库 = exe 所在目录/knowledge-pet（软件启动位置）
fn exe_dir() -> PathBuf {
    std::env::current_exe().ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(dirs_home)
}

fn dirs_home() -> PathBuf {
    #[cfg(target_os = "windows")]
    { std::env::var("USERPROFILE").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("C:/Users/Public")) }
    #[cfg(not(target_os = "windows"))]
    { std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("/tmp")) }
}


// ================= 一期二期：引用创作 / 快照 / 跨卡选题 / 每日冒泡 =================

#[derive(Serialize, Clone)]
pub struct CardItem {
    pub file: String,     // 相对路径
    pub title: String,
    pub tags: String,
    pub status: String,
    pub url: String,
    pub created: String,
    pub conclusion: String, // 一句话结论（正文第一个 # 段）
}

/// 扫描 30-notes 全部知识卡
fn scan_cards(kb: &PathBuf) -> Vec<CardItem> {
    let mut out = Vec::new();
    let dir = kb.join("30-notes");
    let Ok(rd) = fs::read_dir(&dir) else { return out };
    let mut paths: Vec<_> = rd.filter_map(|e| e.ok()).map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "md").unwrap_or(false)).collect();
    paths.sort();
    for p in paths.into_iter().rev() {
        let Ok(text) = fs::read_to_string(&p) else { continue };
        let rel = format!("30-notes/{}", p.file_name().unwrap().to_string_lossy());
        // 一句话结论 = "# 一句话结论" 标题后的第一个非空段
        let mut conclusion = String::new();
        if let Some(i) = text.find("# 一句话结论") {
            for line in text[i + "# 一句话结论".len()..].lines() {
                let t = line.trim();
                if t.is_empty() || t.starts_with('#') { if !conclusion.is_empty() || t.starts_with('#') { break; } continue; }
                conclusion.push_str(t); break;
            }
        }
        out.push(CardItem {
            title: fm_field(&text, "title").unwrap_or_else(|| p.file_stem().unwrap().to_string_lossy().to_string()),
            tags: fm_field(&text, "tags").unwrap_or_default(),
            status: fm_field(&text, "status").unwrap_or_default(),
            url: fm_field(&text, "url").unwrap_or_default(),
            created: fm_field(&text, "date").or_else(|| fm_field(&text, "created")).unwrap_or_default(),
            conclusion: conclusion.chars().take(120).collect(),
            file: rel,
        });
    }
    out
}

/// 拉取厂商模型列表；provider_index=None=用文本厂商（或旧字段）。返回 "id|kind"（kind: text|image）。
#[tauri::command]
async fn fetch_models(state: State<'_, PetState>, provider_index: Option<i64>) -> Result<Vec<String>, String> {
    let cfg = state.config.lock().unwrap().clone();
    let idx = provider_index.map(|i| if i < 0 { cfg.active_provider } else { i as usize });
    let (base, key) = match idx.and_then(|i| cfg.providers.get(i)) {
        Some(p) => (p.base_url.clone(), p.api_key.clone()),
        None => { let (b, k, _) = active_cfg(&cfg); (b.clone(), k.clone()) }
    };
    if base.trim().is_empty() { return Err("请先填 API 地址".into()); }
    let cli = reqwest::Client::new();
    let url = format!("{}/models", base.trim_end_matches('/'));
    let mut req = cli.get(&url);
    if !key.trim().is_empty() { req = req.bearer_auth(key.trim()); }
    let resp = req.send().await.map_err(|e| format!("请求失败：{}", e))?;
    let j: serde_json::Value = resp.json().await.map_err(|e| format!("返回异常：{}", e))?;
    let mut out: Vec<String> = j["data"].as_array()
        .map(|a| a.iter().filter_map(|m| m["id"].as_str().map(String::from)).collect())
        .unwrap_or_default();
    out.sort();
    if out.is_empty() { return Err(format!("接口没返回模型列表：{}", serde_json::to_string(&j).unwrap_or_default().chars().take(150).collect::<String>())); }
    // 启发式标记：图片模型 🖼️image / 文本 📝text
    let img_kw = ["image", "vision", "dall", "flux", "sd", "stable", "diffusion", "imagen", "seedream", "cogview", "wanx"];
    Ok(out.into_iter().map(|id| {
        let lower = id.to_lowercase();
        let kind = if img_kw.iter().any(|k| lower.contains(k)) { "image" } else { "text" };
        format!("{}|{}", id, kind)
    }).collect())
}

#[tauri::command]
fn list_cards(state: State<PetState>) -> Vec<CardItem> {
    scan_cards(&state.kb_dir.lock().unwrap().clone())
}

pub(crate) async fn ai_chat(cfg: &AiConfig, prompt: &str) -> Result<String, String> {
    if cfg.api_key.is_empty() || cfg.base_url.is_empty() { return Err("请先在设置里配置 API".into()); }
    let cli = reqwest::Client::builder().timeout(std::time::Duration::from_secs(120)).build().map_err(|e| e.to_string())?;
    let resp = cli.post(format!("{}/chat/completions", cfg.base_url.trim_end_matches('/')))
        .bearer_auth(&cfg.api_key)
        .json(&serde_json::json!({
            "model": cfg.model,
            "messages": [{"role":"user","content":prompt}]
        }))
        .send().await.map_err(|e| format!("AI 请求失败：{}", e))?;
    let j: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    j.pointer("/choices/0/message/content").and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("AI 返回异常：{}", serde_json::to_string(&j).unwrap_or_default().chars().take(200).collect::<String>()))
}

/// 引用创作：勾选知识卡 + 写作要求 → AI 生成草稿 → 存 50-drafts/，快照内嵌
#[tauri::command]
async fn quote_create(state: State<'_, PetState>, files: Vec<String>, goal: String) -> Result<String, String> {
    let cfg = state.config.lock().unwrap().clone();
    let kb = state.kb_dir.lock().unwrap().clone();
    if files.is_empty() { return Err("先勾选至少一张知识卡".into()); }
    // 读取卡片原文（快照）
    let mut snapshot = String::new();
    let mut used = Vec::new();
    for f in &files {
        let text = fs::read_to_string(kb.join(f)).map_err(|e| format!("读卡失败 {f}: {e}"))?;
        used.push(f.clone());
        snapshot.push_str(&format!("\n\n----- 知识卡：{} -----\n{}", f, text));
    }
    let prompt = if cfg.prompt_quote.trim().is_empty() {
        format!(
            "你是内容创作助手。基于下面的知识卡素材完成写作任务。\n铁律：观点和事实尽量来自知识卡原文（可引用原句）；知识卡里没有的数据/事实一律不写，不许编造。\n直接输出正文（Markdown），不要额外解释。\n\n【写作任务】\n{}\n\n【知识卡素材】{}",
            goal, snapshot
        )
    } else {
        cfg.prompt_quote.replace("{goal}", &goal).replace("{snapshot}", &snapshot)
    };
    let content = ai_chat(&cfg, &prompt).await?;
    // 草稿文件：frontmatter 含 used 快照
    let fname = format!("{}-{}.md", chrono_today(), sanitize(&goal.chars().take(24).collect::<String>()));
    let dir = kb.join("50-drafts");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let doc = format!(
        "---\nkind: draft\ncreated: {}\nused: {}\ntask: \"{}\"\n---\n\n{}",
        chrono_today(), used.join(", "), goal.replace('"', "'"), content
    );
    fs::write(dir.join(&fname), doc).map_err(|e| e.to_string())?;
    Ok(format!("草稿已生成：50-drafts/{}", fname))
}

/// 列出草稿（50-drafts）
#[tauri::command]
fn list_drafts(state: State<PetState>) -> Vec<CardItem> {
    let kb = state.kb_dir.lock().unwrap().clone();
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(kb.join("50-drafts")) else { return out };
    let mut paths: Vec<_> = rd.filter_map(|e| e.ok()).map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "md").unwrap_or(false)).collect();
    paths.sort();
    for p in paths.into_iter().rev() {
        let Ok(text) = fs::read_to_string(&p) else { continue };
        out.push(CardItem {
            title: fm_field(&text, "task").unwrap_or_else(|| p.file_stem().unwrap().to_string_lossy().to_string()),
            tags: fm_field(&text, "used").unwrap_or_default(),
            status: "draft".into(),
            url: String::new(),
            created: fm_field(&text, "created").unwrap_or_default(),
            conclusion: String::new(),
            file: format!("50-drafts/{}", p.file_name().unwrap().to_string_lossy()),
        });
    }
    out
}

/// 跨卡选题：AI 扫全部 seed 卡，两两碰撞出选题
#[tauri::command]
async fn gen_topics(state: State<'_, PetState>) -> Result<Vec<String>, String> {
    let cfg = state.config.lock().unwrap().clone();
    let kb = state.kb_dir.lock().unwrap().clone();
    let cards = scan_cards(&kb);
    if cards.len() < 2 { return Err("知识卡不足 2 张，先去收集素材".into()); }
    let digest = cards.iter().map(|c| format!("- 《{}》({})：{}", c.title, c.tags, c.conclusion)).collect::<Vec<_>>().join("\n");
    let prompt = if cfg.prompt_topics.trim().is_empty() {
        format!(
            "下面是我的知识卡片清单（标题+标签+一句话结论）。请把它们两两组合、跨领域碰撞，提炼 5 个值得写的选题。\n每个选题一行，格式：「选题名 — 用哪几张卡（引用标题）— 为什么值得写（一句话）」。只输出 5 行。\n\n{}", digest
        )
    } else {
        cfg.prompt_topics.replace("{digest}", &digest)
    };
    let content = ai_chat(&cfg, &prompt).await?;
    Ok(content.lines().filter(|l| !l.trim().is_empty()).map(|s| s.to_string()).collect())
}

/// 每日冒泡内部逻辑（integrations 模块复用）
#[derive(serde::Serialize)]
pub struct DailyReview { pub title: String, pub conclusion: String, pub todos: Vec<String> }

pub fn daily_review_inner(kb: &std::path::PathBuf) -> Result<DailyReview, String> {
    let cards = scan_cards(kb);
    if cards.is_empty() { return Err("知识库还没有知识卡".into()); }
    // 简单按日期轮换：当天日期 mod 卡数
    let seed: u64 = chrono_today().chars().filter_map(|c| c.to_digit(10)).fold(0u64, |a, d| a * 10 + d as u64);
    let card = &cards[(seed as usize) % cards.len()];
    let text = fs::read_to_string(kb.join(&card.file)).unwrap_or_default();
    // 一句话结论 = "# 一句话结论" 后第一段非空行
    // 抽未勾选的 action
    let todos: Vec<String> = text.lines()
        .filter(|l| l.trim_start().starts_with("- [ ]"))
        .map(|l| l.trim().trim_start_matches("- [ ]").trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    let conclusion = text.splitn(2, "# 一句话结论").nth(1)
        .and_then(|rest| rest.lines().find(|l| !l.trim().is_empty()))
        .unwrap_or(card.title.as_str()).trim().to_string();
    Ok(DailyReview { title: card.title.clone(), conclusion, todos })
}

/// 每日冒泡命令
#[tauri::command]
fn daily_review(state: State<PetState>) -> Result<serde_json::Value, String> {
    let kb = state.kb_dir.lock().unwrap().clone();
    let r = daily_review_inner(&kb)?;
    Ok(serde_json::json!({ "card": { "title": r.title, "conclusion": r.conclusion }, "todos": r.todos }))
}


#[cfg(test)]
mod v095_tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_scan_items_finds_subfolder_index() {
        let dir = std::env::temp_dir().join(format!("pet-v095-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("10-inbox")).unwrap();
        fs::write(dir.join("10-inbox/old.md"), "---\nstatus: seed\nkind: text\n---\n\n# 旧条目\n内容").unwrap();
        fs::create_dir_all(dir.join("10-inbox/2026-11-07-测试文章/images")).unwrap();
        fs::write(dir.join("10-inbox/2026-11-07-测试文章/index.md"),
            "---\nkind: url\nurl: https://x\nstatus: fetched\ncreated: 2026-11-07\n---\n\n# 测试文章标题\n\n正文").unwrap();
        let items = scan_items(&dir);
        assert_eq!(items.len(), 2, "应同时找到顶层 md 与子文件夹 index.md");
        let sub = items.iter().find(|i| i.file.contains("index.md")).expect("子文件夹条目");
        assert_eq!(sub.title, "测试文章标题");
        assert_eq!(sub.status, "fetched");
        assert!(sub.file.starts_with("10-inbox/2026-11-07-测试文章/index.md"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_nickname_extract() {
        let html = r#"<script>var nickname = "前端晚自习"; var biz = "x";</script>"#;
        let p = html.find("nickname").unwrap();
        let seg = &html[p..(p + 120).min(html.len())];
        let after = seg.find(|c| c == '=' || c == ':').unwrap();
        let rest = &seg[after..];
        let q1 = rest.find('"').unwrap(); let rest2 = &rest[q1 + 1..]; let q2 = rest2.find('"').unwrap();
        assert_eq!(rest2[..q2].trim(), "前端晚自习");
        // nickname 出现在无赋值上下文（如 css class）时不应误抠
        let html2 = r#"<div class="nickname">正文</div>"#;
        let seg2 = &html2[html2.find("nickname").unwrap()..];
        let no_assign = seg2.find(|c| c == '=' || c == ':').is_none();
        assert!(no_assign);
    }

    #[test]
    fn test_url_extract_from_pasted_text() {
        // 与 capture_text_urls 相同的分隔逻辑（复制保持一致）
        let text = "看看这两篇 https://mp.weixin.qq.com/s/abc123 和 http://example.com/a?x=1)还有这个<br>https://mp.weixin.qq.com/s/abc123 重复";
        let mut urls = Vec::new();
        for word in text.split(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ')' || c == '>') {
            let w = word.trim_start_matches('(').trim_start_matches('<');
            let url = if w.starts_with("http://") || w.starts_with("https://") {
                w.split(|c| c == '<' || c == '>').next().unwrap_or(w).to_string()
            } else { continue };
            let url = url.trim_end_matches(&[')', '>', ',', '，', '。', '；', ';'][..]).to_string();
            if url.len() < 12 || urls.contains(&url) { continue; }
            urls.push(url);
        }
        assert_eq!(urls.len(), 2, "应抠出 2 个去重后的 URL");
        assert_eq!(urls[0], "https://mp.weixin.qq.com/s/abc123");
        assert_eq!(urls[1], "http://example.com/a?x=1");
    }
}

#[cfg(test)]
mod workshop_tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_scan_cards_extracts_conclusion() {
        let dir = std::env::temp_dir().join(format!("pet-test-{}", std::process::id()));
        let notes = dir.join("30-notes");
        fs::create_dir_all(&notes).unwrap();
        fs::write(notes.join("20261104-判断力.md"),
            "---\ntitle: 判断力比提示词重要\ntags: [自媒体, AI]\ndate: 2026-11-04\nstatus: seed\n---\n\n# 一句话结论\n\n很多人用 AI 的问题是不知道怎么判断。\n\n## 展开\n\n- 要点一\n\n## 下一步行动\n\n- [ ] 写成公众号文章\n- [x] 已完成的\n").unwrap();
        let kb = dir.clone();
        let cards = scan_cards(&kb);
        assert_eq!(cards.len(), 1);
        let c = &cards[0];
        assert_eq!(c.title, "判断力比提示词重要");
        assert_eq!(c.tags, "[自媒体, AI]");
        assert!(c.conclusion.contains("不知道怎么判断"), "conclusion={}", c.conclusion);
        assert!(!c.conclusion.contains("要点一"), "结论不应混入展开段");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_quote_create_snapshot_flow() {
        let dir = std::env::temp_dir().join(format!("pet-test-q-{}", std::process::id()));
        let notes = dir.join("30-notes");
        fs::create_dir_all(&notes).unwrap();
        fs::write(notes.join("card-a.md"), "---\ntitle: 卡A\n---\n\n# 一句话结论\n\n内容A").unwrap();
        // 模拟 quote_create 的快照与落盘逻辑（不走 AI）
        let kb = dir.clone();
        let files = vec!["30-notes/card-a.md".to_string()];
        let mut used = Vec::new();
        let mut snapshot = String::new();
        for f in &files {
            let text = fs::read_to_string(kb.join(f)).unwrap();
            used.push(f.clone());
            snapshot.push_str(&format!("\n\n----- 知识卡：{} -----\n{}", f, text));
        }
        assert!(snapshot.contains("内容A"));
        let goal = "写一篇开头";
        let fname = format!("{}-{}.md", chrono_today(), sanitize(&goal.chars().take(24).collect::<String>()));
        let doc = format!("---\nkind: draft\ncreated: {}\nused: {}\ntask: \"{}\"\n---\n\n正文", chrono_today(), used.join(", "), goal.replace('"', "'"));
        fs::create_dir_all(kb.join("50-drafts")).unwrap();
        fs::write(kb.join("50-drafts").join(&fname), &doc).unwrap();
        // 验证 list_drafts 能读到
        let drafts = {
            // 复用 list_drafts 内部逻辑
            let mut out = Vec::new();
            let rd = fs::read_dir(kb.join("50-drafts")).unwrap();
            for e in rd.filter_map(|e| e.ok()) {
                let p = e.path();
                if p.extension().map(|x| x == "md").unwrap_or(false) {
                    let t = fs::read_to_string(&p).unwrap();
                    out.push(fm_field(&t, "used").unwrap_or_default());
                }
            }
            out
        };
        assert_eq!(drafts, vec!["30-notes/card-a.md".to_string()]);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_daily_review_rotation() {
        let dir = std::env::temp_dir().join(format!("pet-test-r-{}", std::process::id()));
        let notes = dir.join("30-notes");
        fs::create_dir_all(&notes).unwrap();
        for name in ["a", "b", "c"] {
            fs::write(notes.join(format!("20261104-{}.md", name)),
                format!("---\ntitle: 卡{}\n---\n\n# 一句话结论\n\n内容{}\n\n- [ ] 待办{}", name.to_uppercase(), name, name)).unwrap();
        }
        let kb = dir.clone();
        let cards = scan_cards(&kb);
        assert_eq!(cards.len(), 3);
        let seed: u64 = chrono_today().chars().filter_map(|c| c.to_digit(10)).fold(0u64, |a, d| a * 10 + d as u64);
        let card = &cards[(seed as usize) % cards.len()];
        let text = fs::read_to_string(kb.join(&card.file)).unwrap();
        let todos: Vec<String> = text.lines()
            .filter(|l| l.trim_start().starts_with("- [ ]"))
            .map(|l| l.trim().trim_start_matches("- [ ]").trim().to_string())
            .collect();
        assert_eq!(todos.len(), 1);
        assert!(todos[0].starts_with("待办"), "todos={:?} card={}", todos, card.title);
        fs::remove_dir_all(&dir).ok();
    }
}/// 从粘贴文本中抠出所有 URL，去重后批量入队（借鉴数海丹心下载工具）
#[tauri::command]
fn capture_text_urls(state: State<PetState>, text: String) -> Result<String, String> {
    let kb = state.kb_dir.lock().unwrap().clone();
    let mut added = 0; let mut skipped = 0;
    let existing: Vec<String> = scan_items(&kb).iter().map(|i| i.url.clone()).collect();
    let mut seen: Vec<String> = Vec::new();
    for word in text.split(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ')' || c == '>') {
        let w = word.trim_start_matches('(').trim_start_matches('<');
        let url = if w.starts_with("http://") || w.starts_with("https://") {
            w.split(|c| c == '<' || c == '>').next().unwrap_or(w).to_string()
        } else { continue };
        let url = url.trim_end_matches(&[')', '>', ',', '，', '。', '；', ';'][..]).to_string();
        if url.len() < 12 || seen.contains(&url) { continue; }
        seen.push(url.clone());
        if existing.contains(&url) { skipped += 1; continue; }
        let id = now_id();
        let fname = format!("{}-网页剪藏.md", id);
        let body = format!("---\nkind: url\nurl: {}\nstatus: seed\ncreated: {}\n---\n\n# 网页剪藏\n\n{}", url, chrono_today(), url);
        let full = kb.join("10-inbox").join(&fname);
        fs::create_dir_all(full.parent().unwrap()).map_err(|e| e.to_string())?;
        fs::write(&full, body).map_err(|e| e.to_string())?;
        state.queue.lock().unwrap().push(id.clone());
        added += 1;
    }
    if added == 0 && skipped == 0 { return Err("文本里没找到链接".into()); }
    Ok(format!("已加入 {} 条链接{}（重复跳过 {}）", added, if skipped > 0 { "，已收过的不再收" } else { "" }, skipped))
}


