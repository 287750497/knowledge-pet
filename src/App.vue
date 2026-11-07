<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { readText } from "@tauri-apps/plugin-clipboard-manager";
import PetSprite from "./components/PetSprite.vue";

interface Item { id: string; file: string; title: string; kind: string; url: string; status: string; created_at: string; error: string; keywords?: string }
interface AiConfig { base_url: string; api_key: string; model: string; hotkey: string; kb_dir_override: string;
  prompt_article?: string; prompt_wechat?: string; prompt_quote?: string; prompt_topics?: string; prompt_daily?: string;
  providers: ProviderCfg[]; active_provider: number; image_provider: number; image_model: string;
  feishu_webhook: string; feishu_push_daily: boolean; notion_token: string; notion_db_id: string }
interface Bookmark { title: string; url: string }
interface Stats { total: number; seed: number; fetched: number; "fetch-failed": number; read: number; carded: number; queue: number; notes: number }

const view = ref<"pet" | "panel" | "settings">("pet");
// 宠物窗守卫：pet 窗永远只显示宠物（曾因 hide 权限失败把宠物窗变成面板小窗）
watch(view, (v) => {
  import("@tauri-apps/api/window").then(({ getCurrentWindow }) => {
    if (getCurrentWindow().label === "pet" && v !== "pet") view.value = "pet";
  });
});
const items = ref<Item[]>([]);
const st = ref<Stats | null>(null);
const cfg = ref<AiConfig>({ base_url: "", api_key: "", model: "", hotkey: "Ctrl+Alt+C", kb_dir_override: "", prompt_article: "", prompt_wechat: "", prompt_quote: "", prompt_topics: "", prompt_daily: "",
  providers: [], active_provider: 0, image_provider: -1, image_model: "", feishu_webhook: "", feishu_push_daily: false, notion_token: "", notion_db_id: "" });
const bookmarks = ref<Bookmark[]>([]);
const bmNew = ref({ title: "", url: "" });
const kbNew = ref("");
const isDragging = ref(false);
const bubble = ref("");
const bubbleShow = ref(false);
const busy = ref<Record<string, boolean>>({});
const filter = ref<"all" | "seed" | "fetched" | "read" | "carded">("all");
let bubbleTimer: number | undefined;
let pollTimer: number | undefined;

// v0.4：宠物状态机（PetSprite 暴露 poke）+ 右键菜单
const petSprite = ref<InstanceType<typeof PetSprite> | null>(null);
const petMood = ref("idle");
const menuShow = ref(false);
document.addEventListener("click", () => { menuShow.value = false; });
// 点到窗口外（失焦）自动收菜单
window.addEventListener("blur", () => { menuShow.value = false; });
// 兜底：菜单开着时轮询全局鼠标位置，移出宠物窗口即收（点击桌面任何其他地方/别的应用都生效）
let menuPoll: number | null = null;
watch(menuShow, async (on) => {
  if (!on) { if (menuPoll) { clearInterval(menuPoll); menuPoll = null; } return; }
  try {
    const { getCurrentWindow, cursorPosition } = await import("@tauri-apps/api/window");
    const cur = getCurrentWindow();
    menuPoll = window.setInterval(async () => {
      try {
        const pos = await cur.outerPosition();
        const size = await cur.outerSize();
        const m = await cursorPosition(); // 全局物理坐标
        const cx = m.x - pos.x, cy = m.y - pos.y;
        const outside = cx < -40 || cy < -40 || cx > size.width + 40 || cy > size.height + 40;
        if (outside) menuShow.value = false;
      } catch {}
    }, 150);
  } catch {}
});
function petPoke(mood: "happy" | "eat", ms?: number) {
  petSprite.value?.poke(mood, ms);
  petMood.value = mood;
  window.setTimeout(() => { petMood.value = "idle"; }, ms ?? 1200);
}
async function quitApp() {
  try {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    const { emit } = await import("@tauri-apps/api/event");
    await emit("pet-quit", true);  // 任意窗监听，Rust 侧退出
    getCurrentWindow().destroy().catch(() => {});
  } catch { window.close(); }
}
async function openPanelForSettings() {
  // 右键菜单的设置：唤起 main 面板窗并切到设置视图（pet 小窗放不下设置页）
  try {
    const { getCurrentWindow, Window } = await import("@tauri-apps/api/window");
    const { emitTo } = await import("@tauri-apps/api/event");
    const main = await Window.getByLabel("main");
    if (main) { await main.show(); await main.setFocus(); await emitTo("main", "switch-view", "settings"); }
    const cur = getCurrentWindow();
    if (cur.label === "pet") await cur.hide();
  } catch (e) { showBubble("⚠️ 打开设置失败"); }
}

const shown = computed(() => {
  let arr = filter.value === "all" ? items.value : items.value.filter(i => i.status === filter.value);
  if (kwFilter.value) arr = arr.filter(i => (i.keywords || "").includes(kwFilter.value));
  return arr;
});

async function refresh() {
  items.value = await invoke<Item[]>("list_items");
  st.value = await invoke<Stats>("stats");
  try { await loadCards(); } catch { /* 旧库无 30-notes */ }
}

// 桌宠单击：收剪贴板
async function petClick() {
  if (menuShow.value) { menuShow.value = false; return; }
  petPoke("happy", 1000);
  try {
    let text = "";
    try { text = await readText(); } catch { text = ""; }
    if (!text || !text.trim()) { showBubble("⚠️ 剪贴板是空的，先复制点东西"); return; }
    const it = await invoke<Item>("capture", { text });
    showBubble(it.kind === "url" ? "📥 收到网址，排队抓取" : "📥 收到，已放进收件箱");
    refresh();
  } catch (e) {
    showBubble("⚠️ " + String(e).replace(/^.*：/, ""));
  }
}

let dragTimer: number | undefined;
function petDrag(e: PointerEvent) {
  // 按住（220ms 未抬手）=系统拖拽；快速单击/双击则取消拖拽让 dblclick 生效
  if (e.button !== 0) return;
  clearTimeout(dragTimer);
  dragTimer = window.setTimeout(() => {
    isDragging.value = true;
    (async () => {
      try {
        const { getCurrentWindow } = await import("@tauri-apps/api/window");
        await getCurrentWindow().startDragging();
      } catch {}
      setTimeout(() => { isDragging.value = false; }, 300);
    })();
  }, 220);
  const cancel = () => { clearTimeout(dragTimer); window.removeEventListener("pointerup", cancel); };
  window.addEventListener("pointerup", cancel);
}

async function bowlClick() {
  if (menuShow.value) { menuShow.value = false; return; }
  petPoke("eat", 1400);
  try {
    let text = "";
    try { text = await readText(); } catch { text = ""; }
    if (!text || !text.trim()) { showBubble("⚠️ 碗是空的，先复制点东西"); return; }
    const it = await invoke<Item>("capture", { text });
    showBubble(it.kind === "url" ? "🍚 吃掉一个网址" : "🍚 吃掉一段文字");
    refresh();
  } catch (e) {
    showBubble("⚠️ " + String(e).replace(/^.*：/, ""));
  }
}

function showBubble(msg: string) {
  bubble.value = msg;
  bubbleShow.value = true;
  clearTimeout(bubbleTimer);
  bubbleTimer = window.setTimeout(() => (bubbleShow.value = false), 2200);
}

async function crawlAll() {
  showBubble("🕷️ 开始抓取网页…（每篇间隔 3 秒防限流）");
  for (let i = 0; i < 20; i++) {
    try {
      const msg = await invoke<string>("process_queue");
      showBubble("✓ " + msg);
      await new Promise(r => setTimeout(r, 3000)); // 防微信限流（数海丹心教训）
    } catch (e) {
      if (String(e).includes("队列空")) break;
      showBubble("⚠️ " + String(e));
    }
  }
  showBubble("✅ 队列抓取完毕");
  refresh();
}

async function makeCard(it: Item) {
  busy.value[it.id] = true;
  try {
    showBubble("✨ " + await invoke<string>("ai_make_card", { id: it.id }));
  } catch (e) { showBubble("⚠️ " + String(e)); }
  busy.value[it.id] = false;
  refresh();
}

async function markRead(it: Item) {
  await invoke("mark_read", { id: it.id });
  refresh();
}

async function delItem(it: Item) {
  try { showBubble("🗑 " + await invoke<string>("delete_item", { id: it.id })); }
  catch (e) { showBubble("⚠️ " + String(e)); }
  refresh();
}

