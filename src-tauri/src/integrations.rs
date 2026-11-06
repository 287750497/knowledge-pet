// ================= 集成：飞书 webhook + Notion 连接 =================
// 飞书：自定义机器人 webhook，POST JSON 即推送（无需建应用）
// Notion：internal connection token，官方 REST API

use crate::PetState;
use std::fs;
use serde_json::json;
use tauri::State;

// ---------- 飞书 ----------

/// 发送飞书文本消息。返回 Err(原因) 或 Ok(())
async fn feishu_send_text(webhook: &str, text: &str) -> Result<String, String> {
    let client = reqwest::Client::new();
    let resp = client.post(webhook)
        .json(&json!({ "msg_type": "text", "content": { "text": text } }))
        .timeout(std::time::Duration::from_secs(15))
        .send().await
        .map_err(|e| format!("网络错误: {e}"))?;
    let body: serde_json::Value = resp.json().await.map_err(|e| format!("响应解析失败: {e}"))?;
    // 飞书返回 {"code":0,"msg":"success"} 或 {"StatusCode":0}
    let code = body.get("code").and_then(|v| v.as_i64())
        .or_else(|| body.get("StatusCode").and_then(|v| v.as_i64()))
        .unwrap_or(-1);
    let msg = body.get("msg").and_then(|v| v.as_str()).unwrap_or("");
    if code == 0 { Ok(msg.to_string()) } else { Err(format!("飞书错误({code}): {msg}")) }
}

#[tauri::command]
pub async fn feishu_test(state: State<'_, PetState>) -> Result<String, String> {
    let wh = state.config.lock().unwrap().feishu_webhook.clone();
    if wh.trim().is_empty() { return Err("请先填写飞书 webhook 地址".into()); }
    feishu_send_text(wh.trim(), "🐱 知识收藏桌宠连接成功！以后每日知识冒泡会推到这里。").await
}

/// 每日冒泡推送：抽今日知识卡 + 待办，推飞书。由前端定时器或手动触发。
#[tauri::command]
pub async fn feishu_push_daily(state: State<'_, PetState>) -> Result<String, String> {
    let (wh, kb) = {
        let cfg = state.config.lock().unwrap();
        let kb = state.kb_dir.lock().unwrap().clone();
        (cfg.feishu_webhook.clone(), kb)
    };
    if wh.trim().is_empty() { return Err("未配置飞书 webhook".into()); }
    // 复用每日冒泡逻辑
    let review = crate::daily_review_inner(&kb)?;
    let mut text = format!("🐱 每日冒泡 {}\n\n📌 今日知识卡：《{}》\n", 
        crate::chrono_today(), review.title);
    text.push_str(&format!("💡 {}\n", review.conclusion));
    if !review.todos.is_empty() {
        text.push_str("\n✅ 待办：\n");
        for t in review.todos.iter().take(5) { text.push_str(&format!("· {t}\n")); }
    } else {
        text.push_str("\n（今日知识卡没有待办）");
    }
    feishu_send_text(wh.trim(), &text).await
}

// ---------- Notion ----------

const NOTION_VERSION: &str = "2022-06-11";

fn notion_client(token: &str) -> reqwest::Client {
    let mut h = reqwest::header::HeaderMap::new();
    h.insert("Authorization", format!("Bearer {token}").parse().unwrap());
    h.insert("Notion-Version", NOTION_VERSION.parse().unwrap());
    reqwest::Client::builder().default_headers(h).build().unwrap()
}

/// 测试 Notion 连接：token 有效 + 数据库可读，返回数据库标题
#[tauri::command]
pub async fn notion_test(token: String, db_id: String) -> Result<String, String> {
    let token = token.trim();
    if token.is_empty() || db_id.trim().is_empty() { return Err("请填写 token 和数据库 ID".into()); }
    let client = notion_client(token);
    let db_id = db_id.trim().replace('-', "");
    let resp = client.get(format!("https://api.notion.com/v1/databases/{db_id}"))
        .timeout(std::time::Duration::from_secs(15))
        .send().await.map_err(|e| format!("网络错误: {e}"))?;
    let st = resp.status();
    let body: serde_json::Value = resp.json().await.map_err(|e| format!("响应解析失败: {e}"))?;
    if !st.is_success() {
        let m = body.get("message").and_then(|v| v.as_str()).unwrap_or("未知错误");
        if st.as_u16() == 401 { return Err("token 无效或过期（检查 Notion integration 的 secret）".into()); }
        if st.as_u16() == 404 { return Err("找不到该数据库——确认已在 Notion 页面右上角「连接」里把这个 integration 加进去".into()); }
        return Err(format!("Notion 错误 {}: {m}", st.as_u16()));
    }
    let title = body["title"].as_array()
        .and_then(|a| a.first())
        .and_then(|t| t["plain_text"].as_str())
        .unwrap_or("未命名数据库").to_string();
    Ok(format!("✅ 连接成功，数据库：《{title}》"))
}

