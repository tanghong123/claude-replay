#!/usr/bin/env python3
"""build_deck.py — the agent-monitor elevator pitch deck (#208), Chinese, self-contained.

Two builds from one source: `--inline-video` embeds the demo as a data URI (for a published
page), otherwise the page points at `demo/agent-monitor-demo.mp4` beside it (for the repo).
Every figure in the deck was measured in the demo run recorded by tour.mjs against a hermetic
store; nothing here comes from a real session.
"""
import base64, os, sys

HERE = os.path.dirname(os.path.abspath(__file__))
inline = "--inline-video" in sys.argv
# The publish target wraps the page itself, so that build omits the document shell.
bare = "--bare" in sys.argv
out = sys.argv[1]

def data_uri(name, mime):
    with open(os.path.join(HERE, name), "rb") as fh:
        return f"data:{mime};base64," + base64.b64encode(fh.read()).decode()

SHOT_LIST = data_uri("still-4.jpg", "image/jpeg")
SHOT_FILTER = data_uri("still-12.jpg", "image/jpeg")
SHOT_DRILL = data_uri("still-20.jpg", "image/jpeg")
SHOT_BIG = data_uri("still-35.jpg", "image/jpeg")
VIDEO = data_uri("agent-monitor-demo.mp4", "video/mp4") if inline else "demo/agent-monitor-demo.mp4"
POSTER = SHOT_BIG

SHELL_OPEN = "" if bare else """<!doctype html>
<html lang="zh-Hans">
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
"""
SHELL_CLOSE = "" if bare else "</html>\n"