async function loadBookmarks() {
  bookmarks.value = await invoke<Bookmark[]>("list_bookmarks");
}

async function addBm() {
  try {
    await invoke("add_bookmark", { title: bmNew.value.title, url: bmNew.value.url });
    bmNew.value = { title: "", url: "" };
    showBubble("✓ 已加入常用网址");
    loadBookmarks();
  } catch (e) { showBubble("⚠️ " + String(e).replace(/^.*：/, "")); }
}

async function delBm(url: string) {
  await invoke("remove_bookmark", { url });
  loadBookmarks();
}

// ---- 知识卡工坊（引用创作/跨卡选题/每日冒泡）----
interface CardItem { file: string; title: string; tags: string; status: string; url: string; created: string; conclusion: string }
const cards = ref<CardItem[]>([]);
const drafts = ref<CardItem[]>([]);
const picked = ref<Set<string>>(new Set());
const goal = ref("");
const topics = ref<string[]>([]);
const review = ref<{ card: CardItem; todos: string[] } | null>(null);

function togglePick(f: string) {
  const s = new Set(picked.value);
  if (s.has(f)) s.delete(f); else if (s.size < 5) s.add(f); else { showBubble("一次最多勾 5 张"); return; }
  picked.value = s;
}
async function loadCards() {
  cards.value = await invoke<CardItem[]>("list_cards");
  drafts.value = await invoke<CardItem[]>("list_drafts");
}
async function doQuoteCreate() {
  if (!goal.value.trim()) { showBubble("先写写作任务"); return; }
  busy.value = { ...busy.value, quote: true };
  try {
    showBubble("✍️ " + await invoke<string>("quote_create", { files: [...picked.value], goal: goal.value }));
    goal.value = ""; picked.value = new Set();
    await loadCards();
  } catch (e) { showBubble("⚠️ " + String(e).replace(/^.*：/, "")); }
  finally { busy.value = { ...busy.value, quote: false }; }
}
async function doGenTopics() {
  busy.value = { ...busy.value, topics: true };
  try { topics.value = await invoke<string[]>("gen_topics"); }
  catch (e) { showBubble("⚠️ " + String(e).replace(/^.*：/, "")); }
  finally { busy.value = { ...busy.value, topics: false }; }
}
async function doDailyReview() {
  try { review.value = await invoke<any>("daily_review"); }
  catch (e) { showBubble("⚠️ " + String(e).replace(/^.*：/, "")); }
}

const moving = ref(false);
const prog = ref(0);
const moveMsg = ref("");

async function browseKb() {
  try {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const sel = await open({ directory: true, multiple: false, title: "选择知识库目录" });
    if (typeof sel === "string" && sel) kbNew.value = sel;
  } catch (e) { showBubble("⚠️ " + String(e)); }
}

async function moveKb() {
  if (!kbNew.value.trim() || moving.value) return;
  moving.value = true; prog.value = 5; moveMsg.value = "🚚 正在迁移…";
  try {
    const msg = await invoke<string>("move_kb_dir", { newDir: kbNew.value });
    prog.value = 100; moveMsg.value = "✓ " + msg;
    cfg.value = await invoke<AiConfig>("get_config");
    showBubble("✓ 知识库已迁移");
  } catch (e) { moveMsg.value = "⚠️ " + String(e); prog.value = 0; }
  moving.value = false;
  setTimeout(() => { prog.value = 0; }, 3000);
  refresh();
}

async function saveConfig() {
  await invoke("set_config", {
    baseUrl: cfg.value.base_url,
    apiKey: cfg.value.api_key,
    model: cfg.value.model,
    promptArticle: cfg.value.prompt_article ?? "",
    promptWechat: cfg.value.prompt_wechat ?? "",
    promptQuote: cfg.value.prompt_quote ?? "",
    promptTopics: cfg.value.prompt_topics ?? "",
    promptDaily: cfg.value.prompt_daily ?? "",
    providers: cfg.value.providers,
    activeProvider: cfg.value.active_provider,
    imageProvider: cfg.value.image_provider,
    imageModel: cfg.value.image_model,
  });
  showBubble("✓ 设置已保存");
  view.value = "panel";
}

async function testConfig() {
  try {
    await invoke("set_config", {
      baseUrl: cfg.value.base_url,
      apiKey: cfg.value.api_key,
      model: cfg.value.model,
    });
    const items0 = await invoke<Item[]>("list_items");
    if (!items0.length) { showBubble("⚠️ 先收一条内容再测试"); return; }
    showBubble("🧪 测试中…");
    const msg = await invoke<string>("ai_make_card", { id: items0[0].id });
    showBubble("✓ AI 通了：" + msg);
  } catch (e) { showBubble("✗ " + String(e).slice(0, 80)); }
  refresh();
}

// ---- 关键词筛选 ----
const kwFilter = ref("");
// ---- 贴文本抠链接 ----
const pasteBox = ref(false);
const pasteText = ref("");
const pasteMsg = ref("");
async function capturePaste() {
  pasteMsg.value = "抠取中…";
  try { pasteMsg.value = "✅ " + await invoke<string>("capture_text_urls", { text: pasteText.value }); pasteText.value = ""; refresh(); }
  catch (e) { pasteMsg.value = "❌ " + String(e).slice(0, 120); }
}

// ---- 设置页标签 ----
const setTab = ref<"general" | "ai" | "notion" | "feishu">("ai");
interface ModelEntry { id: string; kind: string }
const modelsList = ref<ModelEntry[]>([]);
const modelsBusy = ref(false);
const modelsMsg = ref("");
const openProv = ref(-1);
interface ProviderCfg { name: string; base_url: string; api_key: string; model: string; image_model: string }
function addProvider() {
  cfg.value.providers.push({ name: "", base_url: "", api_key: "", model: "", image_model: "" });
  openProv.value = cfg.value.providers.length - 1;
}
function delProvider(pi: number) {
  cfg.value.providers.splice(pi, 1);
  if (cfg.value.active_provider >= cfg.value.providers.length) cfg.value.active_provider = 0;
  if (cfg.value.image_provider === pi) cfg.value.image_provider = -1;
}
function pickModel(pi: number, mm: ModelEntry) {
  const p = cfg.value.providers[pi];
  if (mm.kind === "image") { p.image_model = mm.id; cfg.value.image_provider = pi; cfg.value.image_model = mm.id; }
  else { p.model = mm.id; cfg.value.active_provider = pi; }
  modelsList.value = [];
}
const curTextModel = computed(() => cfg.value.providers[cfg.value.active_provider]?.model || cfg.value.model || "");
const curImageModel = computed(() => cfg.value.image_model || "");
async function fetchModels(pi: number) {
  modelsBusy.value = true; modelsMsg.value = ""; modelsList.value = [];
  try {
    const p = cfg.value.providers[pi];
    if (!p.base_url.trim()) { modelsMsg.value = "先填该厂商的 API 地址"; modelsBusy.value = false; return; }
    // 先保存当前全部配置，保证后端拿到最新值
    await saveConfig();
    modelsList.value = (await invoke<string[]>("fetch_models", { providerIndex: pi }))
      .map(s => { const [id, kind] = s.split("|"); return { id, kind }; });
    if (!modelsList.value.length) modelsMsg.value = "接口没返回模型";
  } catch (e) { modelsMsg.value = "❌ " + String(e).slice(0, 160); }
  modelsBusy.value = false;
}
const itgMsg = ref("");

