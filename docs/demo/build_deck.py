#!/usr/bin/env python3
"""build_deck.py — the agent-monitor elevator pitch (#208) as an HTML SLIDE DECK, in Chinese.

One slide per screen: arrow keys / space / PageDown advance, scrolling snaps, and `?print`
(or printing) lays every slide out in one column for a PDF. Two builds from one source:
`--inline-video` embeds the demo as a data URI (for publishing a page that stands alone),
otherwise the deck points at `demo/agent-monitor-demo.mp4` beside it (the repo copy).
`--bare` omits the document shell, for a host that wraps the page itself.

Every figure in the deck was measured in the run recorded by tour.mjs against the hermetic store
that make_store.py writes; nothing here comes from a real session.
"""
import base64, os, sys

HERE = os.path.dirname(os.path.abspath(__file__))
args = sys.argv[1:]
out = args[0]
inline = "--inline-video" in args
bare = "--bare" in args

def data_uri(name, mime):
    with open(os.path.join(HERE, name), "rb") as fh:
        return f"data:{mime};base64," + base64.b64encode(fh.read()).decode()

SHOT_LIST = data_uri("still-4.jpg", "image/jpeg")
SHOT_FILTER = data_uri("still-12.jpg", "image/jpeg")
SHOT_DRILL = data_uri("still-20.jpg", "image/jpeg")
SHOT_BIG = data_uri("still-35.jpg", "image/jpeg")
VIDEO = data_uri("agent-monitor-demo.mp4", "video/mp4") if inline else "demo/agent-monitor-demo.mp4"