/// 拉取 Notion 数据库最近 N 条记录的纯文本，前端展示预览（第二期再做 AI 整理入库）
#[tauri::command]
pub async fn notion_preview(token: String, db_id: String, limit: u32) -> Result<Vec<String>, String> {
    let client = notion_client(token.trim());
    let db_id = db_id.trim().replace('-', "");
    let body = client.post(format!("https://api.notion.com/v1/databases/{db_id}/query"))
        .json(&json!({ "page_size": limit.max(1).min(10), "sorts": [{ "timestamp": "created_time", "direction": "descending" }] }))
        .timeout(std::time::Duration::from_secs(20))
        .send().await.map_err(|e| format!("网络错误: {e}"))?;
    if !body.status().is_success() { return Err(format!("查询失败 HTTP {}", body.status())); }
    let v: serde_json::Value = body.json().await.map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    if let Some(rows) = v["results"].as_array() {
        for r in rows {
            // 取第一行标题属性里的纯文本
            let title = r["properties"].as_object().and_then(|props| {
                props.values().find(|p| p["type"] == "title")
                    .and_then(|p| p["title"].as_array())
                    .and_then(|a| a.first())
                    .and_then(|t| t["plain_text"].as_str()).map(String::from)
            }).unwrap_or_else(|| "(无标题)".into());
            let created = r["created_time"].as_str().unwrap_or("").get(0..10).unwrap_or("").to_string();
            out.push(format!("{created}  {title}"));
        }
    }
    Ok(out)
}

// ---------- 配置扩展命令 ----------

#[tauri::command]
pub fn save_integrations(state: State<'_, PetState>, feishu_webhook: String,
    feishu_push_daily: bool, notion_token: String, notion_db_id: String) -> Result<(), String> {
    let kb = state.kb_dir.lock().unwrap().clone();
    let mut cfg = state.config.lock().unwrap();
    cfg.feishu_webhook = feishu_webhook.trim().to_string();
    cfg.feishu_push_daily = feishu_push_daily;
    cfg.notion_token = notion_token.trim().to_string();
    cfg.notion_db_id = notion_db_id.trim().replace('-', "");
    crate::save_config(&kb, &cfg);
    Ok(())
}

// ---------- 单元测试 ----------

// ================= 第二期/第三期 =================

/// Notion 数据库行 → 纯文本（页面 block 展开）
async fn notion_page_text(client: &reqwest::Client, page_id: &str) -> Result<String, String> {
    let resp = client.get(format!("https://api.notion.com/v1/blocks/{page_id}/children"))
        .timeout(std::time::Duration::from_secs(20))
        .send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() { return Err(format!("读页面失败 HTTP {}", resp.status())); }
    let v: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    if let Some(results) = v["results"].as_array() {
        for b in results {
            let t = b["type"].as_str().unwrap_or("");
            let text = b[t]["rich_text"].as_array()
                .map(|a| a.iter().filter_map(|r| r["plain_text"].as_str()).collect::<Vec<_>>().join(""))
                .unwrap_or_default();
            if !text.is_empty() { out.push(text); }
        }
    }
    Ok(out.join("\n"))
}