// ---- 微信文章采集（借鉴 wechat-url-capture）----
interface WxArticle { url: string; title: string; author: string; time: string; visits: number }
const wxDays = ref(7);
const wxArts = ref<WxArticle[]>([]);
const wxPicked = ref<Set<string>>(new Set());
const wxMsg = ref("");
const wxBusy = ref("");
function toggleWx(url: string) {
  const s = new Set(wxPicked.value);
  if (s.has(url)) s.delete(url); else s.add(url);
  wxPicked.value = s;
}
async function wxScan() {
  wxBusy.value = "scan"; wxMsg.value = "读取微信浏览记录中…";
  try {
    const r = await invoke<{ articles: WxArticle[]; note: string }>("wx_capture", { days: wxDays.value || 7 });
    wxArts.value = r.articles;
    wxMsg.value = "✅ " + r.note + (r.articles.length ? "，勾选后入库" : "");
  } catch (e) { wxMsg.value = "❌ " + String(e).slice(0, 160); }
  wxBusy.value = "";
}
async function wxSave() {
  const pickedArts = wxArts.value.filter(a => wxPicked.value.has(a.url));
  if (!pickedArts.length) { wxMsg.value = "先勾选文章"; return; }
  wxBusy.value = "save"; wxMsg.value = "入库中…";
  try { wxMsg.value = "✅ " + await invoke<string>("wx_save_to_inbox", { articles: pickedArts }); loadCards(); }
  catch (e) { wxMsg.value = "❌ " + String(e).slice(0, 160); }
  wxBusy.value = "";
}
async function wxCards() {
  const pickedArts = wxArts.value.filter(a => wxPicked.value.has(a.url));
  if (!pickedArts.length) { wxMsg.value = "先勾选文章"; return; }
  wxBusy.value = "cards"; wxMsg.value = "AI 整理中，可能要等一会…";
  try { wxMsg.value = "✅ " + await invoke<string>("wx_make_cards", { articles: pickedArts }); loadCards(); }
  catch (e) { wxMsg.value = "❌ " + String(e).slice(0, 160); }
  wxBusy.value = "";
}
async function wxExport() {
  wxBusy.value = "export"; wxMsg.value = "导出公众号列表中…";
  try { wxMsg.value = "✅ " + await invoke<string>("wx_export_gzh"); }
  catch (e) { wxMsg.value = "❌ " + String(e).slice(0, 160); }
  wxBusy.value = "";
}
// ---- 公众号实时监听（v0.9，零 hook 零订阅）----
interface WatchTarget { name: string; biz: string }
const watchCfg = ref<{ targets: WatchTarget[]; interval_min: number; fetch_content: boolean; feishu_notify: boolean }>({ targets: [], interval_min: 30, fetch_content: true, feishu_notify: true });
const watchNewName = ref(""); const watchNewBiz = ref("");
const watchMsg = ref(""); const watchBusy = ref(false);
async function loadWatch() {
  try { watchCfg.value = await invoke("watch_list"); } catch {}
}
async function watchAdd() {
  if (!watchNewBiz.value.trim()) { watchMsg.value = "biz 不能为空"; return; }
  watchBusy.value = true;
  try { watchMsg.value = "✅ " + await invoke<string>("watch_add", { name: watchNewName.value || "公众号", biz: watchNewBiz.value }); watchNewName.value = ""; watchNewBiz.value = ""; loadWatch(); }
  catch (e) { watchMsg.value = "❌ " + String(e).slice(0, 160); }
  watchBusy.value = false;
}
async function watchRemove(biz: string) {
  try { watchMsg.value = await invoke<string>("watch_remove", { biz }); loadWatch(); } catch (e) { watchMsg.value = "❌ " + String(e).slice(0, 160); }
}
async function watchImport() {
  watchBusy.value = true; watchMsg.value = "从浏览记录导入中…";
  try { watchMsg.value = "✅ " + await invoke<string>("watch_import_from_capture"); loadWatch(); }
  catch (e) { watchMsg.value = "❌ " + String(e).slice(0, 160); }
  watchBusy.value = false;
}
async function watchCheck() {
  watchBusy.value = true; watchMsg.value = "检查新文章中…";
  try { const r = await invoke<{ checked: number; found: string[]; note: string }>("watch_check");
    watchMsg.value = r.found.length ? "✅ 发现 " + r.found.length + " 篇新文：" + r.found.join("、") : "✅ 检查 " + r.checked + " 个号，无新文" + (r.note ? "（" + r.note + "）" : ""); }
  catch (e) { watchMsg.value = "❌ " + String(e).slice(0, 160); }
  watchBusy.value = false;
}
// 自动轮询：每 interval_min 分钟一次
let watchTimer: number | undefined;
onMounted(async () => {
  loadWatch();
  try { const c = await invoke("watch_list"); const cfg = c as any; clearInterval(watchTimer); watchTimer = window.setInterval(async () => { try { await invoke("watch_auto_tick"); } catch {} }, Math.max(5, cfg.interval_min || 30) * 60000); } catch {}
});
const notionPreviewRows = ref<string[]>([]);


async function saveIntegrations() {
  try {
    await invoke("save_integrations", {
      feishuWebhook: cfg.value.feishu_webhook,
      feishuPushDaily: cfg.value.feishu_push_daily,
      notionToken: cfg.value.notion_token,
      notionDbId: cfg.value.notion_db_id,
    });
    showBubble("✓ 集成配置已保存");
    itgMsg.value = "";
  } catch (e) { showBubble("✗ " + String(e).slice(0, 80)); }
}

async function testFeishu() {
  itgMsg.value = "发送测试消息中…";
  try {
    await invoke("save_integrations", {
      feishuWebhook: cfg.value.feishu_webhook, feishuPushDaily: cfg.value.feishu_push_daily,
      notionToken: cfg.value.notion_token, notionDbId: cfg.value.notion_db_id,
    });
    await invoke("feishu_test");
    itgMsg.value = "✅ 测试消息已发到飞书群，去手机上看一眼";
  } catch (e) { itgMsg.value = "❌ " + String(e).slice(0, 120); }
}

async function pushDailyNow() {
  itgMsg.value = "推送中…";
  try {
    await invoke("feishu_push_daily");
    itgMsg.value = "✅ 每日冒泡已推送到飞书";
  } catch (e) { itgMsg.value = "❌ " + String(e).slice(0, 120); }
}

async function testNotion() {
  itgMsg.value = "连接 Notion 中…";
  try {
    const msg = await invoke<string>("notion_test", { token: cfg.value.notion_token, dbId: cfg.value.notion_db_id });
    itgMsg.value = msg;
  } catch (e) { itgMsg.value = "❌ " + String(e).slice(0, 160); }
}

async function previewNotion() {
  itgMsg.value = "拉取中…";
  try {
    notionPreviewRows.value = await invoke<string[]>("notion_preview", { token: cfg.value.notion_token, dbId: cfg.value.notion_db_id, limit: 5 });
    itgMsg.value = notionPreviewRows.value.length ? "✅ 最近 " + notionPreviewRows.value.length + " 条：" : "数据库为空";
  } catch (e) { itgMsg.value = "❌ " + String(e).slice(0, 160); }
}

async function syncNotion() {
  itgMsg.value = "同步中（拉取 → AI 整理），可能要等一会…";
  try {
    const msg = await invoke<string>("notion_sync_to_cards", { limit: 5 });
    itgMsg.value = "✅ " + msg;
  } catch (e) { itgMsg.value = "❌ " + String(e).slice(0, 160); }
}

async function pushDraftToNotion(file: string) {
  itgMsg.value = "推送草稿到 Notion 中…";
  try {
    const msg = await invoke<string>("push_draft_to_notion", { file });
    itgMsg.value = "✅ " + msg;
  } catch (e) { itgMsg.value = "❌ " + String(e).slice(0, 160); }
}

async function pushWeekly() {
  itgMsg.value = "生成周报并推送中…";
  try {
    await invoke("feishu_push_weekly");
    itgMsg.value = "✅ 周报已推送到飞书";
  } catch (e) { itgMsg.value = "❌ " + String(e).slice(0, 160); }
}

const statusText: Record<string, string> = {
  seed: "🌱 新收", fetched: "📄 已爬正文", "fetch-failed": "❌ 抓取失败",
  read: "👁 已读", carded: "✨ 已成卡",
};