CSS = """
:root{
  --ink:#0e1420; --sub:#4a5568; --faint:#8892a6; --line:#dfe5f0;
  --paper:#f5f7fb; --card:#fff; --primary:#2f6ee8; --primary-soft:#e9f0fe;
  --amber:#c8781c; --green:#2e7d55;
  --mono:ui-monospace,SFMono-Regular,"SF Mono",Menlo,monospace;
  --han:"PingFang SC","Hiragino Sans GB","Source Han Sans SC","Noto Sans CJK SC","Microsoft YaHei",sans-serif;
  --pad:clamp(28px,5vw,86px);
}
*{box-sizing:border-box}
html,body{margin:0;padding:0}
body{background:var(--paper);color:var(--ink);font-family:var(--han);-webkit-font-smoothing:antialiased}
.deck{height:100dvh;overflow-y:auto;scroll-snap-type:y mandatory;scroll-behavior:smooth}
.slide{min-height:100dvh;scroll-snap-align:start;scroll-snap-stop:always;display:flex;flex-direction:column;justify-content:center;padding:clamp(40px,7vh,90px) var(--pad) clamp(56px,9vh,104px);position:relative}
.slide+.slide{border-top:1px solid var(--line)}
.eyebrow{font:600 12px/1.6 var(--mono);letter-spacing:.18em;text-transform:uppercase;color:var(--faint);margin:0 0 clamp(10px,1.6vh,18px)}
h1{font-size:clamp(34px,5.4vw,68px);line-height:1.15;letter-spacing:-.02em;margin:0 0 clamp(14px,2.4vh,26px);font-weight:700;text-wrap:balance}
h2{font-size:clamp(26px,3.4vw,44px);line-height:1.25;letter-spacing:-.015em;margin:0 0 clamp(12px,2vh,22px);font-weight:700;text-wrap:balance;max-width:20em}
.lede{font-size:clamp(17px,1.55vw,22px);line-height:1.7;color:var(--sub);max-width:34em;margin:0 0 clamp(16px,2.6vh,30px)}
.cols{display:grid;grid-template-columns:minmax(0,1fr) minmax(0,1.06fr);gap:clamp(22px,3.4vw,56px);align-items:center}
ul.points{list-style:none;margin:0;padding:0;max-width:30em}
ul.points li{position:relative;padding:0 0 0 25px;margin:0 0 clamp(9px,1.5vh,15px);font-size:clamp(15.5px,1.35vw,19px);line-height:1.72;color:var(--sub)}
ul.points li:before{content:"";position:absolute;left:2px;top:.74em;width:7px;height:7px;border-radius:2px;background:var(--primary)}
ul.points li b{color:var(--ink);font-weight:650}
figure{margin:0}
figure img,figure video{width:100%;display:block;border:1px solid var(--line);border-radius:14px;box-shadow:0 22px 56px rgba(18,28,52,.13);background:#fff}
figcaption{margin-top:10px;font-size:clamp(12px,1vw,14px);line-height:1.6;color:var(--faint)}
.tiles{display:grid;grid-template-columns:repeat(auto-fit,minmax(min(100%,230px),1fr));gap:clamp(10px,1.2vw,16px);max-width:1180px}
.tile{background:var(--card);border:1px solid var(--line);border-radius:14px;padding:clamp(14px,1.5vw,20px)}
.tile h3{margin:0 0 6px;font-size:clamp(15px,1.25vw,17.5px);font-weight:650}
.tile p{margin:0;font-size:clamp(13px,1.08vw,15px);line-height:1.68;color:var(--sub)}
.states{display:flex;flex-wrap:wrap;gap:9px;margin:0 0 clamp(14px,2vh,22px)}
.state{display:inline-flex;align-items:center;gap:8px;padding:7px 15px;border-radius:999px;border:1px solid var(--line);background:var(--card);font-size:clamp(13px,1.1vw,15.5px)}
.state i{width:8px;height:8px;border-radius:50%;display:block}
.state.a i{background:var(--green)}.state.b i{background:var(--amber)}.state.c i{background:var(--faint)}
.state em{font-style:normal;color:var(--faint);font-size:.86em}
.facts{display:flex;flex-wrap:wrap;gap:clamp(14px,2.4vw,44px);padding-top:clamp(16px,2.4vh,26px);border-top:1px solid var(--line);max-width:1100px}
.fact b{display:block;font:650 clamp(22px,2.4vw,32px)/1.2 var(--mono);font-variant-numeric:tabular-nums;letter-spacing:-.02em}
.fact span{font-size:clamp(12px,1vw,14px);color:var(--faint)}
.num{border-collapse:collapse;font-size:clamp(14px,1.2vw,17px);max-width:30em}
.num td{padding:9px 26px 9px 0;border-bottom:1px solid var(--line);color:var(--sub)}
.num td:last-child{font:650 1em/1.4 var(--mono);font-variant-numeric:tabular-nums;text-align:right;padding-right:0;color:var(--ink)}
code{font-family:var(--mono);font-size:.88em;background:var(--primary-soft);color:#1b4ba8;padding:2px 6px;border-radius:5px}
pre{background:#0e1522;color:#e7edf8;border-radius:14px;padding:clamp(16px,1.8vw,24px);overflow-x:auto;font-family:var(--mono);font-size:clamp(12.5px,1.05vw,15px);line-height:1.85;margin:0;max-width:44em}
pre b{color:#8fb8ff;font-weight:600}
.note{font-size:clamp(12.5px,1.02vw,14.5px);color:var(--faint);border-left:2px solid var(--line);padding-left:14px;margin:clamp(14px,2vh,22px) 0 0;max-width:40em;line-height:1.7}
.mark{display:inline-flex;align-items:center;gap:11px;margin-bottom:clamp(18px,3vh,34px)}
.mark svg{width:clamp(28px,2.6vw,36px);height:clamp(28px,2.6vw,36px)}
.mark span{font:650 clamp(16px,1.5vw,20px)/1 var(--mono);letter-spacing:-.01em}
.pager{position:fixed;right:clamp(14px,2vw,28px);bottom:clamp(12px,2vh,22px);z-index:9;display:flex;align-items:center;gap:12px;font:600 12px/1 var(--mono);color:var(--faint);background:color-mix(in srgb,var(--paper) 86%,transparent);backdrop-filter:blur(6px);padding:8px 12px;border-radius:999px;border:1px solid var(--line)}
.pager button{border:0;background:transparent;color:var(--faint);font:inherit;cursor:pointer;padding:2px 4px;border-radius:6px}
.pager button:hover{color:var(--primary);background:var(--primary-soft)}
.rail{position:fixed;left:0;right:0;top:0;height:2px;z-index:9;background:transparent}
.rail i{display:block;height:100%;background:var(--primary);width:0;transition:width .24s ease}
@media (max-width:900px){.cols{grid-template-columns:1fr;gap:20px}.slide{justify-content:flex-start;padding-top:clamp(34px,6vh,60px)}figure img,figure video{box-shadow:0 12px 30px rgba(18,28,52,.12)}}
@media (prefers-reduced-motion:reduce){.deck{scroll-behavior:auto}.rail i{transition:none}}
@media (prefers-color-scheme:dark){
  :root:not([data-theme="light"]){--ink:#e9eef8;--sub:#a7b2c7;--faint:#79849a;--line:#222a39;--paper:#0b1019;--card:#121926;--primary:#6f9dfb;--primary-soft:#16233c}
  :root:not([data-theme="light"]) figure img,:root:not([data-theme="light"]) figure video{box-shadow:0 22px 56px rgba(0,0,0,.55)}
  :root:not([data-theme="light"]) code{color:#a9c7ff}
}
body.print .deck{height:auto;overflow:visible;scroll-snap-type:none}
body.print .slide{min-height:auto;page-break-after:always;padding-bottom:48px}
body.print .pager,body.print .rail{display:none}
@media print{
  .deck{height:auto;overflow:visible;scroll-snap-type:none}
  .slide{min-height:auto;page-break-after:always;border-top:0}
  .pager,.rail{display:none}
  body{background:#fff}
}
"""

