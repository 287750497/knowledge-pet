# 🐱 Knowledge Pet · 知识收藏桌宠

电脑右下角常驻一只猫咪，**一键收剪贴板 → AI 整理成知识卡**。

> 灵感组合：「桌宠陪伴」+「剪贴板快捕」+「AI 知识管理」，纯本地 Markdown + Git 友好。

## ✨ 功能

| 操作 | 效果 |
|---|---|
| **单击猫咪** | 收集剪贴板内容（自动判断网址/文本），嘴角冒泡 📥 确认 |
| **双击猫咪** | 打开知识收藏面板（统计 / 筛选 / 抓取 / AI 整理 / 设置） |
| **网址收藏** | 自动进抓取队列，定时爬取正文 |
| **AI 整理成卡** | 调用你配置的 API，生成「一句话结论 / 展开 / 下一步行动」知识卡 |
| **状态流转** | 🌱新收 → 📄已爬正文 → 👁已读 → ✨已成卡 |
| **托盘常驻** | 关闭面板只是隐藏，托盘右键唤起/退出 |

- 收集的内容落在本地知识库 `~/knowledge-base/`（`10-inbox` 收件箱、`30-notes` 知识卡）
- 纯 Markdown 文件，随时可用 Git 管版本、用 Obsidian 等编辑器打开
- AI 配置存本地 `.pet-config.json`，不上传、不打包

## 🛠 技术栈

Tauri 2 + Vue 3 + TypeScript + Rust（reqwest / serde），零重型依赖。

## 🚀 从源码运行

```bash
# 前置：Node.js 18+、Rust（MSVC toolchain + VS Build Tools）
npm install --registry=https://registry.npmmirror.com
npm run tauri dev      # 开发模式
npm run tauri build    # 打包 exe
```

## ⚙ 配置 AI

面板 → ⚙ 设置 → 填 OpenAI 兼容接口三项（base_url / API Key / 模型名）→ 保存。
任何 OpenAI 兼容服务均可（如 `https://api.xxx.com/v1`）。

## 📋 Roadmap

- [ ] 全局热键 Ctrl+Alt+C 快速收集
- [ ] 重复收藏去重提示
- [ ] 微信/知乎反爬页抓取失败兜底引导
- [ ] 每日一句：从旧知识卡冒泡复习
- [ ] 划屏截图收藏

## License

MIT
