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

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct AiConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

pub struct PetState {
    pub kb_dir: PathBuf,
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
    scan_items(&state.kb_dir)
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
    let is_url = text.starts_with("http://") || text.starts_with("https://");
    for it in scan_items(&state.kb_dir) {
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
    let full = state.kb_dir.join("10-inbox").join(&fname);
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
    let items = scan_items(&state.kb_dir);
    eprintln!("[pet] items: {:?}", items.iter().map(|i| i.id.clone()).collect::<Vec<_>>());
    let it = items.iter().find(|i| i.id == id || i.id.starts_with(&id)).ok_or(format!("条目不存在：{}", id))?;
    let url = it.url.clone();
    let path = state.kb_dir.join(&it.file);
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
    let items = scan_items(&state.kb_dir);
    let it = items.iter().find(|i| i.id == id).ok_or("条目不存在")?;
    let text = fs::read_to_string(state.kb_dir.join(&it.file)).map_err(|e| e.to_string())?;
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
    let cards_dir = state.kb_dir.join("30-notes");
    fs::create_dir_all(&cards_dir).map_err(|e| e.to_string())?;
    // AI 有时会包 ```yaml 围栏 / 前置闲话——定位真正的 frontmatter 起点
    let body = match content.find("\n---\n").or_else(|| content.starts_with("---").then(|| 0)) {
        Some(p) => &content[p.max(0)..],
        None => &content[..],
    };
    let title = body.lines().find(|l| l.starts_with("title:")).map(|l| l[6..].trim().trim_matches('"').to_string()).unwrap_or_else(|| it.title.clone());
    let fname = format!("{}-{}.md", chrono_today(), sanitize(&title));
    fs::write(cards_dir.join(&fname), body.trim_start()).map_err(|e| e.to_string())?;
    set_fm_field(&state.kb_dir.join(&it.file), "status", "carded");
    Ok(format!("知识卡已生成：{}", fname))
}

#[tauri::command]
fn mark_read(state: State<PetState>, id: String) -> Result<(), String> {
    let items = scan_items(&state.kb_dir);
    let it = items.iter().find(|i| i.id == id).ok_or("条目不存在")?;
    set_fm_field(&state.kb_dir.join(&it.file), "status", "read");
    Ok(())
}

#[tauri::command]
fn get_config(state: State<PetState>) -> AiConfig { state.config.lock().unwrap().clone() }

#[tauri::command]
fn set_config(state: State<PetState>, base_url: String, api_key: String, model: String) {
    let mut c = state.config.lock().unwrap();
    c.base_url = base_url; c.api_key = api_key; c.model = model;
    save_config(&state.kb_dir, &c);
}

#[tauri::command]
fn stats(state: State<PetState>) -> serde_json::Value {
    let items = scan_items(&state.kb_dir);
    let n = |s: &str| items.iter().filter(|i| i.status == s).count();
    serde_json::json!({
        "total": items.len(),
        "seed": n("seed"),
        "fetched": n("fetched"),
        "fetch-failed": n("fetch-failed"),
        "read": n("read"),
        "carded": n("carded"),
        "queue": state.queue.lock().unwrap().len(),
        "notes": fs::read_dir(state.kb_dir.join("30-notes")).map(|rd| rd.count()).unwrap_or(0),
    })
}

#[tauri::command]
fn bubble(app: AppHandle, msg: String) {
    let _ = Emitter::emit(&app, "pet-bubble", msg);
}

#[tauri::command]
fn open_kb(state: State<PetState>) {
    #[cfg(target_os = "windows")]
    { let _ = std::process::Command::new("explorer").arg(state.kb_dir.as_os_str()).spawn(); }
    #[cfg(not(target_os = "windows"))]
    { let _ = std::process::Command::new("xdg-open").arg(state.kb_dir.as_os_str()).spawn(); }
}

#[tauri::command]
fn read_clipboard_text(app: AppHandle) -> String {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    app.clipboard().read_text().unwrap_or_default()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let kb = dirs_home().join("knowledge-base");
    fs::create_dir_all(kb.join("10-inbox")).ok();
    fs::create_dir_all(kb.join("30-notes")).ok();
    let cfg = load_config(&kb);
    tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(PetState { kb_dir: kb, config: Mutex::new(cfg), queue: Mutex::new(Vec::new()) })
        .invoke_handler(tauri::generate_handler![
            list_items, capture, process_queue, ai_make_card, mark_read,
            get_config, set_config, stats, bubble, open_kb, read_clipboard_text
        ])
        .setup(|app| {
            use tauri::menu::{Menu, MenuItem};
            use tauri::tray::TrayIconBuilder;
            let show = MenuItem::with_id(app, "show", "打开管理面板", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;
            TrayIconBuilder::with_id("pet-tray")
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, ev| match ev.id.as_ref() {
                    "show" => { if let Some(w) = app.get_webview_window("main") { let _ = w.show(); let _ = w.set_focus(); } }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;
            // main 窗口点关闭 = 隐藏（托盘常驻），不退出
            let handle = app.handle().clone();
            app.get_webview_window("main").unwrap().on_window_event(move |ev| {
                if let tauri::WindowEvent::CloseRequested { api, .. } = ev {
                    api.prevent_close();
                    if let Some(w) = handle.get_webview_window("main") { let _ = w.hide(); }
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running knowledge-pet");
}

fn dirs_home() -> PathBuf {
    #[cfg(target_os = "windows")]
    { std::env::var("USERPROFILE").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("C:/Users/Public")) }
    #[cfg(not(target_os = "windows"))]
    { std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("/tmp")) }
}
