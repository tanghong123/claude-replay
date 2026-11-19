# Pairing a browser from the monitor's page (#s590)

> **Stage 1 is built (#s618); stage 2 is not yet (#s619).** The owner chose to build both, in two
> stages (#s605). Stage 1 is a paired monitor's page asking for a code (4.1–4.5, 4.7); stage 2 is an
> unpaired monitor turning pairing on from the page (4.6). Each stage lands only on the owner's yes,
> since pairing is the owner's. Phone codes stay in the terminal (#s585).

## 1. What it is for

Writing to a session from the monitor's page needs a paired browser. Today the only way to pair a
browser on this machine is a terminal: `agent-monitor --pair` makes the monitor require a token and
opens the default browser with it. A contributor's change (#s577) let the page pair its own browser
with one click instead. The owner turned that down as written (#s585): the one click proved only
that the request came from a process running as this user, which a forwarder also is. Pairing from
the page waits for a confirmation only this machine can give: "a code in the monitor's terminal, or
a dialog".

The goal: a person sitting at this machine can pair the browser in front of them without opening a
terminal, while anyone who can only reach the monitor's port gains nothing.

## 2. Where things stand

- **The token is the root of trust.** It is a secret in a file only this user can read (mode 0600).
  The OS's file permissions are what make it this user's, on every platform
  (`fleet-pairing.md`, the token gate). One token serves every device; the cookie a browser holds is
  that token.
- **Phones pair by a one-time code** (`fleet-pairing.md`, the one-time code over `tailscale serve`).
  `agent-monitor --pair-phone` mints a code into `<state>/pair-codes` (0600). The code is 8
  characters from a 31-letter alphabet, good once, for five minutes. The phone types it into
  `/pair`, and `POST /api/pair` swaps it for the cookie. Five wrong tries burn every outstanding code.
  These routes answer ahead of the gate, on a paired monitor only.
- **Writes need the token.** An unpaired monitor serves reads to this user's loopback peers and
  refuses every write.
- **`--pair` against a running unpaired monitor does nothing until a restart** (measured on
  1.537.0). It writes the token file and says the monitor is already running. The running monitor
  keeps serving unpaired: a tokened address sets no cookie and writes are still refused. Only after
  a restart does it require the token. The page's advice, `agent-monitor --pair`, therefore does
  nothing visible on its own. This gap is filed as a bug of its own.

## 3. What the confirmation must prove

That the person asking can **see this user's screen on this machine, or read this user's files**.
That is the token file's own root of trust, so a pairing confirmed this way is as strong as
`--pair`.

"The connecting process runs as this user" is not enough. `ssh -R`, `socat` and probably Docker
Desktop's host proxy all run as this user and carry someone else's requests to the loopback, with
whatever `Host` and `Origin` that someone writes. The token then buys writes, including resuming a
finished session, which runs the agent headless with its permission prompts skipped.

## 4. The proposal: a code shown on this machine, typed into the page

### 4.1 The flow

1. The page offers **Pair this browser** where writing needs pairing: Write mode's help, and the
   `/pair` page when it is loaded from this machine.
2. The page asks the monitor for a code. The monitor mints one and shows it **only on this machine**
   (4.2). The reply says where it was shown, never the code.
3. The page shows a code box: "Type the code this Mac shows". The person reads the code off the
   dialog and types it.
4. The page sends the code to `POST /api/pair`, the route phones already use. It sets the usual
   cookie, and the page reloads paired.

The direction matters. The code appears on this machine and is typed into the browser. A dialog
with an Allow button could be clicked by reflex on a request someone else made. A code the person
must read and type cannot reach a browser they are not using.

### 4.2 Where the code is shown

- **On macOS, a dialog the monitor raises** in the logged-in user's session: "A browser on this Mac
  asks to pair with Agent Monitor, so it can write to your sessions. Type this code in that page,
  and only there: ABCD-EFGH. It works once, for five minutes. If you did not ask, click Ignore." Ignore
  burns the code. The dialog closes when the code is used, expires or is replaced, and when the
  monitor stops. *To measure before building* (section 8): that it shows from a monitor run by
  launchd and from one started in a terminal, without a permission prompt.
- **In a terminal: `agent-monitor --pair-code`.** It prints the waiting code, or mints one if none is
  waiting, and prints only to a terminal, as the tokened address already is. This is the way on
  Linux, on a Mac where the dialog cannot show, and for a person reaching the monitor through an
  `ssh -L` tunnel. Their shell on the machine is the proof.
- **Never in a log, a reply or a notification.** The owner's monitor runs under launchd, so its stderr
  is a log file, which may be readable by others. A code there would pair whoever reads it.

### 4.3 Who may ask

- Only a request that came to the loopback, with the monitor's own `Host`, its own `Origin` or none,
  and none of a relay's headers. These are the checks the gate already makes: `origin_ok` and the
  relay check (#s417). A phone over the tailnet keeps `--pair-phone`.
- **One page code at a time.** An ask while one is waiting is answered "a code is already showing
  on this Mac". The person types the code shown, whoever asked for it: a code is not tied to the
  browser that asked, and redeeming it pairs only the browser that types it.
- **At most one ask every 10 seconds.** After three page codes in a row expire or are burned unused,
  asks are refused for an hour. The reply says until when. `--pair-code` in a terminal still works.
  An ask is visible where a dialog can show: on the owner's screen, naming the monitor's port. Where
  none can (another platform, nobody at the console, the dialog switched off), nobody sees the ask;
  its code waits for `--pair-code` in a terminal, so an asker who is not at the machine can only
  guess it.

### 4.4 Page codes are kept apart from phone codes

Page codes live in a file of their own, `<state>/page-codes` (0600), with their own count of
misses. A page code is redeemed only from a request that may ask (4.3). Phone codes are redeemed as
today. So a code shown on the Mac's screen never pairs a phone over the tailnet, and misses at one
kind never burn the other. The monitor clears page codes when it starts; where it cannot, it offers
no page pairing for that run.

A code pairs only once its use is saved, and every miss is counted in the same write. Where the file
cannot be written, a right code is answered as a miss and stays good, so a guess there teaches
nothing, and no page code pairs until the file can be written again. The guess bound below rests on
this.

A code a link carries (`/pair#code=…`, the shape of `--pair-phone`'s link) is a phone code: the page
sends it marked so, and the monitor never judges it against a page code. A site that opens such a
link with made-up codes spends none of a page code's guesses.

**For the owner:** the codes file is in the monitor's state directory, beside the token, so two
monitors on one machine that share a state directory (separate cache roots, separate ports) share
their page codes as they share the token. A code one monitor shows pairs a browser at the other's
page too. Both are this user's, under one token, so it pairs nothing the showing monitor could not;
the dialog names the port that asked, so the person can tell.

### 4.5 What a pairing grants and writes

- **It grants what `--pair` and `--pair-phone` grant:** the cookie holding the one shared token
  (`SameSite=Strict`, 400 days). That allows every read and every write a paired browser has:
  - a send into a pane, after that pane's own grant and the passcode if one is set;
  - a resume of a finished session;
  - answering a dialog;
  - Codex native mode.

  Nothing narrower exists. A per-browser or read-only pairing would be a design of its own; the
  token's file leaves room for a list of devices later.
- **On a paired monitor it writes nothing new.** The page code is taken out of its file.

### 4.6 A monitor not yet paired

The first run is the case #s577 was for: a fresh monitor, unpaired, and Write mode says it needs
pairing. Pairing a browser there means pairing the monitor, as `--pair` does.

- **Before asking**, the page says what it will do: "This makes the monitor require pairing for every
  browser and tool that reaches it, as `agent-monitor --pair` does. Pair this browser?"
- **The confirmed code mints the token** (0600, created only if absent, under one lock). Two browsers
  pairing at once therefore end with one token.
- **The running monitor starts requiring it at once**, with no restart. What reads the token today
  reads it once, at startup, and each changes:
  - the gate starts requiring the token;
  - the page starts saying it is paired;
  - `/pair` starts answering;
  - the relay check an unpaired monitor runs on its own clock (#s422) stops;
  - the startup banner's unpaired warning is followed by one line saying the monitor is now paired.

  The same switch fixes `--pair` against a running monitor (section 2).
- **What it costs:**
  - On macOS every other browser on this machine needs pairing from then on, and so does every tool
    that reads the monitor over the loopback without the token. A tool reads the token file, as it
    must for any paired monitor.
  - On Linux the verified same-user leg still admits this user's own peers without the token.

This part is a separate stage, and a separate choice (section 6). Paired monitors need none of it.

### 4.7 A second browser, and a restart

- **A second browser** asks for its own code, the same way. Browsers that hold the cookie are not
  touched.
- **A restart** keeps every pairing: the token is in its file and the cookie in the browser. A code
  still waiting is cleared, and its dialog closes; the page asks again. A monitor paired from the
  page stays paired across restarts, as one paired by `--pair` does.
- **Unpairing** is unchanged: deleting or rotating the token unpairs every browser and phone.

## 5. Against each threat

| Who | Can they ask? | Can they finish? |
|---|---|---|
| **Another user on this machine** | Yes, through the loopback. A dialog shows in the owner's session, or nowhere. | No. They cannot see the owner's screen or read the 0600 files. |
| **A page on another site, in the owner's browser** | No. Its POST carries a foreign `Origin`, and it cannot read a reply. It can open `/pair` in a tab, where only the person's own click asks; it cannot frame that page (`frame-ancestors 'none'`, `X-Frame-Options: DENY`), and a code its link carries is judged as a phone code alone. | No. |
| **DNS rebinding** | No. The `Host` is a foreign name. | No. |
| **Another tailnet device, through `tailscale serve`** | No. It carries a relay's headers or a tailnet name; it keeps `--pair-phone`. | No. A page code is never redeemed through the relay. |
| **A raw relay (`tailscale serve --tcp`) or a same-user forwarder (`ssh -R`, `socat`, Docker Desktop's proxy)** | Yes. It can forge the loopback `Host` and `Origin`. Each ask raises a dialog on the owner's screen. | No. It never sees the code, which is in the dialog or the terminal. It gets five guesses at 1 in 31⁸ (about 8.5 × 10¹¹) per code, one code at a time, at most three codes before an hour's back-off: at most 360 guesses a day, about 1 in 2 × 10⁹. The bound holds because a miss is counted and a use saved before either is answered (4.4). Every ask is visible to the owner where a dialog can show. |
| **A program running as the owner** | Not in scope. | It can read the token file today. |

What is left is a person typing the code somewhere other than the page that asked. The dialog says
to type it only into the monitor's page on this machine. It also says a code they did not ask for
means someone else asked, and that they should click Ignore.

## 6. The options, judged against the goal

| Option | Meets the goal? | What it changes |
|---|---|---|
| **A. Both cases, in two stages** (recommended): first a code on this machine's screen pairs a browser on a paired monitor; then, on an unpaired monitor, the confirmed code also turns pairing on, at once | Yes, including the first run | Sections 4.1–4.7 |
| **B. Paired monitors only**: stage 1 alone; an unpaired monitor keeps naming `agent-monitor --pair` | For a second browser, not for the first run | Sections 4.1–4.5 and 4.7 |
| **C. A terminal code only**: `agent-monitor --pair-code`, no dialog | No: it still needs a terminal. It only spares copying the tokened address | The flag, the page's code box, page codes |
| **D. No change** (the standing ruling): the page names `agent-monitor --pair` | No. And on a running unpaired monitor the command does nothing until a restart | Nothing (the gap in section 2 is fixed by its own task) |

Considered and not proposed:
- **A dialog with Allow and Deny.** Approval by reflex, on a request someone else made.
- **Touch ID.** The same Allow shape, and it needs system framework bindings in the monitor.
- **The code in the monitor's own output.** Under launchd that is a log file.
- **A token per browser.** It changes the token file's format, and is not needed to reach the goal.
- **Restarting the monitor after an unpaired pairing**, instead of switching at once. It needs a
  supervisor to bring the monitor back, which a monitor started in a terminal does not have.

## 7. What changes, and for whom

- **People who open the monitor:** Write mode's help and `/pair` offer "Pair this browser". The page
  shows a code box, and a dialog shows on the Mac.
- **The command line:** a new flag, `agent-monitor --pair-code`.
- **The routes:** a new `POST /api/pair/ask`, answered ahead of the gate as `/pair` is, with no
  code in its reply. On an unpaired monitor `/pair` and `/api/pair` answer too, for page codes only.
  `backend-contract.md` names the route and the reasons Write mode gives.
- **Files:** a new `<state>/page-codes` (0600). Stage A2 writes the token file as `--pair` does.
- **Hosts that embed the kit:** the help text they show gains the action. Nothing they pass changes.
- **Tools that read an unpaired monitor over the loopback** (stage A2 only): after a page pairing
  they need the token, as after `--pair`.

## 8. To measure before building

- **The dialog, on the macOS versions the monitor supports.** `osascript`'s `display dialog`, raised
  by a monitor that launchd runs and by one started in a terminal:
  - that it shows without a permission prompt (measured for stage 1 on macOS 27.0.1: it does, from
    a terminal and from launchd);
  - what happens with the screen locked, with nobody logged in at the console, and under fast user
    switching (not yet measured);
  - that it closes when the monitor stops, and when its code is used (a SIGTERM closes it within
    15 ms; the monitor's withdraw sends SIGKILL, which ends the same process);
  - whether it takes the focus from the browser.
- **Linux:** no dialog in this design; the terminal is the way.

## 9. Not in this design

- **Phone codes stay in the terminal**, as ruled. The same dialog could one day show a phone code to
  a browser on this machine. That would be a ruling of its own.
- **Narrower pairings** (read-only, per browser, time-limited).
- **The native apps' own pairing.**
