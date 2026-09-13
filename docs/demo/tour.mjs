// tour.mjs — the agent-monitor demo tour (#208), recorded from the app shell over CDP.
// Everything it shows comes from the hermetic demo store (make_store.py): no real session, no
// real path, no real prompt. Captions are injected as a page overlay so the video explains itself.
import { launch, Cdp, attach, evalIn, until, sleep } from "./cdp.mjs";
import { mkdirSync, rmSync, writeFileSync } from "node:fs";

const PORT = process.env.PORT || "2790";
const OUT = new URL("./frames/", import.meta.url);
const W = 1440, H = 900;
const BIG = "aaaaaaaa-0000-4000-8000-000000000009";   // the 200 MB session
const BLOCKED = "aaaaaaaa-0000-4000-8000-000000000001"; // payments, waiting on an answer

rmSync(OUT, { recursive: true, force: true });
mkdirSync(OUT, { recursive: true });

const { child, wsUrl } = await launch({ width: W, height: H });
const cdp = await Cdp.connect(wsUrl);
const { send } = await attach(cdp);
await send("Page.enable");
await send("Runtime.enable");
await send("Emulation.setDeviceMetricsOverride", { width: W, height: H, deviceScaleFactor: 2, mobile: false });

// ── the caption overlay ────────────────────────────────────────────────────────────────────
const CAPTION_CSS = `
#demoCap{position:fixed;left:0;right:0;bottom:0;z-index:2147483647;display:flex;justify-content:center;pointer-events:none;font-family:"PingFang SC","Hiragino Sans GB","Noto Sans CJK SC",system-ui,sans-serif}
#demoCap .cap{margin:0 0 34px;padding:13px 24px;border-radius:13px;background:rgba(17,20,28,.88);color:#fff;font-size:21px;line-height:1.5;letter-spacing:.02em;box-shadow:0 12px 40px rgba(10,14,22,.28);opacity:0;transform:translateY(8px);transition:opacity .28s ease,transform .28s ease;max-width:1100px;text-align:center}
#demoCap .cap.on{opacity:1;transform:none}
#demoCap .cap b{color:#8fb8ff;font-weight:650}
#demoRing{position:fixed;z-index:2147483646;border:3px solid #2f6ee8;border-radius:12px;box-shadow:0 0 0 4px rgba(47,110,232,.18);pointer-events:none;opacity:0;transition:opacity .2s ease,left .3s ease,top .3s ease,width .3s ease,height .3s ease}
#demoRing.on{opacity:1}`;
const setup = `(function(){
  var s = document.createElement('style'); s.textContent = ${JSON.stringify(CAPTION_CSS)}; document.head.appendChild(s);
  var c = document.createElement('div'); c.id = 'demoCap'; c.innerHTML = '<div class="cap"></div>'; document.body.appendChild(c);
  var r = document.createElement('div'); r.id = 'demoRing'; document.body.appendChild(r);
  window.__cap = function (html) { var el = document.querySelector('#demoCap .cap'); if (!html) { el.classList.remove('on'); return; } el.innerHTML = html; el.classList.add('on'); };
  window.__ring = function (sel) { var r = document.getElementById('demoRing'); if (!sel) { r.classList.remove('on'); return; } var e = document.querySelector(sel); if (!e) { r.classList.remove('on'); return; } var b = e.getBoundingClientRect(); r.style.left = (b.left - 6) + 'px'; r.style.top = (b.top - 6) + 'px'; r.style.width = (b.width + 12) + 'px'; r.style.height = (b.height + 12) + 'px'; r.classList.add('on'); };
  return 'ok';
})()`;
const cap = async (html, hold = 0) => { await evalIn(send, `window.__cap(${JSON.stringify(html)})`); if (hold) await sleep(hold); };
const ring = sel => evalIn(send, `window.__ring(${sel === null ? "null" : JSON.stringify(sel)})`);
const click = async sel => {
  const ok = await evalIn(send, `(function(){var e=document.querySelector(${JSON.stringify(sel)}); if(!e) return 'missing'; e.click(); return 'ok';})()`);
  if (ok !== "ok") throw new Error(`click ${sel}: ${ok}`);
};

// ── the recording ─────────────────────────────────────────────────────────────────────────
const frames = [];
cdp.on("Page.screencastFrame", async p => {
  frames.push({ t: p.metadata.timestamp, data: p.data });
  try { await send("Page.screencastFrameAck", { sessionId: p.sessionId }); } catch {}
});
const measurements = {};

await send("Page.navigate", { url: `http://127.0.0.1:${PORT}/?ui=app&session=${BLOCKED}` });
await until(send, "document.querySelectorAll('.tree-row.session[data-session]').length >= 3", "the session list");
await evalIn(send, setup);
await sleep(400);
await send("Page.startScreencast", { format: "jpeg", quality: 88, maxWidth: W * 2, maxHeight: H * 2, everyNthFrame: 1 });

