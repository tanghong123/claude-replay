#!/usr/bin/env python3
"""make_store.py <root> — a HERMETIC demo store for the agent-monitor pitch video (#208).

Nothing here comes from a real session: every prompt, answer and tool result is written by hand
below. Three agents, several projects, and one deliberately large transcript for the performance
shot. Timestamps are relative to now, so the sessions read as "recently active".
"""
import json, os, random, sys, time

def dumps(obj, **_):
    # COMPACT: the Claude adapter's turn_ended (and the tail's sniffs) match on `"key":"value"`
    # with no spaces, so a pretty-printed fixture never reads as an ended turn.
    return json.dumps(obj, ensure_ascii=False, separators=(",", ":"))

root = sys.argv[1]
BIG_MB = int(sys.argv[2]) if len(sys.argv) > 2 else 200
now = time.time()
random.seed(7)

def ts(secs_ago):
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(now - secs_ago))

def user(text, secs, cwd):
    return dumps({"type": "user", "cwd": cwd, "message": {"role": "user", "content": [{"type": "text", "text": text}]}, "timestamp": ts(secs)})

def assistant(text, secs, stop="end_turn"):
    return dumps({"type": "assistant", "message": {"role": "assistant", "model": "claude-fable-5-1", "stop_reason": stop, "content": [{"type": "text", "text": text}], "usage": {"input_tokens": 1200, "output_tokens": 340}}, "timestamp": ts(secs)})

def thinking(text, secs):
    return dumps({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "thinking", "thinking": text}]}, "timestamp": ts(secs)})

def call(cid, name, inp, secs):
    return dumps({"type": "assistant", "message": {"role": "assistant", "stop_reason": "tool_use", "content": [{"type": "tool_use", "id": cid, "name": name, "input": inp}]}, "timestamp": ts(secs)})

