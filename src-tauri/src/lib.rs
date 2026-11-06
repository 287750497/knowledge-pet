// Knowledge Pet 知识收藏桌宠
// 核心：托盘+桌宠、单击收剪贴板→inbox→冒泡、URL抓取队列、AI整理、设置(api/model)
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, State};

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
}

fn default_hotkey() -> String { "Ctrl+Alt+C".into() }

impl Default for AiConfig {
    fn default() -> Self {
        AiConfig { base_url: String::new(), api_key: String::new(), model: String::new(), hotkey: default_hotkey(), kb_dir_override: String::new() }
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
    fs::read_to_string(kb.join(".pet-config.json")).ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_config(kb: &PathBuf, c: &AiConfig) {
    let _ = fs::write(kb.join(".pet-config.json"), serde_json::to_string_pretty(c).unwrap());
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

fn sanitize(s: &str) -> String {
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
        let mut files: Vec<_> = rd.filter_map(|e| e.ok()).map(|e| e.path())
            .filter(|p| p.extension().map_or(false, |x| x == "md")).collect();
        files.sort();
        for p in files {
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            let text = fs::read_to_string(&p).unwrap_or_default();
            let status = fm_field(&text, "status").unwrap_or_else(|| "seed".into());
            let kind = fm_field(&text, "kind").unwrap_or_else(|| "text".into());
            let url = fm_field(&text, "url").unwrap_or_default();
            let title = text.lines()
                .find(|l| l.starts_with("# ") && !l.starts_with("#!"))
                .map(|l| l[2..].trim().to_string())
                .unwrap_or_else(|| name.trim_end_matches(".md").to_string());
            out.push(Item {
                id: name.trim_end_matches(".md").to_string(),
                file: format!("10-inbox/{}", name),
                title, kind, url, status,
                created_at: name.chars().take(12).collect(),
                error: String::new(),
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
    if is_url { state.queue.lock().unwrap().push(id.clone()); }
    Ok(Item {
        id: fname.trim_end_matches(".md").to_string(), // 与 scan_items 一致（文件名 id）
        file: format!("10-inbox/{}", fname),
        title: if is_url { "网页剪藏".into() } else { first },
        kind: kind.into(),
        url: if is_url { text } else { String::new() },
        status: "seed".into(),
        created_at: chrono_today(),
        error: String::new(),
    })
}

// 抓取队列：取一条 URL → 拉 → 提正文 → 写回 → status=fetched
#[tauri::command]
async fn process_queue(state: State<'_, PetState>) -> Result<String, String> {
    let id = state.queue.lock().unwrap().pop().ok_or("队列空")?;
    eprintln!("[pet] process_queue pop id={}", id);
    let kb = state.kb_dir.lock().unwrap().clone();
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
    let new_text = format!(
        "---\nkind: url\nurl: {}\nstatus: fetched\ncreated: {}\n---\n\n# {}\n\n> 来源：{}\n\n{}",
        url, chrono_today(), title, url, content
    );
    fs::write(&path, &new_text).map_err(|e| { eprintln!("[pet] write err: {}", e); e.to_string() })?;
    eprintln!("[pet] write ok {} bytes", new_text.len());
    Ok(format!("已抓取：{}", title))
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
    let prompt = format!(
        "你是知识库管理员。把下面的素材整理成一张知识卡。\n输出 frontmatter + 正文，格式严格如下（YAML）:\n---\ntitle: 卡片标题（10字内）\ntags: [标签1, 标签2]\nstatus: seed\nsource: \"{}\"\n---\n\n# 一句话结论\n\n（最核心的判断一句话）\n\n## 展开\n\n（要点、例子，markdown 列表，300字内）\n\n## 下一步行动\n\n- [ ] 一个可执行的下一步\n\n素材：\n{}",
        it.url, text
    );
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
fn set_config(state: State<PetState>, base_url: String, api_key: String, model: String, hotkey: Option<String>) {
    let mut c = state.config.lock().unwrap();
    c.base_url = base_url; c.api_key = api_key; c.model = model;
    if let Some(h) = hotkey { if !h.trim().is_empty() { c.hotkey = h.trim().to_string(); } }
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
    fs::create_dir_all(kb.join("90-archive")).ok();
    let mut cfg = load_config(&kb);
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
        .manage(PetState { kb_dir: Mutex::new(kb), config: Mutex::new(cfg), queue: Mutex::new(Vec::new()) })
        .invoke_handler(tauri::generate_handler![
            list_items, capture, process_queue, ai_make_card, mark_read, delete_item,
            get_config, set_config, move_kb_dir, list_bookmarks, add_bookmark, remove_bookmark,
            stats, bubble, open_kb, read_clipboard_text
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
            // main / pet 窗口点关闭 = 退出进程
            let handle = app.handle().clone();
            if let Some(w) = app.get_webview_window("main") {
                w.on_window_event(move |ev| {
                    if let tauri::WindowEvent::CloseRequested { .. } = ev {
                        handle.exit(0);
                    }
                });
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