SLIDES = [
  """<section class="slide" id="s1">
  <div class="mark">
    <svg viewBox="0 0 24 24" fill="none" stroke="#2e7d55" stroke-width="1.7" stroke-linecap="round"><rect x="3" y="4" width="18" height="13" rx="2"/><path d="M8 21h8M12 17v4M7 12l2.5-3 2 4 2-5 1.5 4"/></svg>
    <span>agent-monitor</span>
  </div>
  <h1>本机所有 AI 编码会话，<br>放在同一个页面里看</h1>
  <p class="lede">你同时开着几个 agent、几十个会话。它把它们汇到一处：谁在等你、谁还在跑、上次那段输出在哪——只观察，不插手。</p>
  <div class="facts">
    <div class="fact"><b>3 种</b><span>agent 同列</span></div>
    <div class="fact"><b>0 配置</b><span>读各家自己的会话目录</span></div>
    <div class="fact"><b>200 MB</b><span>会话 5.1 秒打开</span></div>
    <div class="fact"><b>127.0.0.1</b><span>只在回环，不上传</span></div>
  </div>
</section>""",
  """<section class="slide" id="s2">
  <p class="eyebrow">02 · 问题</p>
  <h2>会话越多，越没人知道现在该看哪一个</h2>
  <ul class="points" style="max-width:36em">
    <li>会话散落在各家 agent 自己的目录里，<b>只有开着的那个终端能看</b>；窗口关掉就翻不回来。</li>
    <li>哪个在等你回话、哪个还在跑、哪个半路崩了——<b>没有一个统一的答案</b>，只能一个个窗口去点。</li>
    <li>想回看两小时前那段输出，只能<b>往回滚终端</b>，滚过头就没了。</li>
  </ul>
  <p class="note">它不是再加一个 agent，而是把已经跑着的这些，变得看得见。</p>
</section>""",
  f"""<section class="slide" id="s3">
  <p class="eyebrow">03 · 能力一</p>
  <div class="cols">
    <div>
      <h2>与 agent 无关：<br>一个列表装下本机所有会话</h2>
      <ul class="points">
        <li><b>Claude Code、Codex、QoderWork</b> 并排在同一棵树里，按 agent、按项目分组。</li>
        <li><b>零配置</b>：直接读各家自己写在磁盘上的会话文件。不装钩子、不改命令、不要求你换工作方式。</li>
        <li>会话<b>被访问时才登记</b>，不做全盘扫描，也不往你的磁盘里加东西。</li>
        <li>多支持一种 agent，只是多一个适配器。</li>
      </ul>
    </div>
    <figure>
      <img src="{SHOT_LIST}" alt="三种 agent 的会话在同一个列表里">
      <figcaption>左侧一棵树：Claude Code 的两个项目、Codex 的一个、QoderWork 的一个，都是同一次扫描的结果。</figcaption>
    </figure>
  </div>
</section>""",
  """<section class="slide" id="s4">
  <p class="eyebrow">04 · 能力二</p>
  <h2>只观察，不打扰</h2>
  <div class="tiles">
    <div class="tile"><h3>只读</h3><p>不改写你的会话文件，不介入 agent 的运行。关掉它，一切照旧。</p></div>
    <div class="tile"><h3>只在回环</h3><p>服务绑定 <code>127.0.0.1</code>，不对外开放，不把任何内容发到别处。</p></div>
    <div class="tile"><h3>链接带签名</h3><p>页面给出的每个路径都带能力签名，只能做页面提供过的那件事；本机文件能否内联渲染是一个可设策略。</p></div>
    <div class="tile"><h3>要回话才需授权</h3><p>从浏览器回一句给正在等你的会话，需要显式配对并同意。默认只看。</p></div>
  </div>
</section>""",
  f"""<section class="slide" id="s5">
  <p class="eyebrow">05 · 能力三</p>
  <div class="cols">
    <div>
      <h2>跟进、下钻、搜索<br>都在一屏之内</h2>
      <div class="states">
        <span class="state a"><i></i>最近活跃 <em>一小时内有动静</em></span>
        <span class="state b"><i></i>被阻塞 <em>在等你</em></span>
        <span class="state c"><i></i>空闲 <em>没有欠你的</em></span>
      </div>
      <ul class="points">
        <li><b>状态是会话自己报出来的</b>：权限、提问、计划批准，或失败、卡住、半路退出——都归入「被阻塞」。</li>
        <li><b>过滤器</b>就是这三个勾选框，默认给你「最近活跃 + 被阻塞」。</li>
        <li><b>大纲抽屉</b>：回合、任务、子 agent，拉开一格看一格；<b>会话内搜索</b>直接跳到命中处。</li>
        <li>跟着尾部走，新内容到了<b>不抢走你正在看的位置</b>。</li>
      </ul>
    </div>
    <figure>
      <img src="{SHOT_FILTER}" alt="过滤器把会话分成最近活跃、被阻塞、空闲">
      <figcaption>三类各带计数；被阻塞的那一个在列表里是琥珀色圆点，标题栏直接写着 Awaiting a reply。</figcaption>
    </figure>
  </div>
</section>""",
  f"""<section class="slide" id="s6">
  <p class="eyebrow">06 · 能力三（续）</p>
  <div class="cols">
    <div>
      <h2>点进去就知道<br>它为什么停下来</h2>
      <ul class="points">
        <li>读了哪个文件、改了哪一行、哪条测试失败，最后<b>它问了你什么</b>。</li>
        <li>工具调用、差异、输出都是<b>可展开的记录</b>，不是一大片纯文本。</li>
        <li>文件路径是链接：<b>在访达里定位</b>，或按策略内联查看。</li>
      </ul>
    </div>
    <figure>
      <img src="{SHOT_DRILL}" alt="下钻到一次工具调用">
      <figcaption>一次改动加一次测试：几个事件折在一行里，展开就是全部。</figcaption>
    </figure>
  </div>
</section>""",
  f"""<section class="slide" id="s7">
  <p class="eyebrow">07 · 能力四</p>
  <div class="cols">
    <div>
      <h2>几百 MB 的会话，秒开</h2>
      <p class="lede" style="margin-bottom:18px">页面只挂载看得见的那一段，会话本体在服务端缓存里常驻——不是「加载完再给你看」，而是立刻可读、可滚、可跳到末尾。</p>
      <table class="num">
        <tr><td>首次打开（同时建立缓存）</td><td>5.1 s</td></tr>
        <tr><td>再次打开（缓存已在）</td><td>1.2 – 2.3 s</td></tr>
        <tr><td>会话大小 / 回合数</td><td>200 MB / 8,991</td></tr>
        <tr><td>跳到末尾</td><td>即时</td></tr>
      </table>
      <p class="note">数字是录制时当场量的：一台 36 GB 内存的 Mac，会话为合成数据。量级就是这个量级。</p>
    </div>
    <figure>
      <img src="{SHOT_BIG}" alt="200 MB 的会话跳到末尾">
      <figcaption>同一个会话跳到第 8,991 回合：大纲仍逐条列着，左下角还累着这轮的成本。</figcaption>
    </figure>
  </div>
</section>""",
  """<section class="slide" id="s8">
  <p class="eyebrow">08 · 还有</p>
  <h2>顺带解决的几件事</h2>
  <div class="tiles">
    <div class="tile"><h3>成本与 token</h3><p>每个会话一个数字，翻到哪一轮都在那儿。</p></div>
    <div class="tile"><h3>导出一个 HTML</h3><p>单文件、自包含、不需要网络：发给同事，他什么都不用装。</p></div>
    <div class="tile"><h3>多机汇总</h3><p><code>agent-monitor-fleet</code> 把几台机器的监视器并到一页。</p></div>
    <div class="tile"><h3>状态可被脚本消费</h3><p>状态与其变更同时写成文件，别的工具直接读，不必再解析 transcript。</p></div>
    <div class="tile"><h3>两种外观</h3><p>新的应用外壳与经典页面并存，随时切换，选择会记住。</p></div>
    <div class="tile"><h3>终端里也能看</h3><p><code>agent-replay</code> 是同一套解析的终端查看器。</p></div>
  </div>
</section>""",
  f"""<section class="slide" id="s9">
  <p class="eyebrow">09 · 演示</p>
  <h2 style="margin-bottom:14px">37 秒，走一遍</h2>
  <figure style="max-width:min(100%,1000px)">
    <video src="{VIDEO}" poster="{SHOT_BIG}" controls playsinline preload="metadata"></video>
    <figcaption>一个列表里的三种 agent → 状态分诊 → 下钻到失败的那条测试 → 会话内搜索 → 200 MB 会话打开与滚动。录自本机应用外壳，数据为合成会话。</figcaption>
  </figure>
</section>""",
  """<section class="slide" id="s10">
  <p class="eyebrow">10 · 开始</p>
  <div class="cols">
    <div>
      <h2>三个入口，<br>从最小的开始</h2>
      <p class="note" style="border:0;padding:0;margin:0">打开之后不需要任何配置：它会在你已经有的会话目录里找到该找的东西。</p>
    </div>
    <pre><b># 装</b>
brew install tanghong123/tap/agent-replay

<b># 看一个会话（终端）</b>
agent-replay --latest

<b># 本机所有会话（浏览器）</b>
agent-monitor

<b># 发给同事：一个自包含的 HTML</b>
agent-replay --latest --dump-html session.html</pre>
  </div>
</section>""",
]