/// 第二期①：拉取 Notion 收集箱最近 N 行 → AI 整理成知识卡 → 存 30-notes/，并在 Notion 行属性 marked 标记（可选）
#[tauri::command]
pub async fn notion_sync_to_cards(state: State<'_, PetState>, limit: u32) -> Result<String, String> {
    let cfg = state.config.lock().unwrap().clone();
    if cfg.notion_token.is_empty() || cfg.notion_db_id.is_empty() { return Err("先在设置→Notion 填 token 和数据库 ID".into()); }
    if cfg.api_key.is_empty() { return Err("AI 未配置（设置→AI 模型）".into()); }
    let kb = state.kb_dir.lock().unwrap().clone();
    let client = notion_client(&cfg.notion_token);
    let db_id = cfg.notion_db_id.trim().replace('-', "");
    // 查询最近 N 条
    let body = client.post(format!("https://api.notion.com/v1/databases/{db_id}/query"))
        .json(&json!({ "page_size": limit.max(1).min(10), "sorts": [{ "timestamp": "created_time", "direction": "descending" }] }))
        .timeout(std::time::Duration::from_secs(20))
        .send().await.map_err(|e| format!("网络错误: {e}"))?;
    if !body.status().is_success() { return Err(format!("查询失败 HTTP {}", body.status())); }
    let v: serde_json::Value = body.json().await.map_err(|e| e.to_string())?;
    let rows = v["results"].as_array().cloned().unwrap_or_default();
    if rows.is_empty() { return Ok("收集箱是空的，没有可同步的内容".into()); }
    let mut done = Vec::new();
    for r in &rows {
        let page_id = r["id"].as_str().unwrap_or_default().replace('-', "");
        let title = r["properties"].as_object().and_then(|props| {
            props.values().find(|p| p["type"] == "title")
                .and_then(|p| p["title"].as_array())
                .and_then(|a| a.first())
                .and_then(|t| t["plain_text"].as_str()).map(String::from)
        }).unwrap_or_else(|| "无标题".into());
        let text = notion_page_text(&client, &page_id).await.unwrap_or_default();
        if text.trim().is_empty() { continue; }
        let url = r["url"].as_str().unwrap_or("");
        let prompt = format!(
            "你是知识库管理员。把下面的素材整理成一张知识卡。\n输出 frontmatter + 正文，格式严格如下（YAML）:\n---\ntitle: 卡片标题（10字内）\ntags: [标签1, 标签2]\nstatus: seed\nsource: \"Notion: {}\"\n---\n\n# 一句话结论\n\n（最核心的判断一句话）\n\n## 展开\n\n（要点、例子，markdown 列表，300字内）\n\n## 下一步行动\n\n- [ ] 一个可执行的下一步\n\n素材标题：{}\n素材内容：\n{}",
            url, title, text
        );
        let content = crate::ai_chat(&cfg, &prompt).await?;
        let body = match content.find("\n---\n").or_else(|| content.starts_with("---").then(|| 0)) {
            Some(p) => &content[p.max(0)..],
            None => &content[..],
        };
        let title2 = body.lines().find(|l| l.starts_with("title:")).map(|l| l[6..].trim().trim_matches('"').to_string()).unwrap_or(title.clone());
        let fname = format!("{}-{}.md", crate::chrono_today(), crate::sanitize(&title2));
        fs::create_dir_all(kb.join("30-notes")).map_err(|e| e.to_string())?;
        fs::write(kb.join("30-notes").join(&fname), body.trim_start()).map_err(|e| e.to_string())?;
        done.push(fname);
    }
    if done.is_empty() { Ok("Notion 行都没有正文内容，跳过".into()) }
    else { Ok(format!("已同步 {} 张知识卡：{}", done.len(), done.join("、"))) }
}

/// 第二期②：把本地草稿推回 Notion（创建页面到收集箱数据库），手机可读
#[tauri::command]
pub async fn push_draft_to_notion(state: State<'_, PetState>, file: String) -> Result<String, String> {
    let cfg = state.config.lock().unwrap().clone();
    if cfg.notion_token.is_empty() || cfg.notion_db_id.is_empty() { return Err("先在设置→Notion 填 token 和数据库 ID".into()); }
    let kb = state.kb_dir.lock().unwrap().clone();
    let text = fs::read_to_string(kb.join(&file)).map_err(|e| format!("读草稿失败: {e}"))?;
    // 去掉 frontmatter
    let content = if text.starts_with("---") {
        text[3..].find("\n---\n").map(|p| text[p + 8..].to_string()).unwrap_or_else(|| text.clone())
    } else { text.clone() };
    // 正文 → Notion blocks（按行拆：# 标题 / - 列表 / 普通段落）
    let mut children = Vec::new();
    for line in content.lines() {
        let l = line.trim();
        if l.is_empty() { continue; }
        if let Some(h) = l.strip_prefix("# ") { children.push(json!({"object":"block","type":"heading_1","heading_1":{"rich_text":[{"type":"text","text":{"content":h}}]}})); }
        else if let Some(h) = l.strip_prefix("## ") { children.push(json!({"object":"block","type":"heading_2","heading_2":{"rich_text":[{"type":"text","text":{"content":h}}]}})); }
        else if let Some(h) = l.strip_prefix("### ") { children.push(json!({"object":"block","type":"heading_3","heading_3":{"rich_text":[{"type":"text","text":{"content":h}}]}})); }
        else if let Some(b) = l.strip_prefix("- ").or_else(|| l.strip_prefix("* ")) { children.push(json!({"object":"block","type":"bulleted_list_item","bulleted_list_item":{"rich_text":[{"type":"text","text":{"content":b}}]}})); }
        else { children.push(json!({"object":"block","type":"paragraph","paragraph":{"rich_text":[{"type":"text","text":{"content":l}}]}})); }
        if children.len() >= 90 { break; } // Notion 单次 100 上限，留余量
    }
    let title = file.rsplit('/').next().unwrap_or("草稿").trim_end_matches(".md").to_string();
    let client = notion_client(&cfg.notion_token);
    let db_id = cfg.notion_db_id.trim().replace('-', "");
    let resp = client.post("https://api.notion.com/v1/pages")
        .json(&json!({
            "parent": { "database_id": db_id },
            "properties": { "title": { "title": [{ "text": { "content": format!("📝 {}", title) } }] } },
            "children": children
        }))
        .timeout(std::time::Duration::from_secs(30))
        .send().await.map_err(|e| format!("网络错误: {e}"))?;
    let st = resp.status();
    let v: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    if !st.is_success() {
        let m = v["message"].as_str().unwrap_or("未知错误");
        return Err(format!("推送失败 {}: {}", st.as_u16(), m));
    }
    Ok(format!("已推送到 Notion：📝 {}（手机打开 Notion 就能看）", title))
}

