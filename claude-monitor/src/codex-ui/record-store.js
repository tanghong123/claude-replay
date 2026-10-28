// The `/pull` + `/records` protocol's client half is the shared module's (#49): this store
// keeps its timer, its array and its handlers, and applies the plan the reducer returns.
import { cursorText, freshCursor, parseRecords, pullQuery, recordsQuery, reducePull } from "./shared/record-stream.js";

/** How long a first pull may take before the page says what it is doing (#221). Long enough that
 *  a warm open — under a second, measured — never shows it; short enough that a cold one does not
 *  look hung. */
const WAITING_AFTER_MS = 1200;

/** The head of a tail-first open is read in pieces of this many bytes of the log, each its own
 *  `/records` range, so a link that drops a long transfer costs one piece and never the whole head
 *  (#s274: a phone over the tailnet climbed to 48% and fell back to 1%, again and again, while one
 *  9.7 MB read restarted from the top after every drop). The server answers a range in whole
 *  records, and a dropped read keeps the records it finished. */
export const HEAD_PIECE_BYTES = 1024 * 1024;

/** How much of the head a page reading it turn by turn keeps, in bytes of the log (#s285,
 *  design/slow-links.md §6.6; Hong on #s290: "Read up to about 16 MB, then only what I visit"). The
 *  rest behind the reader is read in the background until the turns read hold this much; after that
 *  only what the reader visits or a jump names is read, and the turns farthest from the reader are let
 *  go as they move (`drop`). A head smaller than this is read whole, as before. The largest logs
 *  measured were 90 to 150 MB of records, which Safari held whole. */
export const KEEP_BYTES = 16 * 1024 * 1024;

/** How many times in a row the server may REFUSE a read of the head (an HTTP error, or a range that
 *  brings no whole record) before the page starts the session over. A network drop is never
 *  counted: a phone out of signal keeps what it has and tries again every few seconds, however
 *  long. A drop costs the record in flight, so a record larger than what the link carries between
 *  drops would never finish; the largest record measured across hong-devserver's sessions was
 *  682 KB, against drops 3 MB or more apart. */
const HEAD_REFUSALS = 12;

/** How many times the turn index is asked for before the page does without it (#s276): it is small,
 *  and without it the page reads the tail alone until the head lands, as it did before. */
const INDEX_TRIES = 5;

/** A record that has not arrived yet: the HEAD of a tail-first open (#314,
 *  design/tail-first-open.md). One frozen object stands for each of them; the projection folds
 *  the run into a single pending unit, so nothing ever renders one as a record. */
export const PENDING = Object.freeze({ kind: "pending" });

export class RecordStore {
  /** `options.tailBudget` — ask a FIRST pull for the last this-many bytes of the committed log
   *  (the server honours it only on a log over twice that), draw them, and read the head behind
   *  them (#314). Absent: every open reads the whole log first, as it always did. */
  constructor(handlers, options = {}) {
    this.handlers = handlers;
    this.tailBudget = options.tailBudget || 0;
    this.headPieceBytes = options.headPieceBytes || HEAD_PIECE_BYTES;
    this.keepBytes = options.keepBytes || KEEP_BYTES;
    this.emptySession = options.emptySession;
    this.sessionFailure = options.sessionFailure;
    // Stage 2 (#s277, design/slow-links.md §6): read the head turn by turn, in the order the reader
    // needs it, from the turn index's map, instead of whole from the top (stage 1, the desktop's).
    this.windowed = !!options.windowed;
    // The head still to come after a tail-first open: `{ count, len, epoch }`, or null.
    this.head = null;
    // Stage 2: pieces of the head read and not yet taken by the page (`takeFill`), in read order.
    this.fills = [];
    // The turn index for that head (#s276): what the records still to come hold — their turns,
    // compactions, tool counts, artifacts and offered files — or null. Only while the head is out.
    this.index = null;
    this.session = "";
    this.records = [];
    this.meta = null;
    this.cursor = freshCursor();
    this.generation = 0;
    this.timer = 0;
    // Whether this session's first reply has been applied — `handlers.opened` fires once per open.
    this.shown = false; this.confirmedEmpty = false; this.missingSince = null;
  }

