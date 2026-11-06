// ================= 集成：飞书 webhook + Notion 连接 =================
// 飞书：自定义机器人 webhook，POST JSON 即推送（无需建应用）
// Notion：internal connection token，官方 REST API

use crate::PetState;
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
