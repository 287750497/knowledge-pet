<script setup lang="ts">
// 多状态 SVG 桌宠：idle / happy / eat / sleep
// 参考 BongoCat 的状态机思路，但用内联 SVG（Windows 渲染稳定、无白底、零资源依赖）
import { ref, computed, onMounted, onUnmounted } from "vue";

type Mood = "idle" | "happy" | "eat" | "sleep";
const mood = ref<Mood>("idle");
let idleTimer: number | undefined;
let moodTimer: number | undefined;

// 5 分钟无操作 → 睡觉；任何交互唤醒
function armSleep() {
  clearTimeout(idleTimer);
  idleTimer = window.setTimeout(() => { mood.value = "sleep"; }, 5 * 60 * 1000);
}
function poke(m: Mood, ms = 1200) {
  mood.value = m;
  clearTimeout(moodTimer);
  moodTimer = window.setTimeout(() => { mood.value = "idle"; }, ms);
  armSleep();
}
function wake() { if (mood.value === "sleep") mood.value = "idle"; armSleep(); }

onMounted(() => {
  armSleep();
  window.addEventListener("pointerdown", wake);
});
onUnmounted(() => { clearTimeout(idleTimer); clearTimeout(moodTimer); });

defineExpose({ poke });

// 眼睛：happy/eat 闭眼弧线，idle 圆眼，sleep 缝眼
const eyes = computed(() => mood.value === "happy" || mood.value === "eat" ? "closed" : mood.value === "sleep" ? "sleep" : "open");
const mouth = computed(() => mood.value === "happy" ? "smile" : mood.value === "eat" ? "open" : mood.value === "sleep" ? "zzz" : "w");
// 尾巴摆动参数：sleep 时停
const tailDur = computed(() => mood.value === "sleep" ? "0s" : "1.6s");
</script>

<template>
  <svg class="pet-svg" viewBox="0 0 120 110" width="110" height="100" aria-hidden="true">
    <defs>
      <radialGradient id="bodyG" cx="40%" cy="30%" r="80%">
        <stop offset="0%" stop-color="#ffe9c9"/>
        <stop offset="100%" stop-color="#f4b96a"/>
      </radialGradient>
      <linearGradient id="earG" x1="0" y1="0" x2="0" y2="1">
        <stop offset="0%" stop-color="#ffd9a0"/>
        <stop offset="100%" stop-color="#e59a45"/>
      </linearGradient>
    </defs>

    <!-- 尾巴（摆动动画） -->
    <path class="tail" d="M96 82 Q116 76 112 58 Q110 50 104 54"
      fill="none" stroke="#e59a45" stroke-width="7" stroke-linecap="round"
      :style="{ animation: tailDur ? `tailswing ${tailDur} ease-in-out infinite alternate` : 'none' }"/>

    <!-- 身体 -->
    <ellipse cx="56" cy="82" rx="34" ry="22" fill="url(#bodyG)"/>
    <!-- 前爪 -->
    <ellipse cx="42" cy="98" rx="10" ry="5" fill="#ffdca6"/>
    <ellipse cx="68" cy="98" rx="10" ry="5" fill="#ffdca6"/>

    <!-- 头 -->
    <g>
      <!-- 耳朵 -->
      <path d="M28 40 L34 16 L48 32 Z" fill="url(#earG)"/>
      <path d="M84 40 L78 16 L64 32 Z" fill="url(#earG)"/>
      <path d="M33 34 L36 24 L43 32 Z" fill="#ff9d8a"/>
      <path d="M79 34 L76 24 L69 32 Z" fill="#ff9d8a"/>
      <!-- 脸 -->
      <circle cx="56" cy="46" r="30" fill="url(#bodyG)"/>

      <!-- 眼睛 -->
      <template v-if="eyes === 'open'">
        <circle cx="45" cy="42" r="4.5" fill="#33261a"/>
        <circle cx="67" cy="42" r="4.5" fill="#33261a"/>
        <circle cx="46.6" cy="40.4" r="1.4" fill="#fff"/>
        <circle cx="68.6" cy="40.4" r="1.4" fill="#fff"/>
      </template>
      <template v-else-if="eyes === 'closed'">
        <path d="M40 42 Q45 47 50 42" stroke="#33261a" stroke-width="2.4" fill="none" stroke-linecap="round"/>
        <path d="M62 42 Q67 47 72 42" stroke="#33261a" stroke-width="2.4" fill="none" stroke-linecap="round"/>
      </template>
      <template v-else>
        <path d="M40 43 H50" stroke="#33261a" stroke-width="2.2" stroke-linecap="round"/>
        <path d="M62 43 H72" stroke="#33261a" stroke-width="2.2" stroke-linecap="round"/>
        <!-- zZ -->
        <text x="88" y="30" font-size="11" fill="#9a7434" font-weight="bold" class="zzz1">z</text>
        <text x="98" y="20" font-size="9" fill="#b8945c" font-weight="bold" class="zzz2">z</text>
      </template>

      <!-- 鼻+胡须 -->
      <path d="M53 50 L59 50 L56 54 Z" fill="#c96f4a"/>
      <g stroke="#c99a68" stroke-width="1.4" stroke-linecap="round">
        <path d="M36 50 L46 51"/><path d="M36 55 L46 54"/>
        <path d="M76 50 L66 51"/><path d="M76 55 L66 54"/>
      </g>

      <!-- 嘴 -->
      <template v-if="mouth === 'smile'">
        <path d="M50 58 Q56 64 62 58" stroke="#8a5a30" stroke-width="2.4" fill="#ffb08a" stroke-linecap="round"/>
      </template>
      <template v-else-if="mouth === 'open'">
        <ellipse cx="56" cy="59" rx="6" ry="5" fill="#8a4a30"/>
        <path d="M51 57 Q56 62 61 57" stroke="#ffb08a" stroke-width="1.5" fill="none"/>
      </template>
      <template v-else-if="mouth === 'w'">
        <path d="M51 58 Q53.5 60.5 56 58 Q58.5 60.5 61 58" stroke="#8a5a30" stroke-width="2" fill="none" stroke-linecap="round"/>
      </template>
      <!-- sleep 无嘴 -->
    </g>
  </svg>
</template>

<style>
.pet-svg { display: block; filter: drop-shadow(0 4px 6px rgba(0,0,0,.22)); }
.pet-svg .tail { transform-origin: 96px 82px; }
@keyframes tailswing { from { transform: rotate(-6deg); } to { transform: rotate(10deg); } }
.pet-svg .zzz1 { animation: zfloat 2.2s ease-in-out infinite alternate; }
.pet-svg .zzz2 { animation: zfloat 2.2s .6s ease-in-out infinite alternate; }
@keyframes zfloat { from { opacity: .3; transform: translateY(0); } to { opacity: 1; transform: translateY(-3px); } }
/* 整体呼吸感 */
.petwrap .pet-svg { animation: petbreathe 3s ease-in-out infinite; }
.petwrap.mood-sleep .pet-svg { animation: none; }
@keyframes petbreathe { 0%,100% { transform: scale(1); } 50% { transform: scale(1.03); } }
</style>