  open(session) {
    this.stop();
    this.session = session;
    this.records = [];
    this.meta = null;
    this.cursor = freshCursor();
    this.shown = false; this.confirmedEmpty = false; this.missingSince = null;
    this.head = null; this.index = null; this.fills = [];
    const generation = ++this.generation;
    this.handlers.reset?.();
    this.poll(generation);
  }

  stop() { clearTimeout(this.timer); this.generation++; }
  cursorText() { return cursorText(this.cursor); }
  recover() {
    this.records = [];
    this.meta = null;
    this.cursor = freshCursor();
    this.head = null; this.index = null; this.fills = [];
    this.handlers.reset?.();
  }

  /** Show an authoritative native empty session even while its first JSONL pull is pending. */
  refreshEmpty() {
    if (this.records.length || !this.session || !this.emptySession?.(this.session)) return false;
    this.confirmedEmpty = true; this.missingSince = null;
    this.handlers.empty?.();
    if (!this.shown) { this.shown = true; this.handlers.opened?.(); }
    return true;
  }

  /** Surface a definite native read failure without waiting for a missing JSONL pull. */
  refreshFailure() {
    if (this.records.length || !this.session) return false;
    const failure = this.sessionFailure?.(this.session);
    if (!failure) return false;
    this.handlers.error?.(new Error(failure.message), false);
    return true;
  }

  async poll(generation) {
    if (generation !== this.generation || !this.session || this.refreshFailure()) return;
    // #221: the FIRST pull on a session with no cache blocks for as long as the server takes to
    // fold and highlight the whole transcript — measured at 86 s for 400 MB, against 0.4 s once
    // the cache is warm. The page had nothing on it for all of that, which a reader cannot tell
    // from a hang. Say what is happening, but only once the wait is long enough to be a wait: a
    // warm open answers in under a second and must not flash a message.
    let waited = 0;
    if (!this.records.length) {
      waited = setTimeout(() => {
        if (generation !== this.generation || this.refreshFailure() || this.refreshEmpty()) return;
        if (generation === this.generation && !this.records.length) this.handlers.waiting?.();
      }, WAITING_AFTER_MS);
    }
    try {
      // Only a store that holds nothing asks for the tail: a fresh open, or one that recovered.
      const tail = this.tailBudget && !this.records.length ? `&tail=${this.tailBudget}` : "";
      const response = await fetch(`/pull?${pullQuery(this.session, this.cursor)}${tail}`, { cache: "no-store" });
      if (generation !== this.generation) return;
      if (response.status === 404) {
        if (this.refreshEmpty()) return;
        // A turn arrives over the native stream before its first rollout reaches the
        // reader index. Only a previously confirmed empty session gets this grace period.
        if (this.confirmedEmpty && !this.records.length) {
          this.missingSince ??= Date.now();
          if (Date.now() - this.missingSince < 10_000) { this.handlers.materializing?.(); return; }
        }
      }
      if (!response.ok) throw new Error(`pull HTTP ${response.status}`);
      const reply = await response.json();
      if (generation !== this.generation) return;
      this.confirmedEmpty = false; this.missingSince = null;
      if (reply.t === "redirect" && reply.url) { location.assign(reply.url); return; }
      if (reply.t === "error") throw new Error(reply.message || "Session stream unavailable");
      if (reply.committed_ext?.len) {
        const ext = reply.committed_ext;
        const records = await fetch(`/records?${recordsQuery(this.session, ext, reply.epoch)}`, { cache: "no-store" });
        // The pointer raced a store reset. Keep the currently rendered records and the old
        // cursor; the next pull sees the epoch bump and resynchronizes atomically.
        if (records.status === 409) return;
        if (!records.ok) throw new Error(`records HTTP ${records.status}`);
        reply.committed = parseRecords(await this.read(records, ext.len, generation));
      } else reply.committed = [];
      if (generation === this.generation) {
        // A tail-first answer: the committed zone starts past records nobody has sent yet.
        const headCount = !this.records.length && reply.committed_ext?.len && reply.committed_from > 0 ? reply.committed_from : 0;
        this.apply(reply, headCount);
        if (headCount) {
          this.head = { count: headCount, len: reply.committed_ext.offset, epoch: reply.epoch };
          this.loadIndex(generation, this.head);
          // Stage 2 reads the head from the index's map, so its reads start when the index lands
          // (`loadIndex`), and without an index the head is read whole after all.
          if (!this.windowed) this.loadHead(generation, this.head);
        }
        if (!this.shown) { this.shown = true; this.handlers.opened?.(); }
      }
    } catch (error) {
      if (generation === this.generation) this.handlers.error?.(error, this.records.length > 0);
    } finally {
      clearTimeout(waited);
      if (generation === this.generation) this.timer = setTimeout(() => this.poll(generation), 1000);
    }
  }

