"use strict";

// ELO ladder tab and the Calendar tab: availability editing, match cards,
// standings, and the month grid. Shares state with app-core.js.
let availSet = new Set();   // slot ids I've toggled on (edit buffer)
let availDirty = false;     // unsaved availability edits pending
let calYear = null, calMonth = null; // month shown in the Calendar tab grid

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

  // Weekly target (don't clobber the field while the user is editing it).
  const gpw = $("l-gpw");
  if (gpw && document.activeElement !== gpw) gpw.value = ladder.my_games_per_week || 0;
  if (gpw) gpw.max = ladder.max_games_per_week;
  $("l-gpw-max").textContent = `/ ${ladder.max_games_per_week} max`;

  // Availability: re-sync from the server unless there are unsaved edits.
  if (!availDirty) availSet = new Set(ladder.my_availability || []);
  renderCalendar("l-calendar", { editable: true });
  renderMyMatches();
  renderAllMatches();
  renderMonthCalendar(); // the Calendar tab's month grid depends on ladder data
  renderTodo();          // the schedule section depends on ladder data

  // ELO standings.
  const tb = $("t-standings").querySelector("tbody");
  tb.innerHTML = "";
  (ladder.standings || []).forEach((s) => {
    const tr = document.createElement("tr");
    if (state && s.player === state.me) tr.className = "mine";
    tr.innerHTML =
      `<td>${s.rank}</td><td>${esc(s.name)}${state && s.player === state.me ? " ★" : ""}</td>` +
      `<td class="num">${s.elo}</td><td class="num">${s.wins}-${s.losses}-${s.draws}</td><td class="num">${s.scheduled}</td>`;
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
      const game = games.get(s.slot);
      const name = blockName(s.block, nb);
      const label = name ? `<b>${name}</b> <span class="cal-time">${localTimeLabel(s.start)}</span>` : localTimeLabel(s.start);
      if (editable) {
        html += `<button class="cal-chip${on ? " on" : ""}" ${past ? "disabled" : `data-slot="${s.slot}"`}>${label}</button>`;
      } else {
        const mark = game ? ` <span class="cal-game" title="game vs ${esc(game)}">🎲</span>` : "";
        html += `<span class="cal-chip${on ? " on" : ""}${game ? " game" : ""}${past ? " past" : ""}">${label}${mark}</span>`;
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
      const avail = (availByDay.get(key) || []).slice().sort((a, b) => a - b);
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
    : `<p class="muted">No matches yet. Set your availability and games per week, and the system will schedule them.</p>`;
}

function matchCard(m, me) {
  const iAmA = m.a === me;
  const opp = iAmA ? m.b_name : m.a_name;
  const myW = iAmA ? m.a_wins : m.b_wins, oppW = iAmA ? m.b_wins : m.a_wins;
  const head = `<div class="matchhead"><b>vs ${esc(opp)}</b> <span class="muted">${fmtSlot(m.slot_start)}</span></div>`;

  if (m.status === "completed") {
    const delta = iAmA ? m.a_delta : m.b_delta;
    const verdict = myW > oppW ? "won" : myW < oppW ? "lost" : "drew";
    return `<div class="matchcard">${head}<span class="muted">final ${myW}–${oppW} · ${verdict} · ELO ${delta >= 0 ? "+" : ""}${delta}</span></div>`;
  }
  if (m.status === "cancelled") {
    const byMe = m.cancelled_by === me;
    const delta = iAmA ? m.a_delta : m.b_delta;
    return `<div class="matchcard">${head}<span class="muted">cancelled ${byMe ? `by you (ELO ${delta})` : "by opponent"}</span></div>`;
  }
  if (m.status === "expired") {
    return `<div class="matchcard">${head}<span class="muted">expired — no result was reported in time</span></div>`;
  }

  // Scheduled: result entry (pre-filled from any proposal) + confirm + cancel.
  const pw = m.proposed_by != null ? myW : 2, ow = m.proposed_by != null ? oppW : 0, dw = m.proposed_by != null ? m.draws : 0;
  const form =
    `<span class="report-row">` +
    `<input class="lm-yw" type="number" min="0" value="${pw}" data-mid="${m.id}" title="your game wins" /> – ` +
    `<input class="lm-ow" type="number" min="0" value="${ow}" data-mid="${m.id}" title="${esc(opp)} game wins" />` +
    `<input class="lm-dw" type="number" min="0" value="${dw}" data-mid="${m.id}" title="draws" />` +
    `<button class="buy lm-report" data-mid="${m.id}" data-a="${iAmA ? 1 : 0}">Report</button></span>`;
  let note = `<div class="muted">Report your result:</div>`, confirmBtn = "";
  if (m.proposed_by === me) {
    note = `<div class="muted">You reported ${myW}–${oppW}; waiting for ${esc(opp)} to confirm.</div>`;
  } else if (m.proposed_by != null) {
    note = `<div class="muted">${esc(opp)} reported ${myW}–${oppW}. Confirm or counter:</div>`;
    confirmBtn = `<button class="buy lm-confirm" data-mid="${m.id}">Confirm ${myW}–${oppW}</button> `;
  }
  return `<div class="matchcard">${head}${note}<div class="actrow">${confirmBtn}<button class="sell lm-cancel" data-mid="${m.id}">Cancel</button></div>${form}</div>`;
}

// All matches (read-only overview).
function renderAllMatches() {
  const box = $("l-allmatches");
  const ms = (ladder.matches || []).slice().sort((a, b) => a.slot_start - b.slot_start);
  if (!ms.length) { box.innerHTML = `<p class="muted">No matches scheduled yet.</p>`; return; }
  box.innerHTML =
    `<table class="grid"><thead><tr><th>When</th><th>Match</th><th class="num">Result</th></tr></thead><tbody>` +
    ms.map((m) => {
      const mine = state && (m.a === state.me || m.b === state.me) ? ' class="mine"' : "";
      const res = m.status === "completed" ? `${m.a_wins}–${m.b_wins}`
        : m.status === "cancelled" ? `<span class="muted">cancelled</span>`
          : m.status === "expired" ? `<span class="muted">expired</span>`
            : m.proposed_by != null ? `<span class="muted">reported</span>` : `<span class="muted">scheduled</span>`;
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
  const rep = e.target.closest(".lm-report");
  if (rep) {
    const mid = Number(rep.dataset.mid), iAmA = rep.dataset.a === "1";
    const v = (cls) => Math.max(0, Number($("l-mymatches").querySelector(`.${cls}[data-mid="${mid}"]`).value) || 0);
    const yw = v("lm-yw"), ow = v("lm-ow"), dw = v("lm-dw");
    const body = { match_id: mid, a_wins: iAmA ? yw : ow, b_wins: iAmA ? ow : yw, draws: dw };
    try { await api("/api/ladder/report", "POST", body); await refresh(); } catch (err) { toastError(err.message); }
    return;
  }
  const cf = e.target.closest(".lm-confirm");
  if (cf) {
    try { await api("/api/ladder/confirm", "POST", { match_id: Number(cf.dataset.mid) }); await refresh(); } catch (err) { toastError(err.message); }
    return;
  }
  const cx = e.target.closest(".lm-cancel");
  if (cx) {
    if (!confirm("Cancel this match? You'll take an ELO penalty.")) return;
    try { await api("/api/ladder/cancel", "POST", { match_id: Number(cx.dataset.mid) }); await refresh(); } catch (err) { toastError(err.message); }
  }
});