HTML = f"""{SHELL_OPEN}<title>本机会话观察台</title>
<style>
:root {{
  --ink:#101520; --sub:#48536a; --faint:#8b95ab; --line:#dde3ee;
  --paper:#f6f8fc; --card:#ffffff;
  --primary:#2f6ee8; --primary-soft:#eaf1fe;
  --amber:#c8781c; --green:#2e7d55;
  --mono:ui-monospace,SFMono-Regular,"SF Mono",Menlo,monospace;
  --han:"PingFang SC","Hiragino Sans GB","Source Han Sans SC","Noto Sans CJK SC","Microsoft YaHei",sans-serif;
}}
* {{ box-sizing:border-box }}
html {{ scroll-behavior:smooth }}
body {{ margin:0; background:var(--paper); color:var(--ink); font-family:var(--han); font-size:17px; line-height:1.75; -webkit-font-smoothing:antialiased }}
.deck {{ max-width:1080px; margin:0 auto; padding:0 26px 90px }}
section {{ padding:64px 0 8px; border-top:1px solid var(--line) }}
section:first-of-type {{ border-top:0 }}
.eyebrow {{ font:600 12px/1.6 var(--mono); letter-spacing:.16em; text-transform:uppercase; color:var(--faint); margin:0 0 14px }}
h1 {{ font-size:52px; line-height:1.16; letter-spacing:-.015em; margin:0 0 18px; font-weight:700; text-wrap:balance }}
h2 {{ font-size:31px; line-height:1.35; letter-spacing:-.01em; margin:0 0 14px; font-weight:700; text-wrap:balance }}
p {{ margin:0 0 16px; max-width:46em }}
.lede {{ font-size:20px; color:var(--sub) }}
.cover {{ padding:96px 0 40px }}
.cover .mark {{ display:inline-flex; align-items:center; gap:11px; margin-bottom:26px }}
.cover .mark svg {{ width:34px; height:34px }}
.cover .mark span {{ font:650 19px/1 var(--mono); letter-spacing:-.01em }}
.cover h1 {{ font-size:60px }}
.cover .sub {{ font-size:22px; color:var(--sub); max-width:30em; margin-bottom:34px }}
.facts {{ display:flex; flex-wrap:wrap; gap:10px 36px; padding:22px 0 0; border-top:1px solid var(--line) }}
.fact b {{ display:block; font:650 27px/1.2 var(--mono); font-variant-numeric:tabular-nums; letter-spacing:-.02em }}
.fact span {{ font-size:13.5px; color:var(--faint) }}
ul.points {{ list-style:none; margin:0 0 14px; padding:0; max-width:46em }}
ul.points li {{ position:relative; padding:0 0 0 26px; margin:0 0 13px }}
ul.points li:before {{ content:""; position:absolute; left:3px; top:.72em; width:7px; height:7px; border-radius:2px; background:var(--primary) }}
ul.points li b {{ font-weight:650 }}
.grid {{ display:grid; grid-template-columns:repeat(auto-fit,minmax(230px,1fr)); gap:14px; margin:22px 0 6px }}
.tile {{ background:var(--card); border:1px solid var(--line); border-radius:13px; padding:17px 18px }}
.tile h3 {{ margin:0 0 7px; font-size:16px; font-weight:650 }}
.tile p {{ margin:0; font-size:14.5px; color:var(--sub); line-height:1.7 }}
figure {{ margin:26px 0 10px }}
figure img, figure video {{ width:100%; display:block; border:1px solid var(--line); border-radius:13px; box-shadow:0 18px 44px rgba(20,30,55,.10); background:#fff }}
figcaption {{ margin-top:10px; font-size:13.5px; color:var(--faint) }}
.states {{ display:flex; flex-wrap:wrap; gap:9px; margin:4px 0 20px }}
.state {{ display:inline-flex; align-items:center; gap:8px; padding:7px 14px; border-radius:999px; border:1px solid var(--line); background:var(--card); font-size:14.5px }}
.state i {{ width:8px; height:8px; border-radius:50%; display:block }}
.state.a i {{ background:var(--green) }} .state.b i {{ background:var(--amber) }} .state.c i {{ background:var(--faint) }}
.state em {{ font-style:normal; color:var(--faint); font-size:13px }}
table.num {{ border-collapse:collapse; margin:8px 0 18px; font-size:15px }}
table.num td {{ padding:7px 22px 7px 0; border-bottom:1px solid var(--line) }}
table.num td:last-child {{ font:600 15px/1.5 var(--mono); font-variant-numeric:tabular-nums; text-align:right; padding-right:0 }}
code {{ font-family:var(--mono); font-size:13.5px; background:var(--primary-soft); color:#1b4ba8; padding:2px 6px; border-radius:5px }}
pre {{ background:#0f1522; color:#e6ecf7; border-radius:12px; padding:16px 18px; overflow-x:auto; font-family:var(--mono); font-size:13.5px; line-height:1.75 }}
pre b {{ color:#8fb8ff; font-weight:600 }}
.note {{ font-size:14px; color:var(--faint); border-left:2px solid var(--line); padding-left:14px; margin:18px 0 0; max-width:44em }}
footer {{ padding:46px 0 0; border-top:1px solid var(--line); color:var(--faint); font-size:13.5px }}
@media (max-width:720px) {{ .cover h1 {{ font-size:38px }} h1 {{ font-size:34px }} h2 {{ font-size:24px }} body {{ font-size:16px }} }}
@media (prefers-color-scheme:dark) {{
  :root:not([data-theme="light"]) {{ --ink:#e9eef8; --sub:#a8b3c8; --faint:#79859c; --line:#242c3c; --paper:#0c111b; --card:#131a27; --primary:#6f9dfb; --primary-soft:#16233c }}
  :root:not([data-theme="light"]) figure img, :root:not([data-theme="light"]) figure video {{ box-shadow:0 18px 44px rgba(0,0,0,.5) }}
  :root:not([data-theme="light"]) code {{ color:#a9c7ff }}
}}
</style>
<div class="deck">

<section class="cover">
  <div class="mark">
    <svg viewBox="0 0 24 24" fill="none" stroke="#2e7d55" stroke-width="1.7" stroke-linecap="round"><rect x="3" y="4" width="18" height="13" rx="2"/><path d="M8 21h8M12 17v4M7 12l2.5-3 2 4 2-5 1.5 4"/></svg>
    <span>agent-monitor</span>
  </div>
  <h1>本机所有 AI 编码会话，<br>放在同一个页面里看</h1>
  <p class="sub">你同时开着几个 agent、几十个会话。它把它们汇到一处：谁在等你、谁还在跑、上次那段输出在哪——只观察，不插手。</p>
  <div class="facts">
    <div class="fact"><b>3 种</b><span>agent 同列（Claude Code / Codex / QoderWork）</span></div>
    <div class="fact"><b>0 配置</b><span>读各家自己的会话目录</span></div>
    <div class="fact"><b>200 MB</b><span>会话首次 5.1 秒打开</span></div>
    <div class="fact"><b>127.0.0.1</b><span>只在回环，不上传</span></div>
  </div>
</section>

<section>
  <p class="eyebrow">问题</p>
  <h2>会话越多，越没人知道现在该看哪一个</h2>
  <ul class="points">
    <li>会话散落在各家 agent 自己的目录里，<b>只有开着的那个终端能看</b>；关掉就翻不回来。</li>
    <li>哪个在等你回话、哪个还在跑、哪个半路崩了——<b>没有一个统一的答案</b>，只能一个个窗口去点。</li>
    <li>想回看两小时前那段输出，只能<b>往回滚终端</b>，滚过头就没了。</li>
  </ul>
  <p class="note">这不是要再加一个 agent，而是把已经跑着的这些，变得看得见。</p>
</section>

<section>
  <p class="eyebrow">能力 1</p>
  <h2>与 agent 无关：一个列表，装下本机所有会话</h2>
  <ul class="points">
    <li><b>Claude Code、Codex、QoderWork</b> 并排在同一棵树里，按 agent、按项目分组。</li>
    <li><b>零配置</b>：直接读各家自己写在磁盘上的会话文件。不装钩子、不改命令、不要求你换工作方式。</li>
    <li>会话是<b>被访问时才登记</b>的，不做全盘扫描，也不给你的磁盘添东西。</li>
    <li>多支持一种 agent，只是多一个适配器——机器那一半是共用的。</li>
  </ul>
  <figure>
    <img src="{SHOT_LIST}" alt="三种 agent 的会话在同一个列表里">
    <figcaption>左侧一棵树：Claude Code 的两个项目、Codex 的一个、QoderWork 的一个——都是同一次扫描的结果。</figcaption>
  </figure>
</section>

<section>
  <p class="eyebrow">能力 2</p>
  <h2>只观察，不打扰</h2>
  <div class="grid">
    <div class="tile"><h3>只读</h3><p>不改写你的会话文件，不介入 agent 的运行；关掉它，一切照旧。</p></div>
    <div class="tile"><h3>只在回环</h3><p>服务绑定 <code>127.0.0.1</code>，不对外开放，不把任何内容发到别处。</p></div>
    <div class="tile"><h3>文件链接带签名</h3><p>页面给出的每个路径都带能力签名，链接只能做页面提供过的那件事；本机文件能不能内联渲染，是一个可设的策略。</p></div>
    <div class="tile"><h3>要回话才需要授权</h3><p>从浏览器回一句给正在等你的会话，需要显式配对并同意——默认只看。</p></div>
  </div>
</section>

<section>
  <p class="eyebrow">能力 3</p>
  <h2>跟进、下钻、搜索：三件事都在一屏之内</h2>
  <div class="states">
    <span class="state a"><i></i>最近活跃 <em>一小时内有动静，或正在跑</em></span>
    <span class="state b"><i></i>被阻塞 <em>在等你：权限、提问、计划批准，或失败与卡住</em></span>
    <span class="state c"><i></i>空闲 <em>这一轮完了，没有欠你的</em></span>
  </div>
  <ul class="points">
    <li><b>状态是会话自己报出来的</b>，不是你去猜：它来自对每个会话的观察，同一套词也写进文件供脚本消费。</li>
    <li><b>过滤器</b>就是这三个勾选框，默认只给你「最近活跃 + 被阻塞」——需要全部时按一下「Everything」。</li>
    <li><b>大纲抽屉</b>：回合、任务、子 agent、会话信息，拉开一格看一格。</li>
    <li><b>会话内搜索</b>直接跳到命中处；工具调用、差异、文件都是能展开的记录，不是一堆纯文本。</li>
    <li>跟着尾部走：新内容到了也<b>不抢走你正在看的位置</b>。</li>
  </ul>
  <figure>
    <img src="{SHOT_FILTER}" alt="过滤器把会话分成最近活跃、被阻塞、空闲">
    <figcaption>过滤器打开的样子：三类各有计数，「被阻塞」的那一个在列表里带着琥珀色圆点，标题栏直接写着「Awaiting a reply」。</figcaption>
  </figure>
  <figure>
    <img src="{SHOT_DRILL}" alt="下钻到一次工具调用">
    <figcaption>点进去就知道它为什么停下来：读了哪个文件、改了哪一行、哪条测试失败，最后它问了你什么。</figcaption>
  </figure>
</section>

<section>
  <p class="eyebrow">能力 4</p>
  <h2>几百 MB 的会话，秒开</h2>
  <p>演示里那个会话是 <b>200 MB、8,991 个回合</b>。页面只挂载看得见的那一段（虚拟窗口），会话本体在服务端缓存里常驻——所以它不是"加载完再给你看"，而是立刻可读、可滚、可跳到末尾。</p>
  <table class="num">
    <tr><td>首次打开（同时建立缓存）</td><td>5.1 s</td></tr>
    <tr><td>再次打开（缓存已在）</td><td>1.2 – 2.3 s</td></tr>
    <tr><td>会话大小 / 回合数</td><td>200 MB / 8,991</td></tr>
    <tr><td>跳到末尾</td><td>即时</td></tr>
  </table>
  <figure>
    <img src="{SHOT_BIG}" alt="200 MB 的会话跳到末尾">
    <figcaption>同一个会话跳到第 8,991 回合：大纲仍然逐条列着，成本累计 ~$260.74 也在左下角。</figcaption>
  </figure>
  <p class="note">这些数字是这次录制时当场量的（一台 36 GB 内存的 Mac，会话为合成数据）。它们随机器与会话形状而变，但量级就是这个量级。</p>
</section>

<section>
  <p class="eyebrow">还有</p>
  <h2>顺带解决的几件事</h2>
  <div class="grid">
    <div class="tile"><h3>成本与 token</h3><p>每个会话一个数字，翻到哪一轮都在那儿。</p></div>
    <div class="tile"><h3>导出一个 HTML</h3><p>单文件、自包含、不需要网络：发给同事，他什么都不用装就能看。</p></div>
    <div class="tile"><h3>多机汇总</h3><p><code>agent-monitor-fleet</code> 把几台机器的监视器并到一页。</p></div>
    <div class="tile"><h3>状态可被脚本消费</h3><p>状态与其变更同时写成文件，别的工具可以直接读，不必再解析 transcript。</p></div>
    <div class="tile"><h3>两种外观</h3><p>新的应用外壳与经典页面并存，随时切换，选择会被记住。</p></div>
    <div class="tile"><h3>终端里也能看</h3><p><code>agent-replay</code> 是同一套解析的终端查看器，没有浏览器时照样用。</p></div>
  </div>
</section>

<section>
  <p class="eyebrow">演示</p>
  <h2>37 秒，走一遍</h2>
  <figure>
    <video src="{VIDEO}" poster="{POSTER}" controls playsinline preload="metadata"></video>
    <figcaption>录制自本机的应用外壳，数据为合成的演示会话：一个列表里的三种 agent → 状态分诊 → 下钻到失败的那条测试 → 会话内搜索 → 200 MB 会话打开与滚动。</figcaption>
  </figure>
</section>

<section>
  <p class="eyebrow">开始</p>
  <h2>三个入口，从最小的开始</h2>
  <pre><b># 装</b>
brew install tanghong123/tap/agent-replay

<b># 看一个会话（终端）</b>
agent-replay --latest

<b># 本机所有会话（浏览器）</b>
agent-monitor

<b># 发给同事：一个自包含的 HTML</b>
agent-replay --latest --dump-html session.html</pre>
  <p class="note">打开 <code>agent-monitor</code> 之后不需要任何配置：它会在你已经有的会话目录里找到该找的东西。</p>
</section>

<footer>agent-monitor v1.266.0 · 演示与数字均来自合成会话，本页不含任何真实会话内容</footer>
</div>
{SHELL_CLOSE}"""

with open(out, "w") as fh:
    fh.write(HTML)
print(f"{out}: {os.path.getsize(out) / 1024:.0f} KB (video {'inline' if inline else 'linked'})")