  /** The `/records` body as text. On a FIRST read it reports how much of `total` (the range's own
   *  length, so a gzipped reply is measured in the bytes it decodes to) has arrived: a session
   *  opened over a slow link — a phone on the tailnet, #313 — reads tens of MB, and the page can
   *  say how far along it is instead of only that it is waiting. */
  async read(response, total, generation, onProgress = this.records.length ? null : this.handlers.progress) {
    if (!response.body || !onProgress) return response.text();
    const reader = response.body.getReader(), decoder = new TextDecoder(), parts = [];
    let got = 0;
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      got += value.byteLength;
      parts.push(decoder.decode(value, { stream: true }));
      if (generation === this.generation) onProgress(got, total);
    }
    parts.push(decoder.decode());
    return parts.join("");
  }

  /** The head of a tail-first open, read behind the tail the page has already drawn (#314). It
   *  replaces the placeholders IN PLACE — record indices never move — and is reported as a
   *  prepend (`prepended`: how many records it brought in front of the tail).
   *
   *  It is read as ranges of at most `headPieceBytes` (#s274), and the server answers each in
   *  whole records (an end inside one is carried on to its end; secret masking reads whole
   *  records). Each range's records are parsed as they complete, so a read that drops keeps every
   *  record it finished and the next range starts at the first one it did not: the progress only
   *  climbs, and a drop costs at most one record's bytes; drops are retried for as long as they
   *  last. A 409 (the log left that epoch), a head that will not parse, a count other than the one
   *  the pull named, or a server that keeps refusing (`HEAD_REFUSALS`) starts the session over; a
   *  head is never spliced in. */
  async loadHead(generation, head) {
    const current = () => generation === this.generation && this.head === head;
    head.records ??= [];
    head.got ??= 0;
    head.shown ??= 0;
    const show = got => {
      head.shown = Math.max(head.shown, got);
      this.handlers.headProgress?.(head.shown, head.len);
    };
    try {
      while (head.got < head.len) {
        const len = Math.min(this.headPieceBytes, head.len - head.got);
        const response = await fetch(`/records?${recordsQuery(this.session, { offset: head.got, len }, head.epoch)}`, { cache: "no-store" });
        if (!current()) return;
        if (response.status === 409) { this.recover(); return; }
        if (!response.ok) throw new Error(`records HTTP ${response.status}`);
        const before = head.got;
        // Every record the bytes so far complete is parsed and kept; the bytes after the last
        // newline wait, as chunks, for the rest of their record.
        let pending = [], pendingLength = 0;
        const take = bytes => {
          const cut = bytes.lastIndexOf(10);
          if (cut < 0) { pending.push(bytes); pendingLength += bytes.length; return; }
          const whole = new Uint8Array(pendingLength + cut + 1);
          let at = 0;
          for (const chunk of pending) { whole.set(chunk, at); at += chunk.length; }
          whole.set(bytes.subarray(0, cut + 1), at);
          for (const record of parseRecords(new TextDecoder().decode(whole))) head.records.push(record);
          head.got += whole.length;
          pending = [bytes.slice(cut + 1)]; pendingLength = bytes.length - cut - 1;
        };
        try {
          if (!response.body) {
            const bytes = new Uint8Array(await response.arrayBuffer());
            if (!current()) return;
            take(bytes);
            show(head.got);
          } else {
            const reader = response.body.getReader();
            for (;;) {
              const { done, value } = await reader.read();
              if (!current()) { reader.cancel().catch(() => {}); return; }
              if (done) break;
              take(value);
              show(head.got + pendingLength);
            }
          }
        } catch (error) {
          // What the read finished is kept; a record it was part way through is read again.
          if (head.got > before) { head.refusals = 0; head.drops = 0; }
          throw error;
        }
        if (head.got === before) throw new Error("records: a range brought no whole record");
        head.refusals = 0; head.drops = 0;
      }
      if (head.records.length !== head.count) { this.recover(); return; }
      for (let i = 0; i < head.records.length; i++) this.records[i] = head.records[i];
      this.head = null; this.index = null;
      this.handlers.update?.({ records: this.records, meta: this.meta, changedFrom: 0, prepended: head.count });
    } catch (error) {
      if (!current()) return;
      // A head that does not parse is not this head.
      if (error instanceof SyntaxError) { this.recover(); return; }
      // A network failure (fetch and a dropped body both reject with a TypeError) is retried for as
      // long as it lasts; a server that keeps refusing is given up on, and the session starts over.
      const dropped = error instanceof TypeError;
      if (dropped) head.drops = (head.drops || 0) + 1;
      else head.refusals = (head.refusals || 0) + 1;
      if (head.refusals > HEAD_REFUSALS) { this.recover(); return; }
      const tries = (head.drops || 0) + (head.refusals || 0);
      this.handlers.headFailed?.(error, tries);
      setTimeout(() => { if (current()) this.loadHead(generation, head); }, Math.min(8000, 1000 * tries));
    }
  }

  /** The turn index for the head still to come (#s276): what its records hold, from the server's
   *  map of the log, so the Turns pane, the tool letters and counts, the compaction marks, the
   *  artifact roster and the offered files are whole before the head lands. It answers for the
   *  records below `head.count` exactly, under the head's epoch; a 409 is the head's to handle. */
  async loadIndex(generation, head, tries = 1) {
    const current = () => generation === this.generation && this.head === head;
    // Stage 2 reads the head only through the index: when there is none to be had, the head is
    // read whole after all (stage 1's path), and a stale epoch starts the session over.
    const without = () => { if (current() && this.windowed) this.loadHead(generation, head); };
    try {
      const response = await fetch(`/turn-index?session=${encodeURIComponent(this.session)}&epoch=${head.epoch}&upto=${head.count}`, { cache: "no-store" });
      if (!current()) return;
      if (response.status === 409) { if (this.windowed) this.recover(); return; }
      if (!response.ok) throw new Error(`turn-index HTTP ${response.status}`);
      const index = await response.json();
      if (!current()) return;
      if (index.epoch !== head.epoch || index.upto !== head.count) { without(); return; }
      if (this.windowed && !(Array.isArray(index.facts) && index.facts.length === head.count)) { without(); return; }
      this.index = index;
      this.handlers.indexed?.(index);
      if (this.windowed) this.startReads(generation, head, index);
    } catch (error) {
      if (!current()) return;
      if (tries < INDEX_TRIES) setTimeout(() => this.loadIndex(generation, head, tries + 1), 1000 * tries);
      else without();
    }
  }

  /** Stage 2 (#s277, design/slow-links.md §6.4): the head read turn by turn, in the order the reader
   *  needs it — the turn a jump names, then the turns the window shows, then the rest from the tail
   *  backwards (D4) until the reads hold `keepBytes` (#s285) — instead of whole from the top. Each turn is read from its first record, in
   *  ranges of at most `headPieceBytes` from where its last read ended, which the server answers in
   *  whole records; one read at a time. What a read brings waits in `fills` until the page takes it
   *  (`takeFill`), which it does when the view allows (§6.3). The units of the reads are the index's
   *  turns, clamped to the head, plus whatever comes before the first turn. */
  startReads(generation, head, index) {
    const reads = [];
    const rows = (index.turns || []).filter(row => row.r < head.count);
    const first = rows.length ? rows[0].r : head.count;
    if (first > 0) reads.push({ r: 0, last: first - 1, o: 0, end: rows.length ? rows[0].o : head.len });
    for (const row of rows) reads.push({ r: row.r, last: Math.min(row.last, head.count - 1), o: row.o, end: Math.min(row.end, head.len) });
    for (const read of reads) Object.assign(read, { next: read.r, at: read.o, done: read.o >= read.end, want: Infinity, asked: 0 });
    head.reads = reads;
    head.asked = 0;
    // The bytes of the log the reads hold (#s285), and whether the background read has reached what
    // the page keeps: from then on it is over.
    head.held = 0;
    head.full = false;
    this.readNext(generation, head);
  }

  /** The bytes of the log the head's reads hold now, those waiting in `fills` included (#s285). */
  get held() { return this.head?.held || 0; }

  /** The read that holds record `r`, or null. */
  readFor(r) {
    const reads = this.head?.reads;
    if (!reads) return null;
    let lo = 0, hi = reads.length - 1;
    while (lo < hi) { const mid = (lo + hi + 1) >> 1; if (reads[mid].r <= r) lo = mid; else hi = mid - 1; }
    const read = reads[lo];
    return read && read.r <= r && r <= read.last ? read : null;
  }

  /** The page needs records `[from, to)`: `priority` 0 for a jump, 1 for what the window shows. The
   *  latest asked goes first within a priority. False when none of them is owed. */
  want(from, to, priority) {
    const head = this.head;
    if (!head?.reads) return false;
    let owed = false;
    for (let r = from; r < to;) {
      const read = this.readFor(r);
      if (!read) { r++; continue; }
      if (!read.done) {
        owed = true;
        if (priority <= read.want) { read.want = priority; read.asked = ++head.asked; }
      }
      r = read.last + 1;
    }
    // The next read starts once every ask made with this one is in (a jump's, after the window's it
    // set off): started here, the first ask — the window's farthest — would be read first (#s285: a
    // store whose background read is over is idle when the reader asks).
    if (owed && !head.reading && !head.kicked) {
      head.kicked = true;
      queueMicrotask(() => { head.kicked = false; this.readNext(this.generation, head); });
    }
    return owed;
  }

  /** Every ask not yet read goes back to the background order (#s285): the page asks again after
   *  every transaction for what the window shows and for the jump it still waits on, so a turn the
   *  reader has left, or a jump they gave up, is no longer read for them once the background read is
   *  over. */
  unwant() {
    for (const read of this.head?.reads || []) if (read.want < Infinity) read.want = Infinity;
  }

  readNext(generation, head) {
    const current = () => generation === this.generation && this.head === head;
    if (!current() || head.reading || !head.reads) return;
    let pick = null;
    for (const read of head.reads) {
      if (read.done) continue;
      // Once the page holds what it keeps, the rest is read only where the reader goes (#s285).
      if (head.full && read.want === Infinity) continue;
      if (!pick || read.want < pick.want || (read.want === pick.want && (read.want < Infinity ? read.asked > pick.asked : read.r > pick.r))) pick = read;
    }
    if (!pick) return;
    // The background read ends before the turn that would take the page past what it keeps (#s285):
    // reaching the budget stops it, and only what the reader visits makes the page let go of a turn.
    if (pick.want === Infinity && head.held + pick.end - pick.at > this.keepBytes) { head.full = true; return; }
    head.reading = true;
    head.current = pick;
    this.readPiece(generation, head, pick).finally(() => {
      head.reading = false;
      head.current = null;
      this.readNext(generation, head);
    });
  }

  /** Let go of the records `read` has brought (#s285, design/slow-links.md §6.6): they go back to
   *  `PENDING`, and the turn is read again from its start if the reader comes back to it — a fill like
   *  any other. Never the read in flight, nor one with a piece still waiting in `fills`. Returns the
   *  records let go, `{from, to}`, or null. */
  drop(read) {
    const head = this.head;
    if (!head?.reads || read.next === read.r || head.current === read) return null;
    if (this.fills.some(fill => fill.from <= read.last && fill.from + fill.records.length > read.r)) return null;
    const from = read.r, to = read.next;
    for (let i = from; i < to; i++) this.records[i] = PENDING;
    head.held -= read.at - read.o;
    Object.assign(read, { next: read.r, at: read.o, done: read.o >= read.end, want: Infinity, asked: 0 });
    return { from, to };
  }

  /** One range of `read`'s turn: its whole records wait in `fills`, as each completes (#s274's
   *  resuming reader: a drop keeps every record the read finished, and the next read starts at the
   *  first it did not). A drop is read again for as long as it lasts; a 409, a range that does not
   *  parse, a record that is not the one the index's facts describe there, a turn whose records do
   *  not come out at its count, or a server that keeps refusing (`HEAD_REFUSALS`) starts the session
   *  over. */
  async readPiece(generation, head, read) {
    const current = () => generation === this.generation && this.head === head;
    const before = read.at;
    // Take the whole records `bytes` completes into `fills`; false when they are not this turn's.
    let pending = [], pendingLength = 0;
    const take = bytes => {
      const cut = bytes.lastIndexOf(10);
      if (cut < 0) { pending.push(bytes); pendingLength += bytes.length; return true; }
      const whole = new Uint8Array(pendingLength + cut + 1);
      let at = 0;
      for (const chunk of pending) { whole.set(chunk, at); at += chunk.length; }
      whole.set(bytes.subarray(0, cut + 1), at);
      pending = [bytes.slice(cut + 1)]; pendingLength = bytes.length - cut - 1;
      const records = parseRecords(new TextDecoder().decode(whole));
      if (!records.length || read.next + records.length > read.last + 1) return false;
      // Each record is the one the index described there — its kind and id — or the log was
      // rewritten under the same epoch, and nothing is filled from it (framework I12).
      const facts = this.index?.facts;
      if (facts && records.some((record, k) => { const fact = facts[read.next + k]; return !fact || fact[0] !== record.kind || (fact[1] || "") !== (record.id || ""); })) return false;
      this.fills.push({ from: read.next, records });
      read.next += records.length;
      read.at += whole.length;
      head.held += whole.length;
      // A visit that takes the page past what it keeps ends the background read too: what is let go
      // for it is never read again behind the reader's back.
      if (head.held > this.keepBytes) head.full = true;
      this.handlers.filled?.();
      return true;
    };
    try {
      const len = Math.min(this.headPieceBytes, read.end - read.at);
      const response = await fetch(`/records?${recordsQuery(this.session, { offset: read.at, len }, head.epoch)}`, { cache: "no-store" });
      if (!current()) return;
      if (response.status === 409) { this.recover(); return; }
      if (!response.ok) throw new Error(`records HTTP ${response.status}`);
      if (!response.body) {
        const bytes = new Uint8Array(await response.arrayBuffer());
        if (!current()) return;
        if (!take(bytes)) { this.recover(); return; }
      } else {
        const reader = response.body.getReader();
        for (;;) {
          const { done, value } = await reader.read();
          if (!current()) { reader.cancel().catch(() => {}); return; }
          if (done) break;
          if (!take(value)) { reader.cancel().catch(() => {}); this.recover(); return; }
        }
      }
      if (read.at === before) throw new Error("records: a range brought no whole record");
      if (read.at >= read.end) {
        read.done = true;
        if (read.next !== read.last + 1) { this.recover(); return; }
      }
      head.refusals = 0; head.drops = 0;
    } catch (error) {
      if (!current()) return;
      if (error instanceof SyntaxError) { this.recover(); return; }
      // What the read finished is kept; a record it was part way through is read again.
      if (read.at > before) { head.refusals = 0; head.drops = 0; }
      if (error instanceof TypeError) head.drops = (head.drops || 0) + 1;
      else head.refusals = (head.refusals || 0) + 1;
      if (head.refusals > HEAD_REFUSALS) { this.recover(); return; }
      const tries = (head.drops || 0) + (head.refusals || 0);
      this.handlers.headFailed?.(error, tries);
      await new Promise(resolve => setTimeout(resolve, Math.min(8000, 1000 * tries)));
    }
  }

  /** The next piece read and not yet taken: its record range `[from, to)`, or null. */
  peekFill() {
    const fill = this.fills[0];
    return fill ? { from: fill.from, to: fill.from + fill.records.length } : null;
  }

  /** Take the next piece into the records, in place (record indices never move, #314), and say which
   *  records it filled. Once the last piece of a head read whole is taken, the head and its index go:
   *  every record is here. */
  takeFill() {
    const fill = this.fills.shift();
    if (!fill) return null;
    for (let i = 0; i < fill.records.length; i++) this.records[fill.from + i] = fill.records[i];
    if (!this.fills.length && this.head?.reads?.every(read => read.done)) {
      this.head = null; this.index = null;
      this.handlers.headRead?.();
    }
    return { from: fill.from, to: fill.from + fill.records.length };
  }

  apply(reply, headCount = 0) {
    const plan = reducePull(this.cursor, this.records.length, reply);
    // A resync drops everything, a head still in flight with it: that head belongs to a log the
    // server no longer has. So does a pull that rewrites a record the head still owes, whatever its
    // epoch (#s280): a client ahead of a log that was cut, or one that meets a session resumed from
    // the cache at epoch 1 again, is sent the log from 0 under the SAME epoch, and a head that went
    // on reading would write the old log's records over the new ones when it finished.
    const owed = this.head;
    const cut = !!owed && !headCount && plan.changedFrom < owed.count;
    if ((plan.resync && !headCount) || cut) { this.head = null; this.index = null; this.fills = []; }
    for (const step of plan.steps) {
      if (step.op === "truncate") this.records.length = step.to;
      else this.records.push(...step.records);
    }
    // A placeholder left with no head to fill it would wait for ever: start over instead.
    if (cut && this.records.includes(PENDING)) { this.recover(); return; }
    // The shared plan truncated the empty store to `headCount` before appending the tail, which
    // leaves that many empty slots: they are the head, still to come.
    for (let i = 0; i < headCount; i++) this.records[i] = PENDING;
    this.cursor = plan.cursor;
    if (reply.meta) this.meta = reply.meta;
    // The update gate is what the store must REPAINT (changedFrom), not the classic page's
    // idle rule (both zones empty): a shorter provisional zone with nothing to append is idle
    // by that rule yet truncates the store, and the viewport must hear it.
    // The placeholders a cut replaced are the head landing, to the page (#314): it is reported as
    // one, so the reader keeps the turn they were on and the change waits for a still view (#340).
    if (plan.changedFrom !== Infinity || reply.meta) this.handlers.update?.({ records: this.records, meta: this.meta, changedFrom: plan.changedFrom === Infinity ? this.records.length : plan.changedFrom, ...(cut ? { prepended: owed.count } : {}) });
  }
}
