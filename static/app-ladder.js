"use strict";

// ELO ladder tab and the Calendar tab: availability editing, match cards,
// standings, and the month grid. Shares state with app-core.js.
let availSet = new Set();   // slot ids I've toggled on (edit buffer)
let availDirty = false;     // unsaved availability edits pending
let recurSet = new Set();   // recurring weekly-slot ids toggled on (edit buffer)
let recurDirty = false;     // unsaved recurring-pattern edits pending
let calYear = null, calMonth = null; // month shown in the Calendar tab grid

// The recurring "weekly slot" a concrete slot maps to (must match the server's
// `weekly_slot`): weekday·blocks + block, weekday 0 = Sunday.
function weeklySlot(slot, nb) {
  const weekday = ((Math.floor(slot / nb) + 4) % 7 + 7) % 7;
  const block = ((slot % nb) + nb) % nb;
  return weekday * nb + block;
}

// ---- ELO ladder ----
// All times are rendered in the viewer's local timezone (slots are UTC instants
// server-side; only the display shifts).
function fmtSlot(epoch) {
  return new Date(epoch * 1000).toLocaleString(undefined, { weekday: "short", month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
}
function localDayKey(epoch) {
  const d = new Date(epoch * 1000);
  return `${d.getFullYear()}-${d.getMonth()}-${d.getDate()}`;
}
function localDayLabel(epoch) {
  return new Date(epoch * 1000).toLocaleDateString(undefined, { weekday: "short", month: "short", day: "numeric" });
}
function localTimeLabel(epoch) {
  return new Date(epoch * 1000).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
}

function renderLadder() {
  if (!ladder) return;
  const isLeagueGame = state && state.mode === "league";

  // League matches are assigned with deadlines — no availability, weekly
  // targets, or calendar slots to manage.
  $("l-prefs").hidden = isLeagueGame;
  $("l-league-note").hidden = !isLeagueGame;
  if (!isLeagueGame) {
    // Weekly target (don't clobber the field while the user is editing it).
    const gpw = $("l-gpw");
    if (gpw && document.activeElement !== gpw) gpw.value = ladder.my_games_per_week || 0;
    if (gpw) gpw.max = ladder.max_games_per_week;
    $("l-gpw-max").textContent = `/ ${ladder.max_games_per_week} max`;

    // Availability: re-sync from the server unless there are unsaved edits.
    if (!availDirty) availSet = new Set(ladder.my_availability || []);
    if (!recurDirty) recurSet = new Set(ladder.my_recurring || []);
    renderRecurringGrid();
    renderCalendar("l-calendar", { editable: true });
  }
  renderMyMatches();
  renderAllMatches();
  renderMonthCalendar(); // the Calendar tab's month grid depends on ladder data
  renderTodo();          // the schedule section depends on ladder data

  // Standings (league games rank by swiss points; standard games by ELO —
  // the server sends them pre-sorted either way).
  const league = state && state.mode === "league";
  $("t-standings").querySelector("thead tr").innerHTML =
    `<th>#</th><th>Player</th>` +
    (league ? `<th class="num" title="3 per match win, 1 per draw">Pts</th>` : `<th class="num">ELO</th>`) +
    `<th class="num">W-L-D</th>` +
    (league ? `<th class="num" title="opponents' match-win % (strength of schedule)">OMW%</th>` : "") +
    (league ? `<th class="num" title="individual games won-lost">Games</th>` : "") +
    `<th class="num" title="upcoming matches">Sched</th>`;
  const tb = $("t-standings").querySelector("tbody");
  tb.innerHTML = "";
  (ladder.standings || []).forEach((s) => {
    const tr = document.createElement("tr");
    if (state && s.player === state.me) tr.className = "mine";
    tr.innerHTML =
      `<td>${s.rank}</td><td>${esc(s.name)}${state && s.player === state.me ? " ★" : ""}</td>` +
      `<td class="num">${league ? s.points : s.elo}</td><td class="num">${s.wins}-${s.losses}-${s.draws}</td>` +
      (league ? `<td class="num">${(s.omw * 100).toFixed(1)}</td>` : "") +
      (league ? `<td class="num">${s.game_wins}-${s.game_losses}</td>` : "") +
      `<td class="num">${s.scheduled}</td>`;
    tb.appendChild(tr);
  });
}

// Name the daily blocks. With two blocks they're the morning and evening slots;
// any other count just shows the clock time (the label is dropped).
function blockName(block, nb) {
  if (nb === 2) return block === 0 ? "Morning" : "Evening";
  if (nb === 1) return "Anytime";
  return "";
}

// The recurring weekly-availability grid: 7 weekday rows × one column per block,
// each cell a toggle bound to the recurring edit buffer. Set once, applies every
// week. Block columns are labelled with their local clock time so it reads in the
// viewer's timezone (weekdays follow the slot frame, which lines up for viewers
// in/near the league timezone — the same grouping the calendar below uses).
function renderRecurringGrid() {
  const box = $("l-recurring");
  if (!box) return;
  if (!(state && state.me != null)) { box.innerHTML = `<p class="muted">Log in to set your weekly pattern.</p>`; return; }
  if (!ladder) { box.innerHTML = `<p class="muted">Loading…</p>`; return; }
  const blocks = ladder.blocks || [9, 21];
  const nb = blocks.length;
  const now = ladder.server_now || Math.floor(Date.now() / 1000);
  const todayUtc = Math.floor(now / 86400);
  const dow = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
  const blockLabel = (b) => {
    const nm = blockName(b, nb);
    const t = localTimeLabel(todayUtc * 86400 + blocks[b] * 3600);
    return nm ? `${nm} <span class="cal-time">${t}</span>` : t;
  };
  let html = `<table class="cal recur-grid"><thead><tr><th></th>`;
  for (let b = 0; b < nb; b++) html += `<th>${blockLabel(b)}</th>`;
  html += `</tr></thead><tbody>`;
  for (let wd = 0; wd < 7; wd++) {
    html += `<tr><td class="cal-day">${dow[wd]}</td>`;
    for (let b = 0; b < nb; b++) {
      const w = wd * nb + b;
      html += `<td><button class="cal-chip recur-cell${recurSet.has(w) ? " on" : ""}" data-wslot="${w}">${recurSet.has(w) ? "✓" : ""}</button></td>`;
    }
    html += `</tr>`;
  }
  box.innerHTML = html + `</tbody></table>`;
}

// Local-midnight epoch of the Sunday starting the week that contains `epoch`.
function startOfLocalWeek(epoch) {
  const d = new Date(epoch * 1000);
  d.setHours(0, 0, 0, 0);
  d.setDate(d.getDate() - d.getDay()); // back up to Sunday
  return Math.floor(d.getTime() / 1000);
}

// Availability / schedule calendar: one row per local day, a time chip per slot.
// Slots are grouped by their *local* day so the grid reads correctly in any
// timezone (a 21:00 UTC slot can land on the next local morning, etc.).
//
// `editable` (the Ladder tab) renders clickable chips bound to the edit buffer.
// Read-only (Home, TODO) highlights your saved availability, marks scheduled
// games, and — with `fromWeekStart` — begins at the start of the current week.
function renderCalendar(targetId = "l-calendar", { editable = true, fromWeekStart = false } = {}) {
  const cal = $(targetId);
  if (!cal) return;
  if (!(state && state.me != null)) { cal.innerHTML = `<p class="muted">Log in to see your calendar.</p>`; return; }
  if (!ladder) { cal.innerHTML = `<p class="muted">Loading schedule…</p>`; return; }
  const blocks = ladder.blocks || [9, 21];
  const nb = blocks.length;
  const days = ladder.window_days || 14;
  const now = ladder.server_now || Math.floor(Date.now() / 1000);
  const todayUtc = Math.floor(now / 86400);
  const avail = editable ? availSet : new Set(ladder.my_availability || []);
  const recur = editable ? recurSet : new Set(ladder.my_recurring || []);

  // Your scheduled games, keyed by slot id, so they can be marked on the grid.
  const me = state.me;
  const games = new Map();
  (ladder.matches || []).forEach((m) => {
    if ((m.a === me || m.b === me) && m.status === "scheduled") games.set(m.slot, m.a === me ? m.b_name : m.a_name);
  });

  // Candidate slots, padded a week back so a from-week-start view is complete
  // near the window edge regardless of UTC offset.
  const slots = [];
  for (let d = -7; d <= days + 1; d++) {
    for (let b = 0; b < nb; b++) {
      const slot = (todayUtc + d) * nb + b;
      slots.push({ slot, block: b, start: (todayUtc + d) * 86400 + blocks[b] * 3600 });
    }
  }
  const byDay = new Map();
  for (const s of slots) {
    const key = localDayKey(s.start);
    if (!byDay.has(key)) byDay.set(key, { repr: s.start, items: [] });
    byDay.get(key).items.push(s);
  }
  const ordered = [...byDay.values()].sort((a, b) => a.repr - b.repr);
  const anchorKey = localDayKey(fromWeekStart ? startOfLocalWeek(now) : now);
  const startIdx = Math.max(0, ordered.findIndex((d) => localDayKey(d.repr) === anchorKey));
  const visible = ordered.slice(startIdx, startIdx + days);

  let html = `<table class="cal${editable ? "" : " cal-static"}"><tbody>`;
  for (const day of visible) {
    html += `<tr><td class="cal-day">${localDayLabel(day.repr)}</td><td>`;
    day.items.sort((a, b) => a.start - b.start).forEach((s) => {
      const past = s.start <= now;
      const on = avail.has(s.slot);
      const covered = recur.has(weeklySlot(s.slot, nb));
      const game = games.get(s.slot);
      const name = blockName(s.block, nb);
      const cov = covered ? ` <span class="recur-mark" title="covered by your weekly pattern">◇</span>` : "";
      const label = name ? `<b>${name}</b> <span class="cal-time">${localTimeLabel(s.start)}</span>` : localTimeLabel(s.start);
      if (editable) {
        html += `<button class="cal-chip${on ? " on" : ""}${covered ? " covered" : ""}" ${past ? "disabled" : `data-slot="${s.slot}"`}>${label}${cov}</button>`;
      } else {
        const mark = game ? ` <span class="cal-game" title="game vs ${esc(game)}">🎲</span>` : "";
        html += `<span class="cal-chip${on || covered ? " on" : ""}${game ? " game" : ""}${past ? " past" : ""}">${label}${cov}${mark}</span>`;
      }
    });
    html += `</td></tr>`;
  }
  cal.innerHTML = html + `</tbody></table>`;
}

// A proper month grid for the Calendar tab: one cell per day, with a dot per
// block you're free (M/E) and 🎲 for scheduled games. Prev / next / today
// navigate months so you can look ahead to next month at a glance.
function renderMonthCalendar() {
  const box = $("cal-month");
  if (!box) return;
  if (!(state && state.me != null)) { box.innerHTML = `<p class="muted">Log in to see your calendar.</p>`; return; }
  if (!ladder) { box.innerHTML = `<p class="muted">Loading schedule…</p>`; return; }
  const blocks = ladder.blocks || [9, 21];
  const nb = blocks.length;
  const now = ladder.server_now || Math.floor(Date.now() / 1000);
  if (calYear == null) { const t = new Date(now * 1000); calYear = t.getFullYear(); calMonth = t.getMonth(); }
  const me = state.me;

  // Bucket your availability and scheduled games by local day.
  const availByDay = new Map();
  (ladder.my_availability || []).forEach((slot) => {
    const block = ((slot % nb) + nb) % nb;
    const start = Math.floor(slot / nb) * 86400 + blocks[block] * 3600;
    const key = localDayKey(start);
    if (!availByDay.has(key)) availByDay.set(key, []);
    availByDay.get(key).push(block);
  });
  const recurSaved = new Set(ladder.my_recurring || []);
  const gamesByDay = new Map();
  (ladder.matches || []).forEach((m) => {
    if ((m.a === me || m.b === me) && m.status === "scheduled") {
      const key = localDayKey(m.slot_start);
      if (!gamesByDay.has(key)) gamesByDay.set(key, []);
      gamesByDay.get(key).push({ opp: m.a === me ? m.b_name : m.a_name, start: m.slot_start });
    }
  });

  const first = new Date(calYear, calMonth, 1);
  const monthLabel = first.toLocaleDateString(undefined, { month: "long", year: "numeric" });
  const daysInMonth = new Date(calYear, calMonth + 1, 0).getDate();
  const startWeekday = first.getDay(); // 0 = Sunday
  const todayKey = localDayKey(now);
  const dow = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

  let html =
    `<div class="cal-month-head">` +
      `<button type="button" class="cbtn cal-prev" title="previous month">‹</button>` +
      `<b class="cal-month-label">${monthLabel}</b>` +
      `<button type="button" class="cbtn cal-next" title="next month">›</button>` +
      `<button type="button" class="cbtn cal-today" title="jump to this month">Today</button>` +
    `</div>` +
    `<table class="cal-grid"><thead><tr>${dow.map((d) => `<th>${d}</th>`).join("")}</tr></thead><tbody>`;

  let dayNum = 1;
  for (let week = 0; dayNum <= daysInMonth; week++) {
    html += "<tr>";
    for (let col = 0; col < 7; col++) {
      const idx = week * 7 + col;
      if (idx < startWeekday || dayNum > daysInMonth) { html += `<td class="cal-empty"></td>`; continue; }
      const cell = new Date(calYear, calMonth, dayNum);
      const key = `${cell.getFullYear()}-${cell.getMonth()}-${cell.getDate()}`;
      // Effective availability = explicit slots ∪ recurring pattern for this weekday.
      const freeBlocks = new Set(availByDay.get(key) || []);
      for (let b = 0; b < nb; b++) if (recurSaved.has(cell.getDay() * nb + b)) freeBlocks.add(b);
      const avail = [...freeBlocks].sort((a, b) => a - b);
      const gms = (gamesByDay.get(key) || []).slice().sort((a, b) => a.start - b.start);
      const isToday = key === todayKey;
      let marks = avail.map((b) => {
        const nm = blockName(b, nb);
        return `<span class="cal-dot avail" title="free ${esc(nm || "this block")}">${esc((nm || "•")[0])}</span>`;
      }).join("");
      marks += gms.map((g) => `<span class="cal-dot game" title="game vs ${esc(g.opp)} · ${localTimeLabel(g.start)}">🎲</span>`).join("");
      html += `<td class="cal-cell${isToday ? " today" : ""}"><div class="cal-date">${dayNum}</div>` +
        (marks ? `<div class="cal-marks">${marks}</div>` : "") + `</td>`;
      dayNum++;
    }
    html += "</tr>";
  }
  box.innerHTML = html + `</tbody></table>`;
}

// Move the Calendar tab's month grid (delta months; 0 = back to this month).
function shiftCalMonth(delta) {
  const now = (ladder && ladder.server_now) || Math.floor(Date.now() / 1000);
  const t = new Date(now * 1000);
  if (calYear == null || delta === 0) { calYear = t.getFullYear(); calMonth = t.getMonth(); }
  if (delta) { const d = new Date(calYear, calMonth + delta, 1); calYear = d.getFullYear(); calMonth = d.getMonth(); }
  renderMonthCalendar();
}

// The logged-in player's own matches, with report / confirm / cancel controls.
function renderMyMatches() {
  const box = $("l-mymatches");
  const me = state ? state.me : null;
  if (me == null) { box.innerHTML = `<p class="muted">Log in to see your matches.</p>`; return; }
  const mine = (ladder.matches || []).filter((m) => m.a === me || m.b === me).sort((x, y) => x.slot_start - y.slot_start);
  box.innerHTML = mine.length
    ? mine.map((m) => matchCard(m, me)).join("")
    : state && state.mode === "league"
    ? `<p class="muted">No matches yet — they're assigned automatically once matchmaking opens.</p>`
    : `<p class="muted">No matches yet. Set your availability and games per week, and the system will schedule them.</p>`;
}

function matchCard(m, me) {
  const iAmA = m.a === me;
  const opp = iAmA ? m.b_name : m.a_name;
  const myW = iAmA ? m.a_wins : m.b_wins, oppW = iAmA ? m.b_wins : m.a_wins;
  // League matches carry a play-by deadline rather than a scheduled time.
  const when = state && state.mode === "league"
    ? (m.status === "scheduled" ? `play by ${fmtSlot(m.slot_start)}` : localDayLabel(m.slot_start))
    : fmtSlot(m.slot_start);
  const head = `<div class="matchhead"><b>vs ${esc(opp)}</b> <span class="muted">${when}</span></div>`;

  if (m.status === "completed") {
    const verdict = myW > oppW ? `you won ${myW}-${oppW}` : myW < oppW ? `${esc(opp)} won ${oppW}-${myW}` : "draw";
    // League scores swiss match points (3 win / 1 draw); standard games ELO.
    const score = state && state.mode === "league"
      ? `+${myW > oppW ? 3 : myW === oppW ? 1 : 0} pts`
      : (() => { const d = iAmA ? m.a_delta : m.b_delta; return `ELO ${d >= 0 ? "+" : ""}${d}`; })();
    return `<div class="matchcard">${head}<span class="muted">${verdict} · ${score}</span></div>`;
  }
  if (m.status === "cancelled") {
    const byMe = m.cancelled_by === me;
    const delta = iAmA ? m.a_delta : m.b_delta;
    return `<div class="matchcard">${head}<span class="muted">cancelled ${byMe ? `by you (ELO ${delta})` : "by opponent"}</span></div>`;
  }
  // Scheduled (or a legacy "expired" no-show): click the result — it's final
  // immediately (either player can enter it), and can be added any time after
  // the match was played.
  const league = state && state.mode === "league";
  const winBtn = (aw, bw, dw, cls, label) =>
    `<button class="${cls} lm-win" data-mid="${m.id}" data-aw="${aw}" data-bw="${bw}" data-dw="${dw}" data-who="${esc(label)}">${esc(label)}</button>`;
  const form = league
    ? `<div class="report-row winrow">` +
      winBtn(iAmA ? 2 : 0, iAmA ? 0 : 2, 0, "buy", "You won 2-0") +
      winBtn(iAmA ? 2 : 1, iAmA ? 1 : 2, 0, "buy", "You won 2-1") +
      winBtn(iAmA ? 1 : 2, iAmA ? 2 : 1, 0, "sell", `${opp} won 2-1`) +
      winBtn(iAmA ? 0 : 2, iAmA ? 2 : 0, 0, "sell", `${opp} won 2-0`) +
      winBtn(1, 1, 1, "ghost", "Draw") +
      `</div>`
    : `<div class="report-row winrow">` +
      winBtn(iAmA ? 1 : 0, iAmA ? 0 : 1, 0, "buy", "You won") +
      winBtn(iAmA ? 0 : 1, iAmA ? 1 : 0, 0, "buy", `${opp} won`) +
      winBtn(0, 0, 1, "ghost", "Draw") +
      `</div>`;
  const note = `<div class="muted">Click the result — it's final once you report (either player can enter it${league ? "; best of three, and a 2-0 counts for more than a 2-1" : ""}).</div>`;
  // League matches can't be cancelled — they're played or they count as ties.
  const cancel = league || m.status === "expired" ? "" : `<div class="actrow"><button class="sell lm-cancel" data-mid="${m.id}">Cancel</button></div>`;
  return `<div class="matchcard">${head}${note}${form}${cancel}</div>`;
}

// All matches (read-only overview).
function renderAllMatches() {
  const box = $("l-allmatches");
  const ms = (ladder.matches || []).slice().sort((a, b) => a.slot_start - b.slot_start);
  if (!ms.length) { box.innerHTML = `<p class="muted">No matches scheduled yet.</p>`; return; }
  const lg = state && state.mode === "league";
  box.innerHTML =
    `<table class="grid"><thead><tr><th>${lg ? "Play by" : "When"}</th><th>Match</th><th class="num">Result</th></tr></thead><tbody>` +
    ms.map((m) => {
      const mine = state && (m.a === state.me || m.b === state.me) ? ' class="mine"' : "";
      const res = m.status === "completed" ? esc(matchResult(m))
        : m.status === "cancelled" ? `<span class="muted">cancelled</span>`
          : m.status === "expired" ? `<span class="muted">unreported</span>`
            : `<span class="muted">scheduled</span>`;
      return `<tr${mine}><td>${fmtSlot(m.slot_start)}</td><td>${esc(m.a_name)} <span class="muted">vs</span> ${esc(m.b_name)}</td><td class="num">${res}</td></tr>`;
    }).join("") + `</tbody></table>`;
}

// ---- ladder actions ----
$("l-calendar").addEventListener("click", (e) => {
  const chip = e.target.closest(".cal-chip");
  if (!chip || !chip.dataset.slot) return;
  const slot = Number(chip.dataset.slot);
  if (availSet.has(slot)) availSet.delete(slot); else availSet.add(slot);
  availDirty = true;
  chip.classList.toggle("on");
});

$("l-recurring").addEventListener("click", (e) => {
  const cell = e.target.closest(".recur-cell");
  if (!cell) return;
  const w = Number(cell.dataset.wslot);
  if (recurSet.has(w)) recurSet.delete(w); else recurSet.add(w);
  recurDirty = true;
  cell.classList.toggle("on");
  cell.textContent = recurSet.has(w) ? "✓" : "";
  renderCalendar("l-calendar", { editable: true }); // reflect new coverage marks
});

$("l-recurring-save").onclick = async () => {
  try {
    await api("/api/ladder/recurring", "POST", { slots: [...recurSet] });
    recurDirty = false;
    $("l-recurring-msg").textContent = "Weekly pattern saved.";
    await refresh();
  } catch (e) { toastError(e.message); }
};

// Calendar tab: month navigation.
$("cal-month").addEventListener("click", (e) => {
  if (e.target.closest(".cal-prev")) shiftCalMonth(-1);
  else if (e.target.closest(".cal-next")) shiftCalMonth(1);
  else if (e.target.closest(".cal-today")) shiftCalMonth(0);
});

$("l-avail-save").onclick = async () => {
  try {
    await api("/api/ladder/availability", "POST", { slots: [...availSet] });
    availDirty = false;
    $("l-prefs-msg").textContent = "Availability saved.";
    await refresh();
  } catch (e) { toastError(e.message); }
};

$("l-gpw-save").onclick = async () => {
  try {
    await api("/api/ladder/games", "POST", { games_per_week: Math.max(0, Number($("l-gpw").value) || 0) });
    $("l-prefs-msg").textContent = "Weekly target saved.";
    await refresh();
  } catch (e) { toastError(e.message); }
};

$("l-mymatches").addEventListener("click", async (e) => {
  const win = e.target.closest(".lm-win");
  if (win) {
    const who = win.dataset.who;
    // The result is final once reported (no opponent confirmation), so confirm.
    const prompt = who === "draw" ? "Record this match as a draw?" : `Record ${who === "You" ? "yourself" : who} as the winner?`;
    if (!confirm(`${prompt} This is final — the host can correct a mistake.`)) return;
    const body = { match_id: Number(win.dataset.mid), a_wins: Number(win.dataset.aw), b_wins: Number(win.dataset.bw), draws: Number(win.dataset.dw) };
    try { await api("/api/ladder/report", "POST", body); await refresh(); } catch (err) { toastError(err.message); }
    return;
  }
  const cx = e.target.closest(".lm-cancel");
  if (cx) {
    if (!confirm("Cancel this match? You'll take an ELO penalty.")) return;
    try { await api("/api/ladder/cancel", "POST", { match_id: Number(cx.dataset.mid) }); await refresh(); } catch (err) { toastError(err.message); }
  }
});