onMounted(async () => {
  // 按窗口分流：pet 窗口显示桌宠，main 窗口显示面板
  const { listen } = await import("@tauri-apps/api/event");
  try {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    const cur = getCurrentWindow();
    if (cur.label !== "pet") {
      // 面板最小化 → 显示桌宠
      const unR = await cur.onResized(({ payload: size }) => {
        import("@tauri-apps/api/window").then(async ({ Window }) => {
          // Tauri2: 最小化时逻辑尺寸为 0 或物理宽高为 0
          if (size.width === 0 || size.height === 0) {
            view.value = "pet";
            const pet = await Window.getByLabel("pet");
            if (pet) { await pet.show(); await pet.setFocus(); }
          }
        });
      });
      onUnmounted(() => unR());
    }
  } catch {}
  const unP = await listen<string>("kb-move-progress", (e) => {
    const [cur, total] = String(e.payload).split("|").map(Number);
    prog.value = Math.round(5 + (cur / Math.max(total, 1)) * 90);
    moveMsg.value = `🚚 已迁移 ${cur}/${total} 个目录…`;
  });
  onUnmounted(() => unP());
  const un = await listen<string>("switch-view", (e) => {
    const v = e.payload;
    if (v === "panel" || v === "settings" || v === "pet") view.value = v as any;
  });
  onUnmounted(() => un());
  try {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    const label = getCurrentWindow().label;
    view.value = label === "pet" ? "pet" : "panel";
  } catch { view.value = "panel"; }
  cfg.value = await invoke<AiConfig>("get_config");
  refresh();
  loadBookmarks();
  pollTimer = window.setInterval(refresh, 5000);
  dailyTimer = window.setInterval(checkDailyPush, 60000);
  // 面板被最小化 = 收起回桌宠（Windows 最小化物理尺寸变 0）
  (async () => {
    try {
      const { getCurrentWindow } = await import("@tauri-apps/api/window");
      const cur = getCurrentWindow();
      if (cur.label !== "pet") {
        const un = await cur.onResized(({ payload: size }) => {
          if (size.width === 0 && size.height === 0) backToPet();
        });
        onUnmounted(() => un());
      }
    } catch {}
  })();
  // 全局热键触发主动抓取（在 pet 窗口执行收集）
  const un2 = await listen("hotkey-capture", async () => {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    if (getCurrentWindow().label !== "pet") return;
    try {
      let text = "";
      try { text = await readText(); } catch { text = ""; }
      if (!text || !text.trim()) { showBubble("⚠️ 剪贴板是空的"); return; }
      const it = await invoke<Item>("capture", { text });
      showBubble(it.kind === "url" ? "📥 收到网址，排队抓取" : "📥 收到，已放进收件箱");
      refresh();
    } catch (e) { showBubble("⚠️ " + String(e).replace(/^.*：/, "")); }
  });
  onUnmounted(() => un2());
});

// 回到桌宠：显示 pet 窗、隐藏/最小化 main
async function backToPet() {
  try {
    const { getCurrentWindow, Window } = await import("@tauri-apps/api/window");
    const cur = getCurrentWindow();
    if (cur.label === "pet") { view.value = "pet"; return; } // pet 窗永远只显示宠物
    const pet = await Window.getByLabel("pet");
    if (pet) {
      await pet.show();
      await pet.setAlwaysOnTop(true); // 防止被其他窗口盖住
      await pet.setFocus();
    }
    await cur.hide();          // 面板隐藏（托盘/双击桌宠可再唤起）
  } catch (e) {
    // 兜底：至少把 pet 显示出来
    try {
      const { Window } = await import("@tauri-apps/api/window");
      const pet = await Window.getByLabel("pet");
      if (pet) await pet.show();
    } catch {}
  }
}

// pet 窗口切到面板时，显示 main 窗口、隐藏自己
async function openPanel() {
  try {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    const cur = getCurrentWindow();
    if (cur.label === "pet") {
      const { Window } = await import("@tauri-apps/api/window");
      const main = await Window.getByLabel("main");
      if (main) {
        try { await main.show(); } catch (e) { showBubble("show: " + String(e).slice(0, 60)); }
        try { await main.setFocus(); } catch (e) { showBubble("focus: " + String(e).slice(0, 60)); }
        try { const { emitTo } = await import("@tauri-apps/api/event"); await emitTo("main", "switch-view", "panel"); } catch (e) { showBubble("emit: " + String(e).slice(0, 60)); }
      } else { showBubble("main 窗未找到"); }
      await cur.hide();
      return;
    }
    view.value = "panel";
  } catch (e) {
    // pet 窗兜底：失败也不能把自己变成面板（宠物窗必须始终显示宠物）
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    if (getCurrentWindow().label === "pet") { view.value = "pet"; return; }
    view.value = "panel";
  }
}

// main 窗口关闭时隐藏而非退出（托盘常驻）
// 用 Rust 侧 on_window_event 处理，前端只管 UI
// 每日 9 点飞书推送：每分钟检查一次，当天推过就不再推
let dailyTimer: number | undefined;
let lastPushDate = "";
async function checkDailyPush() {
  const now = new Date();
  const hm = now.getHours() * 60 + now.getMinutes();
  const today = now.toDateString();
  if (hm >= 540 && lastPushDate !== today && cfg.value.feishu_push_daily && cfg.value.feishu_webhook) {
    lastPushDate = today;
    try { await invoke("feishu_push_daily"); } catch (e) { console.warn("每日推送失败", e); }
  }
}
onUnmounted(() => { clearInterval(pollTimer); clearTimeout(bubbleTimer); clearInterval(dailyTimer); });
</script>