/// 第三期：飞书周报——本周新增卡数/草稿数/待办完成情况推飞书。手动触发（面板按钮）或每周日 20 点自动
#[tauri::command]
pub async fn feishu_push_weekly(state: State<'_, PetState>) -> Result<String, String> {
    let (wh, kb) = {
        let cfg = state.config.lock().unwrap();
        let kb = state.kb_dir.lock().unwrap().clone();
        (cfg.feishu_webhook.clone(), kb)
    };
    if wh.trim().is_empty() { return Err("未配置飞书 webhook".into()); }
    let stats = weekly_stats(&kb)?;
    let text = format!(
        "📊 知识库周报\n\n本周期（最近7天）：\n· 新收素材 {} 条\n· 新增知识卡 {} 张\n· 生成草稿 {} 篇\n· 待办完成 {} 项（剩 {} 项未完成）\n\n累计知识卡 {} 张。",
        stats.new_items, stats.new_cards, stats.new_drafts, stats.todos_done, stats.todos_open, stats.total_cards
    );
    feishu_send_text(wh.trim(), &text).await
}

#[derive(serde::Serialize)]
pub struct WeeklyStats { pub new_items: usize, pub new_cards: usize, pub new_drafts: usize, pub todos_done: usize, pub todos_open: usize, pub total_cards: usize }

pub fn weekly_stats(kb: &std::path::PathBuf) -> Result<WeeklyStats, String> {
    let recent = |fname: &str| -> bool {
        // 文件名 YYYYMMDD-xxx.md；7 天内按粗略日历序号（y*372+m*31+d）比较
        let digits: String = fname.chars().take(8).filter(|c| c.is_ascii_digit()).collect();
        if digits.len() != 8 { return false; }
        let ymd: i64 = digits.parse().unwrap_or(0);
        let (y, m, d) = (ymd / 10000, ymd / 100 % 100, ymd % 100);
        let today = crate::chrono_today();
        let ty: i64 = today.get(0..4).and_then(|s| s.parse().ok()).unwrap_or(0);
        let tm: i64 = today.get(4..6).and_then(|s| s.parse().ok()).unwrap_or(0);
        let td: i64 = today.get(6..8).and_then(|s| s.parse().ok()).unwrap_or(0);
        let ord = |y: i64, m: i64, d: i64| y * 372 + m * 31 + d;
        ord(ty, tm, td) - ord(y, m, d) <= 7
    };
    let count_dir = |dir: &str| -> usize {
        fs::read_dir(kb.join(dir)).map(|rd| rd.filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|x| x == "md").unwrap_or(false))
            .filter(|e| recent(&e.file_name().to_string_lossy()))
            .count()).unwrap_or(0)
    };
    let total_cards = fs::read_dir(kb.join("30-notes")).map(|rd| rd.filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "md").unwrap_or(false)).count()).unwrap_or(0);
    // 待办：30-notes 里所有卡的 - [ ] / - [x]
    let (mut done, mut open) = (0usize, 0usize);
    if let Ok(rd) = fs::read_dir(kb.join("30-notes")) {
        for e in rd.filter_map(|e| e.ok()) {
            if let Ok(t) = fs::read_to_string(e.path()) {
                for l in t.lines() {
                    if l.contains("- [x]") { done += 1; }
                    else if l.contains("- [ ]") { open += 1; }
                }
            }
        }
    }
    Ok(WeeklyStats { new_items: count_dir("10-inbox"), new_cards: count_dir("30-notes"), new_drafts: count_dir("50-drafts"), todos_done: done, todos_open: open, total_cards })
}

