<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { readText } from "@tauri-apps/plugin-clipboard-manager";

interface Item { id: string; file: string; title: string; kind: string; url: string; status: string; created_at: string; error: string }
interface AiConfig { base_url: string; api_key: string; model: string; hotkey: string; kb_dir_override: string }
interface Bookmark { title: string; url: string }
interface Stats { total: number; seed: number; fetched: number; "fetch-failed": number; read: number; carded: number; queue: number; notes: number }

const view = ref<"pet" | "panel" | "settings">("pet");
const items = ref<Item[]>([]);
const st = ref<Stats | null>(null);
const cfg = ref<AiConfig>({ base_url: "", api_key: "", model: "", hotkey: "Ctrl+Alt+C", kb_dir_override: "" });
const bookmarks = ref<Bookmark[]>([]);
const bmNew = ref({ title: "", url: "" });
const kbNew = ref("");
const bubble = ref("");
const bubbleShow = ref(false);
const busy = ref<Record<string, boolean>>({});
const filter = ref<"all" | "seed" | "fetched" | "read" | "carded">("all");
let bubbleTimer: number | undefined;
let pollTimer: number | undefined;

const shown = computed(() =>
  filter.value === "all" ? items.value : items.value.filter(i => i.status === filter.value)
);

async function refresh() {
  items.value = await invoke<Item[]>("list_items");
  st.value = await invoke<Stats>("stats");
}