<template>
  <!-- 桌宠视图（v0.4：宠物+食盆同窗，把手拖动，右键菜单） -->
  <div v-if="view === 'pet'" class="petwrap" :class="[`mood-${petMood}`, { dragging: isDragging }]" @contextmenu.prevent="menuShow = true">
    <div v-if="bubbleShow" class="bubble">{{ bubble }}</div>

    <!-- 右键菜单 -->
    <div v-if="menuShow" class="pet-menu" @click.stop>
      <button @click="menuShow = false; petClick()">📥 收藏剪贴板</button>
      <button @click="menuShow = false; bowlClick()">🍚 喂食（收进食盆）</button>
      <button @click="menuShow = false; openPanel()">📚 管理面板</button>
      <button @click="menuShow = false; openPanelForSettings()">⚙ 设置</button>
      <button class="danger" @click="menuShow = false; quitApp()">🚪 退出</button>
    </div>

    <!-- 拖动把手（顶部横条，专用拖动区，不与单击/双击冲突） -->
    <div class="drag-handle" @pointerdown="petDrag" title="按住拖动整组">
      <span class="dots">⠿</span>
    </div>

    <!-- 宠物+食盆横排（同窗联动） -->
    <div class="petrow">
      <!-- 宠物（单击=收藏，双击=面板） -->
      <div class="pet" @dblclick="openPanel" @pointerdown="petDrag" title="按住拖动 / 双击面板 / 右键菜单">
        <PetSprite ref="petSprite" />
      </div>
      <!-- 食盆：有排队内容时冒星 -->
      <div class="bowl" :class="{ 'has-queue': st && st.queue > 0 }" @click="bowlClick" title="点食盆喂食 = 收藏剪贴板">
        🍚
        <span v-if="st && st.queue > 0" class="queue-star">⭐</span>
      </div>
    </div>

    <div class="pet-hint" v-if="st">📥 {{ st.total }} · 待爬 {{ st.queue }}</div>
  </div>

  <!-- 管理面板 -->
  <div v-else-if="view === 'panel'" class="panel">
    <header>
      <button class="mini" @click="backToPet">🐱 回到桌宠</button>
      <h2>📚 知识收藏面板</h2>
      <button class="mini" @click="view = 'settings'">⚙ 设置</button>
    </header>

    <div class="statrow" v-if="st">
      <span>共 {{ st.total }}</span>
      <span>🌱 {{ st.seed }}</span>
      <span>📄 {{ st.fetched }}</span>
      <span>👁 {{ st.read }}</span>
      <span>✨ {{ st.carded }}</span>
      <span>❌ {{ st["fetch-failed"] }}</span>
      <span>🗂 知识卡 {{ st.notes }}</span>
      <button class="mini" :disabled="!st.queue" @click="crawlAll">🕷 抓取队列({{ st.queue }})</button>
      <button class="mini" @click="pasteBox = !pasteBox">📋 贴文本抠链接</button>
    </div>
    <div v-if="pasteBox" style="margin:0 16px 8px">
      <textarea v-model="pasteText" rows="3" placeholder="把含链接的一段话/聊天记录/笔记粘进来，自动抠出所有 URL 去重入队" style="width:100%;font-size:12px"></textarea>
      <div style="display:flex;gap:6px;align-items:center;margin-top:4px">
        <button class="mini ok" :disabled="!pasteText.trim()" @click="capturePaste">🔍 抠出链接入队</button>
        <span v-if="pasteMsg" class="mut" style="font-size:12px">{{ pasteMsg }}</span>
      </div>
    </div>

    <div class="filters">
      <button v-for="f in ['all','seed','fetched','read','carded']" :key="f"
        :class="{ active: filter === f }" @click="filter = f as any">
        {{ f === 'all' ? '全部' : statusText[f] }}
      </button>
    </div>

    <div class="list">
      <div v-for="it in shown" :key="it.id" class="card">
        <div class="row1">
          <span class="badge" :class="it.kind">{{ it.kind === 'url' ? '🌐' : '📝' }}</span>
          <b class="title">{{ it.title }}</b>
          <span class="st">{{ statusText[it.status] || it.status }}</span>
        </div>
        <a v-if="it.url" class="url" :href="it.url" target="_blank">{{ it.url }}</a>
        <div class="btns">
          <button v-if="it.status === 'seed' && it.kind === 'url'" class="mini" @click="crawlAll">🕷 抓正文</button>
          <button class="mini ok" :disabled="busy[it.id]" @click="makeCard(it)">✨ AI 整理成卡</button>
          <button v-if="it.status !== 'read' && it.status !== 'carded'" class="mini" @click="markRead(it)">👁 标记已读</button>
          <button class="mini danger" @click="delItem(it)">🗑 删除</button>
        </div>
      </div>
      <p v-if="!shown.length" class="empty">空空如也。去别的窗口复制点东西，然后点一下桌宠 🐱</p>
    </div>

    <h3 class="bmtitle">🐱 每日冒泡 <button class="mini" @click="doDailyReview">抽一张</button></h3>
    <div v-if="review" class="cardblock" style="border-left:3px solid #f97316">
      <b>《{{ review.card.title }}》</b> <span class="mut">{{ review.card.tags }}</span>
      <p class="mut" style="margin:4px 0">{{ review.card.conclusion }}</p>
      <p v-if="review.todos.length" class="mut">待办：{{ review.todos.join("；") }}</p>
    </div>

    <h3 class="bmtitle">💡 跨卡选题 <button class="mini" :disabled="busy.topics" @click="doGenTopics">{{ busy.topics ? "思考中…" : "AI 碰撞 5 个选题" }}</button></h3>
    <div v-if="topics.length" class="cardblock">
      <p v-for="(t, i) in topics" :key="i" class="mut" style="margin:6px 0">{{ t }}</p>
    </div>

    <h3 class="bmtitle">✍️ 引用创作（勾 2-5 张卡，AI 照素材写稿）</h3>
    <div class="cardblock">
      <div v-for="c in cards" :key="c.file" class="cardrow" :class="{ picked: picked.has(c.file) }" @click="togglePick(c.file)">
        <input type="checkbox" :checked="picked.has(c.file)" style="pointer-events:none" />
        <div style="flex:1;min-width:0">
          <b>{{ c.title }}</b> <span class="mut">{{ c.tags }} · {{ c.created }}</span>
          <div class="mut" style="font-size:12px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">{{ c.conclusion }}</div>
        </div>
      </div>
      <p v-if="!cards.length" class="empty">还没有知识卡——先「AI 整理成卡」</p>
      <textarea v-model="goal" rows="2" placeholder="写作任务，如：结合这几张卡写一篇公众号开头（150字），观点用卡里原话"></textarea>
      <button class="primary" :disabled="busy.quote || !picked.size" @click="doQuoteCreate">{{ busy.quote ? "生成中…" : `用 ${picked.size} 张卡生成草稿` }}</button>
    </div>

    <div v-if="drafts.length" class="cardblock">
      <h3 class="bmtitle">📄 草稿（50-drafts/）</h3>
      <div v-for="d in drafts" :key="d.file" class="mut" style="margin:4px 0;font-size:12px">
        📄 {{ d.title }} <span class="mut">· {{ d.created }} · 用卡：{{ d.tags }}</span>
        <button class="mini" style="margin-left:8px" @click="pushDraftToNotion(d.file)">📖 推到 Notion</button>
      </div>
    </div>

    <div class="cardblock">
      <h3 class="bmtitle">📥 微信文章采集</h3>
      <div style="display:flex;gap:6px;flex-wrap:wrap;margin-bottom:6px">
        <label class="mut" style="font-size:12px;display:inline-flex;align-items:center;gap:4px;margin-right:4px">采集最近
        <select v-model.number="wxDays" style="font-size:12px;padding:1px 4px">
          <option :value="1">1 天</option><option :value="3">3 天</option><option :value="7">7 天</option>
          <option :value="14">14 天</option><option :value="30">30 天</option><option :value="90">90 天</option>
        </select> 浏览</label>
        <button class="mini" :disabled="!!wxBusy" @click="wxScan">{{ wxBusy === 'scan' ? "读取中…" : "🔍 抓取浏览记录" }}</button>
        <button class="mini ok" :disabled="!!wxBusy || !wxPicked.size" @click="wxSave">📥 存收件箱 ({{ wxPicked.size }})</button>
        <button class="mini ok" :disabled="!!wxBusy || !wxPicked.size" @click="wxCards">🤖 AI 成卡 ({{ wxPicked.size }})</button>
        <button class="mini" :disabled="!!wxBusy" @click="wxExport" title="导出公众号列表 JSON，供评论寻客导入">📤 导出公众号</button>
      </div>
      <p v-if="wxMsg" class="mut" style="font-size:12px;margin:4px 0">{{ wxMsg }}</p>
      <div v-for="a in wxArts" :key="a.url" class="mut" style="margin:4px 0;font-size:12px;display:flex;align-items:center;gap:6px">
        <input type="checkbox" :checked="wxPicked.has(a.url)" @change="toggleWx(a.url)" style="margin:0" />
        <span style="flex:1;overflow:hidden;text-overflow:ellipsis;white-space:nowrap" :title="a.title">📰 {{ a.title }}</span>
        <span class="mut" style="white-space:nowrap">{{ a.author || '未知号' }} · {{ a.time }}</span>
      </div>
      <p v-if="!wxArts.length" class="mut" style="font-size:12px">点「抓取浏览记录」读取你在微信里看过的公众号文章（只读本地记录，需 Windows + 微信 4.x）</p>
    </div>

    <div class="cardblock">
      <h3 class="bmtitle">👀 公众号实时监听</h3>
      <div style="display:flex;gap:6px;flex-wrap:wrap;margin-bottom:6px">
        <input v-model="watchNewName" placeholder="公众号名" style="width:110px;font-size:12px" />
        <input v-model="watchNewBiz" placeholder="biz（__biz= 参数）" style="width:170px;font-size:12px" @keyup.enter="watchAdd" />
        <button class="mini ok" :disabled="watchBusy" @click="watchAdd">➕ 添加</button>
        <button class="mini" :disabled="watchBusy" @click="watchImport" title="从 v0.8 浏览记录一键导入（Windows）">📥 从浏览记录导入</button>
        <button class="mini ok" :disabled="watchBusy" @click="watchCheck">🔍 立即检查新文</button>
      </div>
      <p v-if="watchMsg" class="mut" style="font-size:12px;margin:4px 0">{{ watchMsg }}</p>
      <div v-for="t in watchCfg.targets" :key="t.biz" class="mut" style="margin:4px 0;font-size:12px;display:flex;align-items:center;gap:6px">
        <span style="flex:1">👀 {{ t.name }} <span class="mut">({{ t.biz.slice(0, 14) }}…)</span></span>
        <button class="mini" @click="watchRemove(t.biz)">移除</button>
      </div>
      <p v-if="!watchCfg.targets.length" class="mut" style="font-size:12px">添加关注的公众号后，每 {{ watchCfg.interval_min }} 分钟自动检查新文章 → 存「监听新文」→ 飞书提醒。零 hook、不付费、数据不出你的机器。</p>
    </div>

    <h3 class="bmtitle">⭐ 常用网址</h3>
    <div class="bmform">
      <input v-model="bmNew.title" placeholder="名称（可空）" class="bm-title" />
      <input v-model="bmNew.url" placeholder="https://…  网址" class="bm-url" @keyup.enter="addBm" />
      <button class="mini ok" @click="addBm">＋ 添加</button>
    </div>
    <div class="bmgrid">
      <div v-for="b in bookmarks" :key="b.url" class="bmitem">
        <a :href="b.url" target="_blank" class="bmlink">{{ b.title }}</a>
        <button class="mini danger" @click="delBm(b.url)">✕</button>
      </div>
      <p v-if="!bookmarks.length" class="empty2">还没有常用网址</p>
    </div>
  </div>

  <!-- 设置页 -->
  <div v-else class="panel settings">
    <header>
      <button class="mini" @click="view = 'panel'">← 返回</button>
      <h2>⚙ 设置</h2>
    </header>
    <div class="settabs">
      <button :class="{ active: setTab === 'ai' }" @click="setTab = 'ai'">🤖 AI 模型</button>
      <button :class="{ active: setTab === 'general' }" @click="setTab = 'general'">⚙ 通用</button>
      <button :class="{ active: setTab === 'notion' }" @click="setTab = 'notion'">📖 Notion</button>
      <button :class="{ active: setTab === 'feishu' }" @click="setTab = 'feishu'">💬 飞书</button>
    </div>
    <form @submit.prevent="saveConfig" class="cfgform">

      <template v-if="setTab === 'ai'">
      <h3 class="sect">🤖 模型厂商（可配多家并存）</h3>
      <p class="tip">点厂商名展开：填 Key → 获取模型 → 保存。各家独立存 Key，互不冲突。</p>
      <div v-for="(p, pi) in cfg.providers" :key="pi" class="provcard">
        <div class="provhead" @click="openProv = openProv === pi ? -1 : pi">
          <span class="provname">{{ p.name || ('厂商 ' + (pi+1)) }}</span>
          <span class="provbadge" :class="p.api_key ? 'okb' : ''">{{ p.api_key ? '已配 Key' : '未配 Key' }}</span>
          <span class="mut" style="font-size:12px;margin-left:auto;display:flex;gap:8px;align-items:center">
            <label class="chkline" style="margin:0" @click.stop><input type="radio" name="textprov" :checked="cfg.active_provider === pi" @change="cfg.active_provider = pi" /> 文本用</label>
            <label class="chkline" style="margin:0" @click.stop><input type="radio" name="imgprov" :checked="cfg.image_provider === pi" @change="cfg.image_provider = pi" /> 图片用</label>
            <a @click.stop="delProvider(pi)" style="cursor:pointer">🗑</a>
          </span>
          <span>{{ openProv === pi ? '∨' : '›' }}</span>
        </div>
        <div v-if="openProv === pi" class="provbody" style="padding:8px 4px;display:block">
          <label>厂商名</label>
          <input v-model="p.name" placeholder="如：Agnes / 硅基流动 / 自建" />
          <label>API 地址（Base URL）</label>
          <input v-model="p.base_url" placeholder="https://api.example.com/v1" />
          <label>API Key</label>
          <input v-model="p.api_key" type="password" placeholder="sk-..." />
          <label>文本模型（model id）</label>
          <div class="rowsep">
            <input v-model="p.model" placeholder="gpt-4o-mini / deepseek-chat …" class="grow" />
            <button type="button" class="mini" :disabled="modelsBusy" @click="fetchModels(pi)">{{ modelsBusy ? "获取中…" : "⟳ 获取模型" }}</button>
          </div>
          <div v-if="modelsList.length" class="modellist">
            <div v-for="mm in modelsList" :key="mm.id" class="modelitem" @click="pickModel(pi, mm)">
              <span>{{ mm.kind === 'image' ? '🖼️' : '📝' }} {{ mm.id }}</span>
              <span class="mut" style="font-size:11px">{{ mm.kind === 'image' ? '图片' : '文本' }}</span>
            </div>
          </div>
          <p v-if="modelsMsg" class="mut" style="font-size:12px;margin:4px 0">{{ modelsMsg }}</p>
        </div>
      </div>
      <div class="rowsep" style="margin:8px 0">
        <button type="button" class="mini" @click="addProvider">＋ 添加模型</button>
      </div>
      <p v-if="curTextModel" class="tip">📝 文本模型用：<b>{{ curTextModel }}</b> ｜ 🖼️ 图片模型用：<b>{{ curImageModel || '未设置' }}</b></p>