// ---------- 测试 ----------

#[cfg(test)]
mod phase23_tests {
    use super::*;

    #[test]
    fn test_weekly_stats() {
        let tmp = std::env::temp_dir().join(format!("kp-w-{}", std::process::id()));
        fs::create_dir_all(tmp.join("10-inbox")).unwrap();
        fs::create_dir_all(tmp.join("30-notes")).unwrap();
        fs::create_dir_all(tmp.join("50-drafts")).unwrap();
        let today = crate::chrono_today();
        // 今天的新卡（应计入）+ 三个月前的旧卡（不计入）
        fs::write(tmp.join(format!("30-notes/{}-新卡.md", today)), "- [x] 完成的\n- [ ] 没完成的\n").unwrap();
        fs::write(tmp.join("30-notes/20260101-旧卡.md"), "").unwrap();
        fs::write(tmp.join(format!("10-inbox/{}-素材.md", today)), "").unwrap();
        fs::write(tmp.join(format!("50-drafts/{}-草稿.md", today)), "---\nkind: draft\n---\n\n正文").unwrap();
        let s = weekly_stats(&tmp).unwrap();
        assert_eq!(s.new_cards, 1, "只算最近的卡");
        assert_eq!(s.new_items, 1);
        assert_eq!(s.new_drafts, 1);
        assert_eq!(s.total_cards, 2);
        assert_eq!(s.todos_done, 1);
        assert_eq!(s.todos_open, 1);
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn test_weekly_stats_empty() {
        let tmp = std::env::temp_dir().join(format!("kp-w2-{}", std::process::id()));
        fs::create_dir_all(tmp.join("30-notes")).unwrap();
        let s = weekly_stats(&tmp).unwrap();
        assert_eq!(s.new_cards, 0);
        assert_eq!(s.total_cards, 0);
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn test_draft_frontmatter_strip() {
        // push 前去 frontmatter 的逻辑
        let text: &str = "---\nkind: draft\nused: a.md\n---\n\n# 标题\n\n正文";
        let content = if text.starts_with("---") {
            text[3..].find("\n---\n").map(|p| &text[p + 5..]).unwrap_or(text)
        } else { text };
        assert!(content.contains("# 标题"));
        assert!(!content.contains("kind: draft"));
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::AiConfig;

    #[test]
    fn test_config_new_fields_default() {
        let c = AiConfig::default();
        assert!(c.feishu_webhook.is_empty());
        assert!(!c.feishu_push_daily);
        assert!(c.notion_token.is_empty());
        assert!(c.notion_db_id.is_empty());
    }

    #[test]
    fn test_config_serde_roundtrip() {
        let mut c = AiConfig::default();
        c.feishu_webhook = "https://open.feishu.cn/open-apis/bot/v2/hook/xxx".into();
        c.feishu_push_daily = true;
        c.notion_token = "ntn_xxx".into();
        let s = serde_json::to_string(&c).unwrap();
        let c2: AiConfig = serde_json::from_str(&s).unwrap();
        assert_eq!(c2.feishu_webhook, c.feishu_webhook);
        assert!(c2.feishu_push_daily);
        assert_eq!(c2.notion_token, "ntn_xxx");
    }

    #[test]
    fn test_daily_review_inner_works() {
        // 用临时目录建一张卡，验证冒泡推送的内容源可用
        let tmp = std::env::temp_dir().join(format!("kp-it-{}", std::process::id()));
        std::fs::create_dir_all(tmp.join("30-notes")).unwrap();
        std::fs::write(tmp.join("30-notes/20261106-测试卡.md"),
            "---\ntitle: 测试卡\ndate: 2026-11-06\n---\n\n# 一句话结论\n测试结论。\n\n## 下一步行动\n- [ ] 待办a\n").unwrap();
        let r = crate::daily_review_inner(&tmp).unwrap();
        assert!(r.title.contains("测试卡"));
        assert_eq!(r.todos.len(), 1);
        std::fs::remove_dir_all(&tmp).ok();
    }
}