def result(cid, text, secs, err=False):
    return dumps({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": cid, "content": text, "is_error": err}]}, "timestamp": ts(secs)})

def write(path, lines):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as fh:
        fh.write("\n".join(lines) + "\n")
    return path

SID = lambda n: f"aaaaaaaa-0000-4000-8000-{n:012d}"
claude = os.path.join(root, "claude")

# 1. A session BLOCKED on a question — the one that needs the reader.
cwd = "/Users/demo/code/payments"
proj = os.path.join(claude, "-Users-demo-code-payments")
lines = [
    user("把结算服务的重试改成指数退避，上限 30 秒", 900, cwd),
    thinking("先看现在的重试是怎么写的，再决定退避的基数。", 890),
    call("t1", "Read", {"file_path": "/Users/demo/code/payments/src/retry.rs"}, 885),
    result("t1", "pub fn retry(attempt: u32) -> Duration {\n    Duration::from_secs(2)\n}\n", 884),
    call("t2", "Edit", {"file_path": "/Users/demo/code/payments/src/retry.rs", "old_string": "Duration::from_secs(2)", "new_string": "Duration::from_secs(2u64.pow(attempt).min(30))"}, 880),
    result("t2", "The file /Users/demo/code/payments/src/retry.rs has been updated.", 879),
    call("t3", "Bash", {"command": "cargo test -p payments retry"}, 870),
    result("t3", "running 3 tests\ntest retry::backoff_is_exponential ... ok\ntest retry::backoff_is_capped ... ok\ntest retry::first_attempt_is_immediate ... FAILED\n\nfailures:\n    retry::first_attempt_is_immediate\n", 840),
    assistant("第一次重试现在也要等 1 秒。你要的是第一次立刻重试，还是从 1 秒开始？", 830, "end_turn"),
]
write(os.path.join(proj, f"{SID(1)}.jsonl"), lines)

# 2. A session still working — busy, with a long tool run.
cwd = "/Users/demo/code/search"
proj = os.path.join(claude, "-Users-demo-code-search")
lines = [user("给检索加上中文分词，先评估两个候选库", 300, cwd), thinking("jieba 和 rust-segmentation 都能用，先量一下速度和体积。", 295)]
for i in range(6):
    lines += [call(f"s{i}", "Bash", {"command": f"cargo bench --bench tokenize -- case{i}"}, 280 - i * 30),
              result(f"s{i}", f"tokenize/case{i}   time: [{1.2 + i * 0.3:.3f} ms {1.4 + i * 0.3:.3f} ms]\n" + "\n".join(f"  sample {j}: {random.random():.6f}" for j in range(40)), 275 - i * 30)]
lines += [call("s6", "Bash", {"command": "cargo bench --bench tokenize -- case6"}, 60)]
write(os.path.join(proj, f"{SID(2)}.jsonl"), lines)

# 3. A finished session, idle.
cwd = "/Users/demo/code/site"
proj = os.path.join(claude, "-Users-demo-code-site")
lines = [user("把首页的字体换成思源宋体，并检查移动端换行", 5400, cwd),
         call("w1", "Write", {"file_path": "/Users/demo/code/site/style.css", "content": "body{font-family:'Source Han Serif SC',serif}"}, 5390),
         result("w1", "File created successfully.", 5389),
         assistant("换好了，移动端 360px 下标题不再折成三行。", 5380)]
write(os.path.join(proj, f"{SID(3)}.jsonl"), lines)

# 4. Codex and QoderWork sessions — the other agents in the same list.
codex = os.path.join(root, "codex", "sessions", time.strftime("%Y/%m/%d", time.gmtime(now)))
rec = lambda payload, secs: dumps({"timestamp": ts(secs), **payload})
write(os.path.join(codex, f"rollout-{SID(4)}.jsonl"), [
    rec({"type": "session_meta", "payload": {"id": SID(4), "cwd": "/Users/demo/code/infra", "originator": "codex"}}, 2400),
    rec({"type": "event_msg", "payload": {"type": "user_message", "message": "把部署脚本从 bash 迁到 just"}}, 2395),
    rec({"type": "event_msg", "payload": {"type": "agent_message", "message": "先列出 deploy.sh 里所有的目标，再一条条搬。"}}, 2390),
])
# QoderWork keeps <project-slug>/<sid>.jsonl with the title in a sibling <sid>-session.json.
qw = os.path.join(root, "qoderwork", "-Users-demo-code-report")
# QoderWork transcripts are Claude-shaped JSONL (it has a discover module and no model of its
# own), with the title in a sibling <sid>-session.json.
qw_lines = [
    user("报表导出超时，看看是不是 N+1", 7200, "/Users/demo/code/report"),
    call("q1", "Bash", {"command": "rails runner 'Report.new.export'"}, 7180),
    result("q1", "Completed 200 OK in 41822ms (Views: 12.1ms | ActiveRecord: 41010.4ms)\n  Currency Load (0.4ms)  SELECT * FROM rates WHERE id = $1  [x 4312]", 7140),
    assistant("是 N+1：每行都单独查了一次汇率，改成一次批量取就回到 380ms。", 7100),
]
# A store scan drops transcripts under 4 KB as single-prompt junk, so give it a real session.
for i in range(8):
    qw_lines += [
        user(f"顺便把第 {i} 张报表的列宽也对齐", 7090 - i * 40, "/Users/demo/code/report"),
        call(f"q{i + 2}", "Edit", {"file_path": f"/Users/demo/code/report/app/views/report_{i}.erb", "old_string": "width: auto", "new_string": "width: 12ch"}, 7085 - i * 40),
        result(f"q{i + 2}", f"The file /Users/demo/code/report/app/views/report_{i}.erb has been updated.", 7084 - i * 40),
        assistant(f"第 {i} 张对齐好了，表头和数据列都用 12ch。", 7080 - i * 40),
    ]
write(os.path.join(qw, f"{SID(5)}.jsonl"), qw_lines)
write(os.path.join(qw, f"{SID(5)}-session.json"), [dumps({"name": "重构报表导出", "cwd": "/Users/demo/code/report"})])

# 5. The big one: a transcript of BIG_MB megabytes, for the performance shot.
cwd = "/Users/demo/code/kernel"
proj = os.path.join(claude, "-Users-demo-code-kernel")
path = os.path.join(proj, f"{SID(9)}.jsonl")
os.makedirs(proj, exist_ok=True)
target = BIG_MB * 1024 * 1024
blob = "\n".join(f"[{i:05d}] pass {i % 7}: resolved {i * 37 % 9973} symbols, {i * 11 % 577} relocations" for i in range(400))
written = 0
with open(path, "w") as fh:
    i = 0
    while written < target:
        secs = max(30, 86400 - i * 3)
        chunk = "\n".join([
            user(f"第 {i} 轮：把链接器的符号解析再快一点", secs, cwd),
            thinking(f"第 {i} 轮的热点还是在符号表的哈希上。", secs - 1),
            call(f"k{i}", "Bash", {"command": f"cargo bench --bench link -- round{i}"}, secs - 2),
            result(f"k{i}", blob, secs - 3),
            assistant(f"第 {i} 轮：符号解析快了 {i % 9 + 1}%，热点转到重定位。", secs - 4),
        ]) + "\n"
        fh.write(chunk)
        written += len(chunk.encode())
        i += 1
print(json.dumps({"root": root, "big_mb": round(os.path.getsize(path) / 1048576, 1), "big_turns": i, "sessions": 6}))