// 桌宠单击：收剪贴板
async function petClick() {
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

function showBubble(msg: string) {
  bubble.value = msg;
  bubbleShow.value = true;
  clearTimeout(bubbleTimer);
  bubbleTimer = window.setTimeout(() => (bubbleShow.value = false), 2200);
}

async function crawlAll() {
  showBubble("🕷️ 开始抓取网页…");
  for (let i = 0; i < 20; i++) {
    try {
      const msg = await invoke<string>("process_queue");
      showBubble("✓ " + msg);
    } catch (e) {
      if (String(e).includes("队列空")) break;
      showBubble("⚠️ " + String(e));
    }
  }
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

async function moveKb() {
  if (!kbNew.value.trim()) { showBubble("⚠️ 先填新目录路径"); return; }
  showBubble("🚚 迁移中…");
  try { showBubble("✓ " + await invoke<string>("move_kb_dir", { newDir: kbNew.value })); }
  catch (e) { showBubble("⚠️ " + String(e)); }
  refresh();
}

async function saveConfig() {
  await invoke("set_config", {
    baseUrl: cfg.value.base_url,
    apiKey: cfg.value.api_key,
    model: cfg.value.model,
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
  const winLabel = await (async () => {
    try { const { getCurrentWindow } = await import("@tauri-apps/api/window"); return getCurrentWindow().label; }
    catch { return "main"; }
  })();
  const un = await listen<string>("switch-view", (e) => {
    if (winLabel !== "main") return; // 只有面板窗响应切换，桌宠视图永不被动改
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
    view.value = "pet";
    const pet = await Window.getByLabel("pet");
    if (pet) { await pet.show(); await pet.setFocus(); }
    if (cur.label !== "pet") {
      await cur.hide();          // 面板隐藏（托盘/双击桌宠可再唤起）
    }
  } catch { view.value = "pet"; }
}

// pet 窗口切到面板时，显示 main 窗口、隐藏自己
async function openPanel() {
  try {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    const cur = getCurrentWindow();
    if (cur.label === "pet") {
      const { Window } = await import("@tauri-apps/api/window");
      const main = await Window.getByLabel("main");
      if (main) { await main.show(); await main.setFocus();
        // main 窗之前被「回到桌宠」切到 pet 视图，重新拉起时让它回到面板
        const { emitTo } = await import("@tauri-apps/api/event");
        await emitTo("main", "switch-view", "panel").catch(() => {});
      }
      await cur.hide();
      return;
    }
    view.value = "panel";
  } catch { view.value = "panel"; }
}

// main 窗口关闭时隐藏而非退出（托盘常驻）
// 用 Rust 侧 on_window_event 处理，前端只管 UI
onUnmounted(() => { clearInterval(pollTimer); clearTimeout(bubbleTimer); });
</script>

<template>
  <!-- 桌宠视图 -->
  <div v-if="view === 'pet'" class="petwrap">
    <div v-if="bubbleShow" class="bubble">{{ bubble }}</div>
    <div class="pet" @click="petClick" @dblclick="openPanel" title="单击收藏剪贴板 / 双击打开面板">
      <div class="pet-face">🐱</div>
      <div class="pet-body"></div>
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
    <form @submit.prevent="saveConfig" class="cfgform">
      <h3 class="sect">🤖 API 模型</h3>
      <label>API 地址（base_url）</label>
      <input v-model="cfg.base_url" placeholder="https://api.example.com/v1" />
      <label>API Key</label>
      <input v-model="cfg.api_key" type="password" placeholder="sk-..." />
      <label>模型名</label>
      <input v-model="cfg.model" placeholder="gpt-4o-mini / deepseek-chat / agnes-chat …" />
      <p class="tip">填 OpenAI 兼容接口即可（chat/completions）。AI 整理知识卡会走这里。</p>
      <div class="btns">
        <button type="submit" class="ok">💾 保存</button>
        <button type="button" class="mini" @click="testConfig">🧪 测试连接（用最新一条做真实整理）</button>
      </div>

      <h3 class="sect">⌨️ 主动抓取热键</h3>
      <label>全局热键（任意窗口按下即收剪贴板）</label>
      <input v-model="cfg.hotkey" placeholder="Ctrl+Alt+C" />
      <p class="tip">格式：Ctrl/Shift/Alt/Super 组合 + 字母/数字，如 Ctrl+Alt+C、Ctrl+Shift+K。改完点保存生效（下次启动也生效）。</p>

      <h3 class="sect">📁 知识库目录</h3>
      <label>当前目录</label>
      <input :value="cfg.kb_dir_override || '默认（用户目录/knowledge-pet）'" disabled />
      <label>迁移到新目录</label>
      <input v-model="kbNew" placeholder="D:\知识库\knowledge-pet" />
      <div class="btns">
        <button type="button" class="mini" @click="moveKb">🚚 迁移（自动搬 inbox/知识卡/常用网址）</button>
      </div>
      <p class="tip">首次启动会自动在用户目录创建 knowledge-pet（含 10-inbox/20-sources/30-notes/40-sop/90-archive）。迁移会把内容整体搬到新目录。</p>
    </form>
  </div>
</template>

<style>
* { box-sizing: border-box; margin: 0; }
html, body { background: transparent !important; }
body { font-family: system-ui, "Microsoft YaHei"; overflow: hidden; }

/* 桌宠 */
.petwrap { width: 100vw; height: 100vh; display: flex; flex-direction: column; align-items: center; justify-content: center; }
.pet { cursor: pointer; text-align: center; user-select: none; transition: transform .15s; }
.pet:hover { transform: scale(1.1); }
.pet:active { transform: scale(0.92); }
.pet-face { font-size: 52px; filter: drop-shadow(0 2px 4px rgba(0,0,0,.35)); }
.pet-body { width: 46px; height: 8px; background: rgba(0,0,0,.18); border-radius: 50%; margin: 2px auto 0; }
.bubble {
  position: absolute; top: -6px; left: 50%; transform: translateX(-50%);
  background: #fff; color: #333; border-radius: 12px; padding: 8px 14px;
  font-size: 13px; white-space: nowrap; box-shadow: 0 3px 10px rgba(0,0,0,.25);
}
.bubble::after {
  content: ""; position: absolute; bottom: -6px; left: 50%; transform: translateX(-50%);
  border: 6px solid transparent; border-top-color: #fff; border-bottom: 0;
}
.pet-hint { margin-top: 6px; font-size: 11px; color: #fff; background: rgba(0,0,0,.4); border-radius: 999px; padding: 2px 10px; }

/* 面板 */
.panel { width: 100vw; height: 100vh; background: #f7f5f1; color: #1a1a1a; padding: 16px; overflow-y: auto; }
header { display: flex; align-items: center; gap: 10px; margin-bottom: 12px; }
h2 { font-size: 18px; flex: 1; }
.statrow { display: flex; gap: 14px; align-items: center; font-size: 13px; color: #666; margin-bottom: 10px; flex-wrap: wrap; }
.filters { display: flex; gap: 6px; margin-bottom: 12px; flex-wrap: wrap; }
.filters button { border: 1px solid #ddd; background: #fff; border-radius: 999px; padding: 4px 12px; cursor: pointer; font-size: 12px; }
.filters button.active { background: #f97316; color: #fff; border-color: #f97316; }
.card { background: #fff; border: 1px solid #e5e0d8; border-radius: 12px; padding: 12px 14px; margin-bottom: 10px; }
.row1 { display: flex; align-items: center; gap: 8px; }
.title { flex: 1; font-size: 14px; }
.st { font-size: 12px; color: #888; }
.badge { font-size: 14px; }
.url { display: block; font-size: 12px; color: #2563eb; margin: 6px 0; word-break: break-all; }
.btns { display: flex; gap: 8px; margin-top: 8px; }
.mini { border: 0; background: #eee; color: #444; border-radius: 8px; padding: 6px 12px; font-size: 12px; cursor: pointer; }
.mini.ok { background: #f97316; color: #fff; }
.mini:disabled { opacity: .4; cursor: default; }
.empty { text-align: center; color: #999; padding: 40px 0; font-size: 13px; }

/* 删除按钮 */
.mini.danger { background: #fee2e2; color: #b91c1c; }
/* 常用网址 */
.bmtitle { font-size: 15px; margin: 18px 0 8px; }
.bmform { display: flex; gap: 6px; margin-bottom: 10px; }
.bmform .bm-title { width: 160px; }
.bmform .bm-url { flex: 1; }
.bmform input { padding: 8px 10px; border: 1px solid #ddd; border-radius: 8px; font-size: 13px; }
.bmgrid { display: flex; flex-wrap: wrap; gap: 8px; }
.bmitem { display: flex; align-items: center; gap: 6px; background: #fff; border: 1px solid #e5e0d8; border-radius: 999px; padding: 6px 12px; }
.bmlink { font-size: 13px; color: #2563eb; text-decoration: none; }
.bmlink:hover { text-decoration: underline; }
.bmitem .mini.danger { padding: 2px 8px; border-radius: 999px; }
.empty2 { font-size: 12px; color: #999; }
/* 设置分节 */
.sect { font-size: 15px; margin: 18px 0 4px; border-top: 1px dashed #e5e0d8; padding-top: 14px; }
form > .sect:first-of-type { border-top: 0; margin-top: 0; padding-top: 0; }

/* 设置 */
.cfgform { max-width: 480px; display: flex; flex-direction: column; }
.cfgform label { font-size: 13px; color: #555; margin: 10px 0 4px; }
.cfgform input { padding: 9px 12px; border: 1px solid #ddd; border-radius: 8px; font-size: 13px; }
.tip { font-size: 12px; color: #999; margin: 12px 0; }
</style>