<h3 class="sect">🎛️ 成卡/创作预设（AI 提示词模板）</h3>
      <p class="tip">每类内容用专属模板；留空 = 用内置默认。占位符会被自动替换。</p>
      <label>文章成卡模板（占位符：{url} {content}）</label>
      <textarea v-model="cfg.prompt_article" rows="4" placeholder="留空用默认：一句话结论/展开/下一步行动"></textarea>
      <label>微信文章成卡模板（占位符：{title} {author} {url}）</label>
      <textarea v-model="cfg.prompt_wechat" rows="4" placeholder="留空用默认：待补全卡模板"></textarea>
      <label>引用创作模板（占位符：{goal} {snapshot}）</label>
      <textarea v-model="cfg.prompt_quote" rows="4" placeholder="留空用默认：铁律=只用卡里的事实"></textarea>
      <label>跨卡选题模板（占位符：{digest}）</label>
      <textarea v-model="cfg.prompt_topics" rows="3" placeholder="留空用默认：两两碰撞出 5 个选题"></textarea>
      <label>每日冒泡推送文案模板（占位符：{date} {title} {conclusion} {todos}）</label>
      <textarea v-model="cfg.prompt_daily" rows="4" placeholder="留空用默认：🐱 每日冒泡 + 今日知识卡 + 待办"></textarea>
      <div class="rowsep">
        <button type="button" class="mini" @click="cfg.prompt_article=''; cfg.prompt_wechat=''; cfg.prompt_quote=''; cfg.prompt_topics=''; cfg.prompt_daily=''; showBubble('已清空，保存后恢复内置默认')">↺ 全部恢复默认</button>
      </div>
      <div class="btns">
        <button type="submit" class="ok">💾 保存</button>
        <button type="button" class="mini" @click="testConfig">🧪 测试连接（用最新一条做真实整理）</button>
      </div>
      </template>

      <template v-if="setTab === 'general'">
      <h3 class="sect">⌨️ 主动抓取热键</h3>
      <label>全局热键（任意窗口按下即收剪贴板）</label>
      <input v-model="cfg.hotkey" placeholder="Ctrl+Alt+C" />
      <p class="tip">格式：Ctrl/Shift/Alt/Super 组合 + 字母/数字，如 Ctrl+Alt+C、Ctrl+Shift+K。改完点保存生效（下次启动也生效）。</p>

      <h3 class="sect">📁 知识库目录</h3>
      <label>当前目录</label>
      <input :value="cfg.kb_dir_override || '默认（exe 所在目录/knowledge-pet）'" disabled />
      <label>迁移到新目录</label>
      <div class="rowsep">
        <input v-model="kbNew" placeholder="D:\知识库\knowledge-pet" class="grow" />
        <button type="button" class="mini" @click="browseKb">📂 浏览</button>
      </div>
      <div class="btns">
        <button type="button" class="mini ok" :disabled="moving || !kbNew.trim()" @click="moveKb">
          {{ moving ? "🚚 迁移中…" : "🚚 迁移（自动搬 inbox/知识卡/常用网址）" }}
        </button>
      </div>
      <div v-if="moving" class="progwrap"><div class="progbar" :style="{ width: prog + '%' }"></div></div>
      <p v-if="moveMsg" class="tip">{{ moveMsg }}</p>
      <p class="tip">默认知识库在软件（exe）所在目录下 knowledge-pet 文件夹，首次启动自动创建（含 10-inbox/20-sources/30-notes/40-sop/90-archive）。</p>
      </template>

      <template v-if="setTab === 'notion'">
      <h3 class="sect">📖 Notion 连接（手机记录 → 桌宠整理）</h3>
      <label>Internal Integration Token</label>
      <input v-model="cfg.notion_token" type="password" placeholder="ntn_..." />
      <label>数据库 ID（收集箱）</label>
      <input v-model="cfg.notion_db_id" placeholder="数据库 URL 里 32 位字符串" />
      <p class="tip">
        步骤：① notion.so/developers 建一个 Internal Integration，拿到 token（ntn_ 开头）；
        ② 在 Notion 里建「收集箱」数据库；③ 打开该数据库页面右上角 ••• → 连接 → 把你的 integration 加进去（不加这步必报 404）。
        ✅ 已支持：拉取新行 → AI 整理成本地知识卡（点下方「同步成知识卡」）。草稿也可一键推回 Notion（面板草稿条目上的按钮）。
      </p>
      <div class="btns">
        <button type="button" class="mini ok" @click="saveIntegrations">💾 保存</button>
        <button type="button" class="mini" @click="testNotion">🔌 测试连接</button>
        <button type="button" class="mini" @click="previewNotion">👀 预览最近 5 条</button>
        <button type="button" class="mini ok" @click="syncNotion">⚡ 同步成知识卡（拉最近 5 条 → AI 整理）</button>
      </div>
      <ul v-if="notionPreviewRows.length" class="notionpreview">
        <li v-for="r in notionPreviewRows" :key="r">{{ r }}</li>
      </ul>
      <p v-if="itgMsg" class="tip" :class="{ err: itgMsg.startsWith('❌') }">{{ itgMsg }}</p>
      </template>

      <template v-if="setTab === 'feishu'">
      <h3 class="sect">💬 飞书推送（知识冒泡 → 手机）</h3>
      <label>自定义机器人 Webhook 地址</label>
      <input v-model="cfg.feishu_webhook" placeholder="https://open.feishu.cn/open-apis/bot/v2/hook/..." />
      <p class="tip">
        步骤：飞书群里 → 设置 → 群机器人 → 添加机器人 → 自定义机器人 → 复制 Webhook 地址填到这里。
        机器人只在那个群里发消息，不用建应用、不用审核。
      </p>
      <label class="chkline"><input type="checkbox" v-model="cfg.feishu_push_daily" /> 每天早上 9 点自动推送「每日冒泡」到飞书</label>
      <div class="btns">
        <button type="button" class="mini ok" @click="saveIntegrations">💾 保存</button>
        <button type="button" class="mini" @click="testFeishu">💬 发测试消息</button>
        <button type="button" class="mini" @click="pushDailyNow">🐱 立即推一条今日冒泡</button>
        <button type="button" class="mini" @click="pushWeekly">📊 推送知识库周报</button>
      </div>
      <p v-if="itgMsg" class="tip" :class="{ err: itgMsg.startsWith('❌') }">{{ itgMsg }}</p>
      </template>
    </form>
  </div>