// 1. One place for every local agent.
await cap("<b>一个页面</b>，本机所有 agent 的会话都在这里", 200);
await sleep(2600);
await ring(".tree");
await sleep(2200);
await ring(null);

// 2. Triage: who is waiting on you.
await cap("会话自己报出状态：<b>谁在等你回话</b>，谁还在跑");
await ring(`[data-session="${BLOCKED}"]`);
await sleep(2600);
await ring(null);
await cap("过滤器把列表分成<b>最近活跃 / 被阻塞 / 空闲</b>");
await click("#filterBtn");
await sleep(900);
await ring("#sessionFilter");
await sleep(2600);
await click('#sessionFilter [data-bucket="idle"]');
await sleep(700);
await cap("勾上<b>空闲</b>，其余会话回到列表");
await sleep(2000);
await ring(null);
await evalIn(send, "document.body.click()");
await sleep(400);

// 3. Drill down: the blocked session's failing test.
await cap("点进去：<b>它为什么停下来</b>，一眼看到");
await sleep(400);
await evalIn(send, "(function(){var h=[...document.querySelectorAll('.process-head,.fold-head,[data-fold]')].pop(); if(h) h.scrollIntoView({block:'center'}); return 'ok';})()");
await sleep(1500);
await evalIn(send, "(function(){var t=document.querySelector('.transcript'); if(t) t.scrollTop = t.scrollHeight; return 'ok';})()");
await sleep(1800);
await cap("工具调用、输出、失败的那条测试，都是可以展开的记录");
await sleep(2600);

// 4. Search inside the session.
await cap("<b>会话内搜索</b>：直接跳到命中的那一处");
await evalIn(send, "(function(){var i=document.querySelector('#sessionSearch,.header-searchbox input'); if(i){i.focus();} return 'ok';})()");
await ring("#sessionSearch, .header-searchbox");
for (const ch of "backoff") {
  await evalIn(send, `(function(){var i=document.querySelector('#sessionSearch,.header-searchbox input'); if(i){i.value+=${JSON.stringify(ch)}; i.dispatchEvent(new Event('input',{bubbles:true}));} return 'ok';})()`);
  await sleep(150);
}
await sleep(2200);
await ring(null);
await evalIn(send, "(function(){var i=document.querySelector('#sessionSearch,.header-searchbox input'); if(i){i.value=''; i.dispatchEvent(new Event('input',{bubbles:true})); i.blur();} return 'ok';})()");

// 5. Performance: the 200 MB transcript.
const bigMb = Number(process.env.BIG_MB || 200);
await cap(`换一个 <b>${bigMb} MB</b> 的会话记录——${await evalIn(send, "'几千个回合'")}`);
const t0 = Date.now();
await send("Page.navigate", { url: `http://127.0.0.1:${PORT}/?ui=app&session=${BIG}` });
await until(send, "document.querySelectorAll('[data-record]').length > 0 || document.querySelectorAll('.turn').length > 0", "the big transcript to paint", 60000, "document.body.innerText.slice(0,160)");
measurements.big_open_ms = Date.now() - t0;
await evalIn(send, setup);
await sleep(300);
await cap(`${bigMb} MB 的记录，<b>${(measurements.big_open_ms / 1000).toFixed(1)} 秒</b>打开；滚动时只渲染看得见的那一段`);
await sleep(1800);
for (let i = 0; i < 14; i++) {
  await evalIn(send, "(function(){var t=document.querySelector('.transcript')||document.scrollingElement; t.dispatchEvent(new WheelEvent('wheel',{deltaY:900,bubbles:true})); t.scrollTop += 900; return 'ok';})()");
  await sleep(110);
}
await sleep(1500);
await cap("跳到末尾也是一样快");
await evalIn(send, "(function(){var t=document.querySelector('.transcript')||document.scrollingElement; t.scrollTop = t.scrollHeight; return 'ok';})()");
await sleep(2200);
await cap("");
await sleep(600);

await send("Page.stopScreencast");
await sleep(300);

// ── frames → files ────────────────────────────────────────────────────────────────────────
let n = 0;
const list = [];
for (let i = 0; i < frames.length; i++) {
  const name = `f${String(n++).padStart(5, "0")}.jpg`;
  writeFileSync(new URL(name, OUT), Buffer.from(frames[i].data, "base64"));
  const dur = i + 1 < frames.length ? Math.max(0.016, frames[i + 1].t - frames[i].t) : 0.4;
  list.push(`file '${name}'`, `duration ${dur.toFixed(3)}`);
}
if (frames.length) list.push(`file 'f${String(n - 1).padStart(5, "0")}.jpg'`);
writeFileSync(new URL("frames.txt", OUT), list.join("\n") + "\n");
measurements.frames = frames.length;
measurements.seconds = frames.length ? +(frames[frames.length - 1].t - frames[0].t).toFixed(1) : 0;
writeFileSync(new URL("./measurements.json", import.meta.url), JSON.stringify(measurements, null, 1));
console.log(JSON.stringify(measurements));
cdp.close();
child.kill();