JS = """
(function () {
  var deck = document.querySelector('.deck');
  var slides = [].slice.call(document.querySelectorAll('.slide'));
  var now = document.getElementById('pgNow'), bar = document.getElementById('pgBar');
  if (new URLSearchParams(location.search).has('print')) document.body.classList.add('print');
  function index() {
    var top = deck.scrollTop, best = 0, d = Infinity;
    slides.forEach(function (s, i) { var x = Math.abs(s.offsetTop - top); if (x < d) { d = x; best = i; } });
    return best;
  }
  function paint() {
    var i = index();
    if (now) now.textContent = String(i + 1).padStart(2, '0');
    if (bar) bar.style.width = (i / (slides.length - 1) * 100) + '%';
  }
  function go(step) {
    var i = Math.min(slides.length - 1, Math.max(0, index() + step));
    deck.scrollTo({ top: slides[i].offsetTop, behavior: 'smooth' });
  }
  deck.addEventListener('scroll', function () { window.requestAnimationFrame(paint); }, { passive: true });
  document.addEventListener('keydown', function (e) {
    if (/^(INPUT|TEXTAREA)$/.test(e.target.tagName || '') || e.metaKey || e.ctrlKey) return;
    if (e.key === 'ArrowRight' || e.key === 'PageDown' || e.key === ' ' || e.key === 'ArrowDown') { e.preventDefault(); go(1); }
    else if (e.key === 'ArrowLeft' || e.key === 'PageUp' || e.key === 'ArrowUp') { e.preventDefault(); go(-1); }
    else if (e.key === 'Home') { e.preventDefault(); deck.scrollTo({ top: 0, behavior: 'smooth' }); }
    else if (e.key === 'End') { e.preventDefault(); deck.scrollTo({ top: slides[slides.length - 1].offsetTop, behavior: 'smooth' }); }
  });
  var prev = document.getElementById('pgPrev'), next = document.getElementById('pgNext');
  if (prev) prev.onclick = function () { go(-1); };
  if (next) next.onclick = function () { go(1); };
  paint();
})();
"""

SHELL_OPEN = "" if bare else """<!doctype html>
<html lang="zh-Hans">
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
"""
SHELL_CLOSE = "" if bare else "</html>\n"

HTML = (
    SHELL_OPEN
    + "<title>本机会话观察台</title>\n<style>"
    + CSS
    + "</style>\n<div class=\"rail\"><i id=\"pgBar\"></i></div>\n<div class=\"deck\">\n"
    + "\n".join(SLIDES)
    + f"\n</div>\n<div class=\"pager\"><button id=\"pgPrev\" aria-label=\"上一页\">&lsaquo;</button><span><span id=\"pgNow\">01</span> / {len(SLIDES):02d}</span><button id=\"pgNext\" aria-label=\"下一页\">&rsaquo;</button></div>\n<script>"
    + JS
    + "</script>\n"
    + SHELL_CLOSE
)

with open(out, "w") as fh:
    fh.write(HTML)
print(f"{out}: {os.path.getsize(out) / 1024:.0f} KB · {len(SLIDES)} slides · video {'inline' if inline else 'linked'}")