</template>

<style>
* { box-sizing: border-box; margin: 0; }
html, body { background: transparent !important; margin: 0; padding: 0; border: 0; overflow: hidden; outline: none; box-shadow: none; }
#app { background: transparent !important; border: 0; outline: none; }
body { font-family: system-ui, "Microsoft YaHei"; overflow: hidden; }

/* 食盆（同窗） */
.bowl { font-size: 34px; cursor: pointer; user-select: none; filter: drop-shadow(0 2px 4px rgba(0,0,0,.35)); transition: transform .15s; position: relative; line-height: 1; }
.bowl:hover { transform: scale(1.12); }
.bowl:active { transform: scale(0.9); }
.bowl .queue-star { position: absolute; top: -10px; right: -8px; font-size: 14px; animation: starpop 1.4s ease-in-out infinite; }
@keyframes starpop { 0%,100% { transform: translateY(0) scale(1); opacity: .7; } 50% { transform: translateY(-4px) scale(1.25); opacity: 1; } }
/* 进度条 */
.progwrap { height: 8px; background: #eee; border-radius: 999px; margin: 10px 0 4px; overflow: hidden; }
.progbar { height: 100%; background: #f97316; border-radius: 999px; transition: width .3s; }
.rowsep { display: flex; gap: 6px; align-items: center; }
.rowsep .grow { flex: 1; }

/* 桌宠（v0.4：横排布局 宠物+食盆，把手在顶部） */
.petwrap { width: 100vw; height: 100vh; display: flex; flex-direction: column; align-items: center; justify-content: center; position: relative; padding-top: 18px; }
.petwrap .pet { cursor: pointer; user-select: none; transition: transform .15s; }
.petwrap .pet:hover { transform: scale(1.05); }
.drag-handle { position: absolute; top: 0; left: 0; right: 0; height: 18px; display: flex; align-items: center; justify-content: center; cursor: grab; background: rgba(255,255,255,.08); border-radius: 10px 10px 0 0; opacity: 0; transition: opacity .15s; }
.petwrap:hover .drag-handle { opacity: 1; }
.drag-handle:active { cursor: grabbing; background: rgba(255,255,255,.18); }
.drag-handle .dots { font-size: 12px; color: rgba(255,255,255,.75); line-height: 1; }
.petrow { display: flex; align-items: flex-end; gap: 6px; }
/* 右键菜单 */
.pet-menu { position: absolute; bottom: 24px; left: 10px; max-height: calc(100vh - 30px); overflow-y: auto; z-index: 50; background: #fffdf9; border: 1px solid rgba(90,60,30,.08); border-radius: 14px; box-shadow: 0 6px 0 rgba(90,60,30,.06), 0 10px 28px rgba(60,40,20,.18); padding: 6px; display: flex; flex-direction: column; min-width: 150px; transform-origin: 18px 100%; animation: menuUp .22s cubic-bezier(.2,.8,.3,1); }
@keyframes menuUp { from { opacity: 0; transform: translateY(4px) scale(.97); } to { opacity: 1; transform: none; } }
@keyframes menuIn { from { opacity: 0; transform: translateY(-4px) scale(.97); } to { opacity: 1; transform: none; } }
.pet-menu button { border: 0; background: none; text-align: left; padding: 8px 12px; font-size: 13px; border-radius: 9px; cursor: pointer; color: #4a3a2a; opacity: 0; animation: menuItemIn .22s ease-out forwards; transition: background-color .12s ease-out; }
.pet-menu button:nth-child(1) { animation-delay: 0ms; } .pet-menu button:nth-child(2) { animation-delay: 30ms; }
.pet-menu button:nth-child(3) { animation-delay: 60ms; } .pet-menu button:nth-child(4) { animation-delay: 90ms; }
.pet-menu button:nth-child(5) { animation-delay: 120ms; }
@keyframes menuItemIn { from { opacity: 0; transform: translateY(-2px); } to { opacity: 1; transform: none; } }
.pet-menu button:hover { background: #fdf0e2; }
.pet-menu button:active { transform: translateY(1px); }
.pet-menu button.danger { color: #b4543f; }
.bubble {
  position: absolute; top: 24px; left: 50%; transform: translateX(-50%);
  background: #fffdf9; color: #4a3a2a; border: 1px solid rgba(90,60,30,.08);
  border-radius: 12px; padding: 8px 14px;
  font-size: 13px; white-space: nowrap; box-shadow: 0 3px 0 rgba(90,60,30,.06), 0 6px 16px rgba(60,40,20,.18);
  max-width: 96vw; overflow: hidden; text-overflow: ellipsis; z-index: 60;
  animation: menuIn .18s ease-out;
}
.bubble::after {
  content: ""; position: absolute; bottom: -6px; left: 50%; transform: translateX(-50%);
  border: 6px solid transparent; border-top-color: #fff; border-bottom: 0;
}
.pet-hint { margin-top: 6px; font-size: 11px; color: #5a4632; background: rgba(255,250,242,.88); border: 1px solid rgba(90,60,30,.08); border-radius: 999px; padding: 2px 10px; box-shadow: 0 2px 6px rgba(60,40,20,.12); }

/* ============ 管理面板 · 设计规范（微信绿 + 白卡片 + 灰阶，对齐 WeChatPubMonitor 复刻版） ============
   色板：BG #F3F4F8 / CARD #FFF / 边框 #E2E8F0 / 主色 #07C160 (#06AD56 hover, #E6F9EF light)
   文字：#0F172A / #334155 / #64748B / #94A3B8   警告 #D97706   危险 #B91C1C */
.panel {
  width: 100vw; height: 100vh; overflow-y: auto;
  background: #F3F4F8; color: #0F172A;
  font-family: system-ui, "Microsoft YaHei UI", "Microsoft YaHei", sans-serif;
  padding: 0 0 24px;
}
/* 顶栏：白底页头 + 工具栏 */
.panel > header {
  display: flex; align-items: center; gap: 12px;
  background: #FFFFFF; border-bottom: 1px solid #E2E8F0;
  padding: 12px 20px; margin-bottom: 14px;
  position: sticky; top: 0; z-index: 10;
}
.panel > header h2 { font-size: 16px; font-weight: 600; color: #0F172A; flex: 1; display: flex; align-items: center; gap: 8px; }
.panel > header h2::before { content: ""; width: 4px; height: 18px; border-radius: 2px; background: #07C160; }
/* 通用按钮（工具栏/卡片内） */
.panel button, .settabs button, .filters button {
  border: 1px solid #E2E8F0; background: #FFFFFF; color: #334155;
  border-radius: 8px; padding: 6px 14px; font-size: 12.5px; cursor: pointer;
  transition: background .12s, border-color .12s, color .12s;
}
.panel button:hover { background: #F9FAFB; border-color: #CBD5E1; }
.panel button:active { transform: translateY(1px); }
.panel button:disabled { opacity: .45; cursor: default; }
/* 主操作按钮（绿色实心） */
.mini.ok { background: #07C160; border-color: #07C160; color: #fff; }
.mini.ok:hover { background: #06AD56; border-color: #06AD56; }
.mini.ok:disabled { opacity: .5; }
.mini { border: 1px solid #E2E8F0; background: #FFFFFF; color: #334155; border-radius: 8px; padding: 6px 12px; font-size: 12px; cursor: pointer; transition: background .12s, border-color .12s; }
.mini:hover { background: #F9FAFB; border-color: #CBD5E1; }
.mini:disabled { opacity: .4; cursor: default; }
.mini.danger { background: #FDD5D5; border-color: #FECACE; color: #B91C1C; }
.mini.danger:hover { background: #FECACE; }
/* 返回/设置等 header 小按钮 */
.panel > header .mini { padding: 6px 14px; }
/* 状态统计行 → 浅灰数据条 */
.statrow {
  display: flex; gap: 8px; align-items: center; flex-wrap: wrap;
  margin: 0 20px 10px; padding: 10px 14px;
  background: #FFFFFF; border: 1px solid #E2E8F0; border-radius: 10px;
  font-size: 12.5px; color: #64748B;
}
.statrow span { padding: 2px 8px; background: #F9FAFB; border-radius: 6px; }
.statrow .mini { margin-left: auto; }
/* 筛选 tabs */
.filters { display: flex; gap: 6px; margin: 0 20px 12px; flex-wrap: wrap; }
.filters button { border-radius: 999px; padding: 5px 14px; }
.filters button.active { background: #07C160; border-color: #07C160; color: #fff; }
/* 内容卡 */
.card {
  background: #FFFFFF; border: 1px solid #E2E8F0; border-radius: 10px;
  padding: 12px 14px; margin: 0 20px 10px;
}
.row1 { display: flex; align-items: center; gap: 8px; }
.title { flex: 1; font-size: 13.5px; color: #0F172A; font-weight: 500; }
.st { font-size: 12px; color: #94A3B8; }
.badge { font-size: 14px; }
.url { display: block; font-size: 12px; color: #07C160; margin: 6px 0; word-break: break-all; text-decoration: none; }
.url:hover { text-decoration: underline; }
.btns { display: flex; gap: 8px; margin-top: 8px; flex-wrap: wrap; }
.empty { text-align: center; color: #94A3B8; padding: 40px 0; font-size: 13px; }
/* 分节标题：左绿条 + 灰说明 */
.bmtitle { font-size: 14px; font-weight: 600; color: #0F172A; margin: 20px 20px 8px; display: flex; align-items: center; gap: 8px; }
.bmtitle::before { content: ""; width: 4px; height: 14px; border-radius: 2px; background: #07C160; flex: none; }
/* 工坊/采集/监听分块卡片 */
.cardblock { background: #FFFFFF; border: 1px solid #E2E8F0; border-radius: 10px; padding: 12px 14px; margin: 0 20px 10px; }
.cardblock .bmtitle, .cardblock h3.bmtitle { margin: 0 0 10px; }
/* 输入框 */
.panel input, .bmform input, .cfgform input {
  padding: 8px 12px; border: 1px solid #E2E8F0; border-radius: 8px;
  font-size: 13px; color: #0F172A; background: #FFFFFF; outline: none;
  transition: border-color .12s, box-shadow .12s;
}
.panel input:focus, .bmform input:focus, .cfgform input:focus {
  border-color: #07C160; box-shadow: 0 0 0 3px rgba(7,193,96,.12);
}
.panel input::placeholder { color: #94A3B8; }
/* 常用网址 */
.bmform { display: flex; gap: 6px; margin: 0 20px 10px; }
.bmform .bm-title { width: 160px; }
.bmform .bm-url { flex: 1; }
.bmgrid { display: flex; flex-wrap: wrap; gap: 8px; margin: 0 20px; }
.bmitem { display: flex; align-items: center; gap: 6px; background: #FFFFFF; border: 1px solid #E2E8F0; border-radius: 999px; padding: 6px 12px; }
.bmitem:hover { border-color: #CBD5E1; }
.bmlink { font-size: 13px; color: #07C160; text-decoration: none; }
.bmlink:hover { text-decoration: underline; }
.bmitem .mini.danger { padding: 2px 8px; border-radius: 999px; }
.empty2 { font-size: 12px; color: #94A3B8; margin: 0 20px; }
/* 设置页 */
.sect { font-size: 14px; font-weight: 600; color: #0F172A; margin: 18px 0 4px; border-top: 1px solid #E2E8F0; padding-top: 14px; }
form > .sect:first-of-type { border-top: 0; margin-top: 0; padding-top: 0; }
.panel.settings { padding: 0 0 24px; }
.panel.settings > header { position: sticky; }
.settabs { display: flex; gap: 6px; margin: 0 20px 14px; flex-wrap: wrap; }
.settabs button { border-radius: 999px; padding: 6px 16px; }
.settabs button.active { background: #07C160; border-color: #07C160; color: #fff; }
.chkline { display: flex; align-items: center; gap: 8px; font-size: 13px; color: #334155; margin-top: 12px; cursor: pointer; }
.notionpreview { list-style: none; padding: 0; margin: 10px 0; font-size: 12px; color: #64748B; background: #FFFFFF; border: 1px solid #E2E8F0; border-radius: 8px; }
.notionpreview li { padding: 6px 10px; border-bottom: 1px solid #F3F4F8; }
.notionpreview li:last-child { border-bottom: 0; }
.tip { font-size: 12px; color: #94A3B8; margin: 12px 0; }
.tip.err { color: #B91C1C; }
.cfgform { max-width: 480px; display: flex; flex-direction: column; margin: 0 20px; }
.cfgform label { font-size: 12.5px; color: #64748B; margin: 10px 0 4px; }
/* 知识卡工坊 */
.cardrow { display: flex; gap: 8px; align-items: center; padding: 6px 8px; border-radius: 8px; cursor: pointer; transition: background .1s; }
.cardrow:hover { background: #F3F4F8; }
.cardrow.picked { background: #E6F9EF; }
.mut { color: #64748B; }
/* 进度条换绿 */
.progwrap { height: 6px; background: #E2E8F0; border-radius: 999px; margin: 10px 0 4px; overflow: hidden; }
.progbar { height: 100%; background: #07C160; border-radius: 999px; transition: width .3s; }
/* 表单主按钮（保存） */
form .ok, .cfgform .ok { background: #07C160; border-color: #07C160; color: #fff; }
form .ok:hover, .cfgform .ok:hover { background: #06AD56; }
/* 获取模型弹层 */
.modellist { max-height: 180px; overflow-y: auto; border: 1px solid #d9dce3; border-radius: 8px; background: #fff; margin-top: 4px; box-shadow: 0 4px 14px rgba(0,0,0,.08); }
.modelitem { padding: 6px 10px; font-size: 12px; cursor: pointer; border-bottom: 1px solid #f0f1f4; }
.modelitem:hover { background: #e8f9ef; color: #07C160; }
/* 厂商卡片 */
.provcard { border: 1px solid #e3e6ec; border-radius: 10px; background: #fff; margin-bottom: 8px; overflow: hidden; }
.provhead { display: flex; align-items: center; gap: 8px; padding: 10px 12px; cursor: pointer; font-weight: 600; }
.provhead:hover { background: #f6f7f9; }
.provname { font-size: 14px; }
.provbadge { font-size: 11px; padding: 2px 8px; border-radius: 999px; background: #fdeaea; color: #c0392b; }
.provbadge.okb { background: #e8f9ef; color: #07C160; }
.provcard > div[v-if], .provcard form, .provcard .provbody { display: block; }
.provcard { min-width: 0; }
.provcard input, .provcard textarea { width: 100%; box-sizing: border-box; display: block; }
.provcard label { display: block; width: 100%; }
/* 关键词标签 */
.kwrow { display: flex; flex-wrap: wrap; gap: 4px; margin: 4px 0; }
.kwtag { font-size: 11px; padding: 1px 8px; border-radius: 999px; background: #eef2f7; color: #64748B; cursor: pointer; }
.kwtag:hover { color: #07C160; }
.kwtag.on { background: #07C160; color: #fff; }
</style>
