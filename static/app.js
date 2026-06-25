"use strict";

const TOKEN_KEY = "mtg_auction_token";
const WANTS_KEY = "mtg_auction_wants";
const UI_KEY = "mtg_auction_ui";

let authToken = localStorage.getItem(TOKEN_KEY) || "";
let state = null;
let cardById = {};
let myQty = {};                 // card id -> copies I hold
let myBidByCard = {}, myOfferByCard = {};
let lastClearByCard = {};       // card id -> last cleared price (cents)
let latestClearByCard = {};     // card id -> {round, best_bid, best_offer, cleared, volume}
let clearHistByCard = {};       // card id -> [{round, price}]
let wants = loadWants();
let activeTab = "inventory";
let planSortKey = "rarity", planSortDir = -1;
let timerDeadline = null, clockSkew = 0;
let modalCardId = null;
let prevBalance = null, prevHistoryLen = null;
let uiRestored = false;
let ladder = null;
let availSet = new Set();   // slot ids I've toggled on (edit buffer)
let availDirty = false;     // unsaved availability edits pending

const $ = (id) => document.getElementById(id);
const $$ = (sel) => Array.from(document.querySelectorAll(sel));
const RARITY_RANK = { common: 0, uncommon: 1, rare: 2, mythic: 3 };
const RARITIES = ["common", "uncommon", "rare", "mythic"];
const KNOWN_TYPES = ["Creature", "Planeswalker", "Instant", "Sorcery", "Artifact", "Enchantment", "Land", "Battle", "Kindred"];

// ---- helpers ----
function fmtUSD(cents) {
  if (cents === null || cents === undefined) return "—";
  const neg = cents < 0, v = Math.abs(cents);
  return (neg ? "-$" : "$") + Math.floor(v / 100) + "." + String(v % 100).padStart(2, "0");
}
// Parse a dollar string into integer cents without going through a binary
// float, so e.g. "1.005" rounds to 101, not 100. Invalid input yields 0.
function toCents(d) {
  const m = String(d).trim().match(/^(\d*)(?:\.(\d*))?$/);
  if (!m || (!m[1] && !m[2])) return 0;
  const frac = (m[2] || "").padEnd(2, "0");
  const round = (m[2] || "").charCodeAt(2) >= 53 ? 1 : 0; // 3rd digit ≥ "5"
  return (m[1] ? parseInt(m[1], 10) : 0) * 100 + parseInt(frac.slice(0, 2), 10) + round;
}
function fmtMV(cmc) { return cmc === null || cmc === undefined ? "—" : String(cmc); }
function shortType(tl) { if (!tl) return "—"; const i = tl.indexOf("—"); return (i >= 0 ? tl.slice(0, i) : tl).trim(); }
function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}
const esc = escapeHtml;
function mineOf(c) { return myQty[c.id] || 0; }
function loadWants() { try { return new Set(JSON.parse(localStorage.getItem(WANTS_KEY) || "[]")); } catch { return new Set(); } }
function saveWants() { localStorage.setItem(WANTS_KEY, JSON.stringify([...wants])); }
function star(name) { return wants.has(name) ? "★" : "☆"; }
function defaultPriceCents(id) {
  const c = cardById[id];
  return lastClearByCard[id] ?? (c && c.ref_price) ?? 100;
}

async function api(path, method = "GET", body = null) {
  const opts = { method, headers: {} };
  if (authToken) opts.headers["X-Token"] = authToken;
  if (body !== null) { opts.headers["Content-Type"] = "application/json"; opts.body = JSON.stringify(body); }
  const res = await fetch(path, opts);
  const data = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(data.error || `request failed (${res.status})`);
  return data;
}

async function refresh() {
  try {
    state = await api("/api/state");
    render();
    if (!uiRestored) { restoreUi(); uiRestored = true; renderPlan(); renderGallery(); }
    refreshLadder();
  } catch (e) { console.error(e); }
}

// Ladder data (standings, matches, the calendar shape, and — for the logged-in
// player — their own availability/target); fetched alongside state.
async function refreshLadder() {
  try { ladder = await api("/api/ladder"); renderLadder(); } catch (e) { console.error(e); }
}

// ---- derived maps ----
function buildMaps(meView) {
  myQty = {}; myBidByCard = {}; myOfferByCard = {};
  if (meView) meView.holdings.forEach((h) => (myQty[h.card] = h.qty));
  (state.my_bids || []).forEach((o) => (myBidByCard[o.card] = o));
  (state.my_offers || []).forEach((o) => (myOfferByCard[o.card] = o));

  lastClearByCard = {}; latestClearByCard = {}; clearHistByCard = {};
  (state.history || []).forEach((r) => {
    (r.clears || []).forEach((cl) => {
      latestClearByCard[cl.card] = { round: r.round, ...cl };
      if (cl.cleared !== null && cl.cleared !== undefined) {
        lastClearByCard[cl.card] = cl.cleared;
        (clearHistByCard[cl.card] = clearHistByCard[cl.card] || []).push({ round: r.round, price: cl.cleared });
      }
    });
  });
}

// ---- top-level render ----
function render() {
  if (!state) return;
  cardById = {};
  state.cards.forEach((c) => (cardById[c.id] = c));

  const inGame = state.phase !== "setup";
  const loggedIn = state.me !== null && state.me !== undefined;
  const meView = loggedIn ? state.players.find((p) => p.id === state.me) : null;
  buildMaps(meView);

  if (!inGame) $("status").textContent = "No game in progress.";
  else if (state.phase === "finished") $("status").textContent = `${state.set_name} — game over after ${state.total_rounds} rounds.`;
  else $("status").textContent = `${state.set_name} — round ${state.round} of ${state.total_rounds} — debt limit ${fmtUSD(state.debt_limit)}`;

  // Per-round results toast when a new round closes.
  const histLen = state.history.length;
  if (prevHistoryLen !== null && histLen > prevHistoryLen && loggedIn) roundToast(state.history[histLen - 1]);
  prevHistoryLen = histLen;

  renderAuth(inGame, loggedIn);
  $("no-game").classList.toggle("hidden", inGame);
  $("game").classList.toggle("hidden", !inGame);
  if (!inGame) return;

  // Balance + funds, with a flash when the balance changes (a trade settled).
  $("me-balance").textContent = meView ? fmtUSD(meView.balance) : "";
  if (meView) {
    if (prevBalance !== null && meView.balance !== prevBalance) flash($("me-balance"));
    prevBalance = meView.balance;
  } else prevBalance = null;
  $("me-funds").textContent = meView
    ? `Committed ${fmtUSD(state.my_committed)} · Available to bid ${fmtUSD(state.my_available)}`
    : "";
  $("dashboard").classList.toggle("hidden", !loggedIn);
  $("login-prompt").classList.toggle("hidden", loggedIn);

  timerDeadline = state.round_deadline ?? null;
  clockSkew = (state.server_now || 0) - Math.floor(Date.now() / 1000);
  tickTimer();

  if (loggedIn) {
    renderHoldings(meView);
    renderCardOptions($("bid-card"), state.cards.map((c) => ({ id: c.id, label: c.name })));
    renderCardOptions($("offer-card"), meView.holdings.map((h) => ({ id: h.card, label: `${h.name} (×${h.qty})` })));
    updatePreview("bid-card", "bid-preview");
    updatePreview("offer-card", "offer-preview");
    renderOrders($("my-bids"), state.my_bids, "bid");
    renderOrders($("my-offers"), state.my_offers, "offer");
    const n = state.my_bids.length + state.my_offers.length;
    $("orders-count").textContent = n ? `${n} open` : "";
    $("cancel-all").classList.toggle("hidden", n === 0);
  }

  populateFilterOptions();
  renderPlayers();
  renderHistory();
  renderTrades();
  renderPlan();
  renderGallery();
  if (modalCardId !== null) renderModalInfo();

  const live = state.phase === "bidding";
  $$("#bid-form button, #offer-form button").forEach((b) => (b.disabled = !live));
}

function renderAuth(inGame, loggedIn) {
  $("auth").classList.toggle("hidden", !inGame);
  const me = loggedIn ? state.players.find((p) => p.id === state.me) : null;
  $("auth-status").textContent = me ? `Logged in as ${me.name}` : "";
  // Logged-out controls (name/password + token fallback).
  ["login-name", "login-pass", "btn-pw-login", "token-input", "btn-login"].forEach((id) =>
    $(id).classList.toggle("hidden", loggedIn)
  );
  // Logged-in controls.
  $("btn-setpw").classList.toggle("hidden", !loggedIn);
  $("btn-setpw").textContent = state && state.my_has_password ? "Change password" : "Set password";
  $("btn-logout").classList.toggle("hidden", !loggedIn);
}

function flash(el) { el.classList.remove("flash"); void el.offsetWidth; el.classList.add("flash"); }
function rarityClass(r) { return "rarity-" + r; }

function thumb(cardId) {
  const c = cardById[cardId];
  if (!c || !c.image) return "";
  return `<img class="thumb" src="${esc(c.image)}" alt="" loading="lazy" data-card="${cardId}" />`;
}

// Small "your order" badge (bid/offer) for a card.
function orderBadges(id) {
  const b = myBidByCard[id], o = myOfferByCard[id];
  if (!b && !o) return "";
  let s = "";
  if (b) s += `<span class="ord-badge buy">bid ${fmtUSD(b.price)}×${b.qty}</span>`;
  if (o) s += `<span class="ord-badge sell">ask ${fmtUSD(o.price)}×${o.qty}</span>`;
  return ` ${s}`;
}

// Can the logged-in player not even afford one copy at the reference price?
function unaffordable(c) {
  return state.me != null && c.ref_price != null && c.ref_price > (state.my_available || 0);
}

function updatePreview(selectId, imgId) {
  const c = cardById[Number($(selectId).value)];
  const img = $(imgId);
  if (c && c.image) { img.src = c.image; img.style.display = "block"; }
  else { img.removeAttribute("src"); img.style.display = "none"; }
}

function renderHoldings(meView) {
  const tb = $("my-holdings").querySelector("tbody");
  tb.innerHTML = "";
  if (!meView || meView.holdings.length === 0) { tb.innerHTML = `<tr><td class="muted">no cards</td></tr>`; return; }
  meView.holdings.forEach((h) => {
    const offered = myOfferByCard[h.card] ? ` <span class="muted">(${myOfferByCard[h.card].qty} offered)</span>` : "";
    const tr = document.createElement("tr");
    tr.innerHTML = `<td>${thumb(h.card)}${esc(h.name)}${offered}</td><td class="num">×${h.qty}</td>`;
    tb.appendChild(tr);
  });
}

function renderCardOptions(sel, items) {
  const prev = sel.value;
  sel.innerHTML = "";
  items.forEach((it) => { const o = document.createElement("option"); o.value = it.id; o.textContent = it.label; sel.appendChild(o); });
  if (items.some((it) => String(it.id) === prev)) sel.value = prev;
}

function renderOrders(table, orders, kind) {
  const tb = table.querySelector("tbody");
  tb.innerHTML = "";
  if (orders.length === 0) { tb.innerHTML = `<tr><td class="muted">none</td></tr>`; return; }
  orders.forEach((o) => {
    const tr = document.createElement("tr");
    tr.innerHTML = `<td>${esc(o.name)}</td><td class="num">×${o.qty}</td><td class="num">@${fmtUSD(o.price)}</td>`;
    const td = document.createElement("td");
    const btn = document.createElement("button");
    btn.className = "linkbtn"; btn.textContent = "cancel";
    btn.onclick = () => cancelOrder(kind, o.card);
    td.appendChild(btn); tr.appendChild(td); tb.appendChild(tr);
  });
}

function renderPlayers() {
  const tb = $("players").querySelector("tbody");
  tb.innerHTML = "";
  state.players.forEach((p) => {
    const tr = document.createElement("tr");
    const meMark = p.id === state.me ? " ★" : "";
    const debt = p.balance < 0 ? ' style="color:var(--sell)"' : "";
    tr.innerHTML = `<td>${esc(p.name)}${meMark}</td><td class="num"${debt}>${fmtUSD(p.balance)}</td><td class="num">${p.elo}</td><td class="num">${p.card_count}</td>`;
    tb.appendChild(tr);
  });
}

function renderHistory() {
  const div = $("history");
  div.innerHTML = "";
  if (state.history.length === 0) { div.innerHTML = `<p class="muted">No auctions closed yet.</p>`; return; }
  [...state.history].reverse().forEach((r) => {
    const block = document.createElement("div");
    block.className = "round-block";
    const h = document.createElement("h4"); h.textContent = `Round ${r.round}`;
    block.appendChild(h);
    if (r.trades.length === 0) block.innerHTML += `<p class="muted">No orders crossed.</p>`;
    else r.trades.forEach((t) => {
      const line = document.createElement("div");
      line.className = "trade";
      line.innerHTML =
        `<span class="buyer">${esc(t.buyer_name)}</span> bought ${t.qty}× <b>${esc(t.card_name)}</b> ` +
        `from <span class="seller">${esc(t.seller_name)}</span> @ ${fmtUSD(t.price)} ` +
        `<span class="muted">(bid ${fmtUSD(t.bid)} / offer ${fmtUSD(t.offer)})</span>`;
      block.appendChild(line);
    });
    div.appendChild(block);
  });
}

function renderTrades() {
  const div = $("my-trades");
  if (!div) return;
  div.innerHTML = "";
  const trades = (state && state.my_trades) || [];
  if (!state || state.me == null) { div.innerHTML = `<p class="muted">Log in to see your trades.</p>`; return; }
  if (trades.length === 0) { div.innerHTML = `<p class="muted">You haven't traded yet.</p>`; return; }
  const tbl = document.createElement("table");
  tbl.className = "grid";
  tbl.innerHTML = `<thead><tr><th>Rd</th><th></th><th>Card</th><th>With</th><th class="num">Qty</th><th class="num">Price</th></tr></thead>`;
  const tb = document.createElement("tbody");
  [...trades].reverse().forEach((t) => {
    const tr = document.createElement("tr");
    const side = t.side === "bought"
      ? `<span class="buyer">bought</span>`
      : `<span class="seller">sold</span>`;
    tr.innerHTML =
      `<td>${t.round}</td><td>${side}</td><td>${esc(t.name)}</td>` +
      `<td>${esc(t.counterparty)}</td><td class="num">×${t.qty}</td><td class="num">${fmtUSD(t.price)}</td>`;
    tb.appendChild(tr);
  });
  tbl.appendChild(tb);
  div.appendChild(tbl);
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

  // Weekly target (don't clobber the field while the user is editing it).
  const gpw = $("l-gpw");
  if (gpw && document.activeElement !== gpw) gpw.value = ladder.my_games_per_week || 0;
  if (gpw) gpw.max = ladder.max_games_per_week;
  $("l-gpw-max").textContent = `/ ${ladder.max_games_per_week} max`;

  // Availability: re-sync from the server unless there are unsaved edits.
  if (!availDirty) availSet = new Set(ladder.my_availability || []);
  renderCalendar();
  renderMyMatches();
  renderAllMatches();

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

// Availability calendar: one row per local day, a clickable time chip per slot.
// Slots are grouped by their *local* day so the grid reads correctly in any
// timezone (a 21:00 UTC slot can land on the next local morning, etc.).
function renderCalendar() {
  const cal = $("l-calendar");
  if (!(state && state.me != null)) { cal.innerHTML = `<p class="muted">Log in to set your availability.</p>`; return; }
  const blocks = ladder.blocks || [9, 13, 18, 21];
  const nb = blocks.length;
  const days = ladder.window_days || 14;
  const now = ladder.server_now || Math.floor(Date.now() / 1000);
  const todayUtc = Math.floor(now / 86400);

  // Gather candidate slots with a day of padding each side so local-day
  // grouping is complete near the window edges regardless of UTC offset.
  const slots = [];
  for (let d = -1; d <= days + 1; d++) {
    for (let b = 0; b < nb; b++) {
      const slot = (todayUtc + d) * nb + b;
      slots.push({ slot, start: (todayUtc + d) * 86400 + blocks[b] * 3600 });
    }
  }
  const byDay = new Map();
  for (const s of slots) {
    const key = localDayKey(s.start);
    if (!byDay.has(key)) byDay.set(key, { repr: s.start, items: [] });
    byDay.get(key).items.push(s);
  }
  const ordered = [...byDay.values()].sort((a, b) => a.repr - b.repr);
  const todayKey = localDayKey(now);
  const startIdx = Math.max(0, ordered.findIndex((d) => localDayKey(d.repr) === todayKey));
  const visible = ordered.slice(startIdx, startIdx + days);

  let html = `<table class="cal"><tbody>`;
  for (const day of visible) {
    html += `<tr><td class="cal-day">${localDayLabel(day.repr)}</td><td>`;
    day.items.sort((a, b) => a.start - b.start).forEach((s) => {
      const past = s.start <= now;
      const on = availSet.has(s.slot);
      html += `<button class="cal-chip${on ? " on" : ""}" ${past ? "disabled" : `data-slot="${s.slot}"`}>${localTimeLabel(s.start)}</button>`;
    });
    html += `</td></tr>`;
  }
  cal.innerHTML = html + `</tbody></table>`;
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

// ---- filtering & sorting ----
function getFilters(prefix) {
  const box = document.querySelector(`.filters[data-prefix="${prefix}"]`);
  const v = (cls) => box.querySelector(cls)?.value ?? "";
  return { q: v(".f-q").trim().toLowerCase(), rarity: v(".f-rarity"), type: v(".f-type"), mvmin: v(".f-mvmin"), mvmax: v(".f-mvmax"), show: v(".f-show") };
}
function cardMatches(c, f) {
  if (f.q && !c.name.toLowerCase().includes(f.q)) return false;
  if (f.rarity && c.rarity !== f.rarity) return false;
  if (f.type && !(c.type_line || "").includes(f.type)) return false;
  if (f.mvmin !== "" && (c.cmc == null || c.cmc < Number(f.mvmin))) return false;
  if (f.mvmax !== "" && (c.cmc == null || c.cmc > Number(f.mvmax))) return false;
  if (f.show === "owned" && mineOf(c) <= 0) return false;
  if (f.show === "wanted" && !wants.has(c.name)) return false;
  return true;
}
function sortVal(c, key) {
  switch (key) {
    case "name": return c.name.toLowerCase();
    case "type": return shortType(c.type_line).toLowerCase();
    case "cmc": return c.cmc ?? -1;
    case "rarity": return RARITY_RANK[c.rarity];
    case "ref": return c.ref_price ?? -1;
    case "last": return lastClearByCard[c.id] ?? -1;
    case "supply": return c.supply;
    case "mine": return mineOf(c);
    case "want": return wants.has(c.name) ? 1 : 0;
    default: return 0;
  }
}
function sortCards(cards, key, dir) {
  return cards.slice().sort((a, b) => {
    const va = sortVal(a, key), vb = sortVal(b, key);
    let cmp = va < vb ? -1 : va > vb ? 1 : 0;
    if (cmp === 0) cmp = a.name.localeCompare(b.name);
    return cmp * dir;
  });
}
function populateFilterOptions() {
  const typesPresent = KNOWN_TYPES.filter((t) => state.cards.some((c) => (c.type_line || "").includes(t)));
  const typeSig = typesPresent.join(",");
  $$(".filters").forEach((box) => {
    const rs = box.querySelector(".f-rarity");
    if (rs.dataset.sig !== "r") { RARITIES.forEach((r) => rs.add(new Option(r, r))); rs.dataset.sig = "r"; }
    const ts = box.querySelector(".f-type");
    if (ts.dataset.sig !== typeSig) {
      const cur = ts.value; ts.length = 1;
      typesPresent.forEach((t) => ts.add(new Option(t, t)));
      ts.value = typesPresent.includes(cur) ? cur : "";
      ts.dataset.sig = typeSig;
    }
  });
}

function renderPlan() {
  if (!state) return;
  const f = getFilters("inv");
  const rows = sortCards(state.cards.filter((c) => cardMatches(c, f)), planSortKey, planSortDir);
  const tb = $("plan").querySelector("tbody");
  tb.innerHTML = "";
  rows.forEach((c) => {
    const tr = document.createElement("tr");
    tr.dataset.card = c.id;
    if (unaffordable(c)) tr.classList.add("unafford");
    tr.innerHTML =
      `<td class="want-cell"><button class="want-star ${wants.has(c.name) ? "on" : ""}" data-name="${esc(c.name)}">${star(c.name)}</button></td>` +
      `<td>${thumb(c.id)}${esc(c.name)}${orderBadges(c.id)}</td>` +
      `<td>${esc(shortType(c.type_line))}</td>` +
      `<td class="num">${fmtMV(c.cmc)}</td>` +
      `<td class="${rarityClass(c.rarity)}">${c.rarity}</td>` +
      `<td class="num">${fmtUSD(c.ref_price)}</td>` +
      `<td class="num">${fmtUSD(lastClearByCard[c.id] ?? null)}</td>` +
      `<td class="num">${c.supply}</td>` +
      `<td class="num you">${mineOf(c) || ""}</td>`;
    tb.appendChild(tr);
  });
  document.querySelector('.filters[data-prefix="inv"] .f-count').textContent = `${rows.length} / ${state.cards.length}`;
  $("plan").querySelectorAll("th[data-sort]").forEach((th) => {
    const k = th.dataset.sort;
    th.classList.toggle("sorted", k === planSortKey);
    th.dataset.arrow = k === planSortKey ? (planSortDir === 1 ? " ▲" : " ▼") : "";
  });
}

function renderGallery() {
  if (!state) return;
  const box = document.querySelector('.filters[data-prefix="mkt"]');
  const key = box.querySelector(".f-sort").value;
  const dir = Number(box.querySelector(".f-dir").dataset.dir || "-1");
  const f = getFilters("mkt");
  const rows = sortCards(state.cards.filter((c) => cardMatches(c, f)), key, dir);
  const g = $("gallery");
  g.innerHTML = "";
  rows.forEach((c) => {
    const mine = mineOf(c);
    const tile = document.createElement("div");
    tile.className = "tile" + (wants.has(c.name) ? " wanted" : "") + (unaffordable(c) ? " unafford" : "");
    tile.dataset.card = c.id;
    const art = c.image
      ? `<img class="tile-img" src="${esc(c.image)}" alt="" loading="lazy" />`
      : `<div class="tile-img no-img ${rarityClass(c.rarity)}">${esc(c.name)}</div>`;
    tile.innerHTML =
      `<button class="want-star ${wants.has(c.name) ? "on" : ""}" data-name="${esc(c.name)}">${star(c.name)}</button>` +
      art +
      `<div class="tile-name">${esc(c.name)}</div>` +
      `<div class="tile-sub muted">${esc(shortType(c.type_line))} · MV ${fmtMV(c.cmc)}</div>` +
      `<div class="tile-foot"><span class="${rarityClass(c.rarity)}">${c.rarity}</span><span class="num">ref ${fmtUSD(c.ref_price)}</span></div>` +
      `<div class="tile-foot muted"><span>last ${fmtUSD(lastClearByCard[c.id] ?? null)}</span><span>sup ${c.supply}${mine ? ` · you ${mine}` : ""}</span></div>` +
      (orderBadges(c.id) ? `<div class="tile-orders">${orderBadges(c.id)}</div>` : "");
    g.appendChild(tile);
  });
  box.querySelector(".f-count").textContent = `${rows.length} / ${state.cards.length}`;
}

// ---- card modal ----
function openModal(id) {
  if (!cardById[id]) return;
  modalCardId = id;
  $("m-qty").value = 1;
  $("m-price").value = (defaultPriceCents(id) / 100).toFixed(2);
  $("m-error").textContent = "";
  $("modal").classList.remove("hidden");
  renderModalInfo();
}
function closeModal() { modalCardId = null; $("modal").classList.add("hidden"); }

function renderModalInfo() {
  const c = cardById[modalCardId];
  if (!c) return;
  const img = $("modal-img");
  if (c.image) { img.src = c.image; img.style.display = "block"; } else { img.removeAttribute("src"); img.style.display = "none"; }
  $("modal-name").textContent = c.name;
  $("modal-meta").innerHTML = [
    c.type_line ? esc(c.type_line) : null,
    c.mana_cost ? `Cost ${esc(c.mana_cost)}` : (c.cmc != null ? `MV ${fmtMV(c.cmc)}` : null),
    `<span class="${rarityClass(c.rarity)}">${c.rarity}</span>`,
    `Ref ${fmtUSD(c.ref_price)}`,
    `Last ${fmtUSD(lastClearByCard[c.id] ?? null)}`,
    `Supply ${c.supply}`,
    `You ${mineOf(c)}`,
  ].filter(Boolean).join(" · ");
  const wb = $("modal-want");
  wb.dataset.name = c.name;
  wb.textContent = wants.has(c.name) ? "★ Wanted — click to unmark" : "☆ Mark as wanted";

  // Your current orders on this card + last round's spread.
  const b = myBidByCard[c.id], o = myOfferByCard[c.id];
  const sp = latestClearByCard[c.id];
  let yours = [];
  if (b) yours.push(`<span class="buyer">Your bid ${fmtUSD(b.price)} ×${b.qty}</span>`);
  if (o) yours.push(`<span class="seller">Your ask ${fmtUSD(o.price)} ×${o.qty}</span>`);
  if (sp) {
    const cleared = sp.cleared != null ? `, cleared ${fmtUSD(sp.cleared)} (×${sp.volume})` : ", no fill";
    yours.push(`<span class="muted">R${sp.round}: bid ${fmtUSD(sp.best_bid)} / ask ${fmtUSD(sp.best_offer)}${cleared}</span>`);
  }
  $("modal-yours").innerHTML = yours.join("<br>") || `<span class="muted">No orders on this card yet.</span>`;

  // Price history.
  const hist = clearHistByCard[c.id] || [];
  $("modal-history").innerHTML = hist.length
    ? `<span class="muted">Cleared:</span> ` + hist.map((h) => `<span class="hist">R${h.round} ${fmtUSD(h.price)}</span>`).join(" ")
    : "";

  modalAfford();
}

function modalAfford() {
  const c = cardById[modalCardId];
  if (!c) return;
  const loggedIn = state && state.me != null;
  const live = state && state.phase === "bidding";
  const qty = Math.max(0, Number($("m-qty").value) || 0);
  const price = toCents($("m-price").value);
  const commit = qty * price;
  const existing = myBidByCard[c.id] ? myBidByCard[c.id].qty * myBidByCard[c.id].price : 0;
  const availForBid = (state ? state.my_available : 0) + existing;
  const left = availForBid - commit;
  const owned = mineOf(c);

  $("m-bid").disabled = !loggedIn || !live || qty < 1 || left < 0;
  $("m-offer").disabled = !loggedIn || !live || qty < 1 || qty > owned;
  const af = $("m-afford");
  if (!loggedIn) af.textContent = "Log in to trade.";
  else if (!live) af.textContent = "Auction is closed.";
  else {
    af.innerHTML = `Bid commits <b>${fmtUSD(commit)}</b> · ${fmtUSD(left)} left` +
      (owned ? ` · you hold ${owned}` : ` · you don't own this`);
  }
  af.classList.toggle("bad", live && loggedIn && left < 0);
}

async function modalOrder(kind) {
  if (modalCardId == null) return;
  const card = modalCardId;
  const qty = Number($("m-qty").value);
  const price = toCents($("m-price").value);
  try {
    await api(kind === "bid" ? "/api/bid" : "/api/offer", "POST", { player: state.me, card, qty, price });
    $("m-error").textContent = "";
    await refresh();
  } catch (e) { $("m-error").textContent = e.message; }
}

function toggleWant(name) {
  if (wants.has(name)) wants.delete(name); else wants.add(name);
  saveWants();
  renderPlan(); renderGallery();
  if (modalCardId != null) renderModalInfo();
}

// ---- toasts ----
function toast(html, kind) {
  const t = document.createElement("div");
  t.className = "toast" + (kind ? " " + kind : "");
  t.innerHTML = html;
  $("toasts").appendChild(t);
  setTimeout(() => { t.classList.add("out"); setTimeout(() => t.remove(), 400); }, kind === "error" ? 7000 : 6000);
}
function toastError(msg) { toast(esc(msg), "error"); }

// ---- live-connection indicator ----
function setConn(live) {
  const el = $("conn");
  if (!el) return;
  el.className = "conn " + (live ? "live" : "down");
  el.textContent = live ? "● live" : "● offline";
  el.title = live ? "Live updates connected" : "Reconnecting…";
}

// ---- magic-link login: ?t=<token> logs you in, then is stripped from the URL ----
function consumeMagicLink() {
  const params = new URLSearchParams(location.search);
  const t = params.get("t");
  if (!t) return;
  setToken(t);
  params.delete("t");
  history.replaceState({}, "", location.pathname + (params.toString() ? "?" + params : ""));
}
function roundToast(round) {
  const me = state.me;
  const bought = round.trades.filter((t) => t.buyer === me);
  const sold = round.trades.filter((t) => t.seller === me);
  let parts = [];
  bought.forEach((t) => parts.push(`<span class="buyer">bought ${t.qty}× ${esc(t.card_name)} @ ${fmtUSD(t.price)}</span>`));
  sold.forEach((t) => parts.push(`<span class="seller">sold ${t.qty}× ${esc(t.card_name)} @ ${fmtUSD(t.price)}</span>`));
  toast(`<b>Round ${round.round} closed</b><br>${parts.length ? parts.join("<br>") : "no fills for you"}`);
}

function setError(msg) { $("order-error").textContent = msg || ""; }

// ---- UI persistence (filters + sort) ----
function saveUi() {
  const read = (prefix) => {
    const b = document.querySelector(`.filters[data-prefix="${prefix}"]`);
    const v = (cls) => b.querySelector(cls)?.value ?? "";
    return { q: v(".f-q"), rarity: v(".f-rarity"), mvmin: v(".f-mvmin"), mvmax: v(".f-mvmax"), show: v(".f-show") };
  };
  const mktBox = document.querySelector('.filters[data-prefix="mkt"]');
  const ui = {
    inv: read("inv"),
    mkt: { ...read("mkt"), sort: mktBox.querySelector(".f-sort").value, dir: mktBox.querySelector(".f-dir").dataset.dir || "-1" },
    plan: { key: planSortKey, dir: planSortDir },
  };
  localStorage.setItem(UI_KEY, JSON.stringify(ui));
}
function restoreUi() {
  let ui; try { ui = JSON.parse(localStorage.getItem(UI_KEY) || "null"); } catch { ui = null; }
  if (!ui) return;
  const apply = (prefix, vals) => {
    const b = document.querySelector(`.filters[data-prefix="${prefix}"]`);
    const set = (cls, val) => { const el = b.querySelector(cls); if (el != null && val != null) el.value = val; };
    set(".f-q", vals.q); set(".f-rarity", vals.rarity); set(".f-mvmin", vals.mvmin); set(".f-mvmax", vals.mvmax); set(".f-show", vals.show);
  };
  if (ui.inv) apply("inv", ui.inv);
  if (ui.mkt) {
    apply("mkt", ui.mkt);
    const b = document.querySelector('.filters[data-prefix="mkt"]');
    if (ui.mkt.sort) b.querySelector(".f-sort").value = ui.mkt.sort;
    const dirBtn = b.querySelector(".f-dir");
    dirBtn.dataset.dir = ui.mkt.dir || "-1";
    dirBtn.textContent = dirBtn.dataset.dir === "1" ? "▲" : "▼";
  }
  if (ui.plan) { planSortKey = ui.plan.key || planSortKey; planSortDir = ui.plan.dir || planSortDir; }
}

// ---- actions ----
function setToken(t) { authToken = t || ""; if (authToken) localStorage.setItem(TOKEN_KEY, authToken); else localStorage.removeItem(TOKEN_KEY); }

$("btn-login").onclick = async () => {
  const token = $("token-input").value.trim();
  if (!token) return;
  try { await api("/api/login", "POST", { token }); setToken(token); $("token-input").value = ""; await refresh(); }
  catch (e) { toastError(`Login failed: ${e.message}`); }
};
$("btn-pw-login").onclick = async () => {
  const name = $("login-name").value.trim();
  const password = $("login-pass").value;
  if (!name || !password) return;
  try {
    const r = await api("/api/password-login", "POST", { name, password });
    setToken(r.token); $("login-pass").value = ""; await refresh();
  } catch (e) { toastError(`Login failed: ${e.message}`); }
};
$("login-pass").addEventListener("keydown", (e) => { if (e.key === "Enter") $("btn-pw-login").click(); });
$("btn-setpw").onclick = async () => {
  const password = prompt(
    "Choose a password to log in by name.\n\n" +
    "⚠️ Do NOT reuse a password you use anywhere else — this site's security is weak and the password is only lightly protected."
  );
  if (password == null) return;
  try { await api("/api/set-password", "POST", { password }); toast("Password saved."); await refresh(); }
  catch (e) { toastError(e.message); }
};
$("btn-logout").onclick = async () => { setToken(""); await refresh(); };

async function cancelOrder(kind, card) {
  try { await api(kind === "bid" ? "/api/bid" : "/api/offer", "POST", { player: state.me, card, qty: 0, price: 0 }); setError(""); await refresh(); }
  catch (e) { setError(e.message); }
}

$("cancel-all").onclick = async () => {
  const jobs = [
    ...state.my_bids.map((o) => ["bid", o.card]),
    ...state.my_offers.map((o) => ["offer", o.card]),
  ];
  for (const [k, c] of jobs) {
    try { await api(k === "bid" ? "/api/bid" : "/api/offer", "POST", { player: state.me, card: c, qty: 0, price: 0 }); } catch (e) { /* keep going */ }
  }
  await refresh();
};

$("bid-card").onchange = () => updatePreview("bid-card", "bid-preview");
$("offer-card").onchange = () => updatePreview("offer-card", "offer-preview");

$("bid-form").onsubmit = async (e) => {
  e.preventDefault();
  try { await api("/api/bid", "POST", { player: state.me, card: Number($("bid-card").value), qty: Number($("bid-qty").value), price: toCents($("bid-price").value) }); setError(""); await refresh(); }
  catch (e) { setError(e.message); }
};
$("offer-form").onsubmit = async (e) => {
  e.preventDefault();
  try { await api("/api/offer", "POST", { player: state.me, card: Number($("offer-card").value), qty: Number($("offer-qty").value), price: toCents($("offer-price").value) }); setError(""); await refresh(); }
  catch (e) { setError(e.message); }
};

// Modal trade controls
$("m-qty").oninput = modalAfford;
$("m-price").oninput = modalAfford;
$$(".step").forEach((b) => (b.onclick = () => {
  const cents = Math.max(0, toCents($("m-price").value) + Number(b.dataset.delta));
  $("m-price").value = (cents / 100).toFixed(2);
  modalAfford();
}));
$("m-ref").onclick = () => { const c = cardById[modalCardId]; if (c && c.ref_price != null) { $("m-price").value = (c.ref_price / 100).toFixed(2); modalAfford(); } };
$("m-last").onclick = () => { const p = lastClearByCard[modalCardId]; if (p != null) { $("m-price").value = (p / 100).toFixed(2); modalAfford(); } };
$("m-bid").onclick = () => modalOrder("bid");
$("m-offer").onclick = () => modalOrder("offer");

// Tabs
$$(".tab").forEach((t) => (t.onclick = () => {
  activeTab = t.dataset.tab;
  $$(".tab").forEach((x) => x.classList.toggle("active", x === t));
  $("tab-inventory").classList.toggle("hidden", activeTab !== "inventory");
  $("tab-market").classList.toggle("hidden", activeTab !== "market");
  $("tab-ladder").classList.toggle("hidden", activeTab !== "ladder");
}));

// Filter bars
$$(".filters").forEach((box) => {
  const prefix = box.dataset.prefix;
  const rerender = () => { (prefix === "inv" ? renderPlan() : renderGallery()); saveUi(); };
  box.addEventListener("input", rerender);
  box.addEventListener("change", rerender);
});
document.querySelector('.filters[data-prefix="mkt"] .f-dir').onclick = (e) => {
  const b = e.currentTarget;
  const dir = Number(b.dataset.dir || "-1") * -1;
  b.dataset.dir = dir; b.textContent = dir === 1 ? "▲" : "▼";
  renderGallery(); saveUi();
};

// Plan header sorting
$("plan").querySelectorAll("th[data-sort]").forEach((th) => (th.onclick = () => {
  const k = th.dataset.sort;
  if (planSortKey === k) planSortDir = -planSortDir;
  else { planSortKey = k; planSortDir = (k === "name" || k === "type") ? 1 : -1; }
  renderPlan(); saveUi();
}));

// Click-to-enlarge / want-star (delegated). Stars handled first.
document.addEventListener("click", (e) => {
  const s = e.target.closest(".want-star");
  if (s) { e.stopPropagation(); toggleWant(s.dataset.name); return; }
  if (e.target.closest(".modal")) return;
  const el = e.target.closest("[data-card]");
  if (el) openModal(Number(el.dataset.card));
});

$("modal-close").onclick = closeModal;
$("modal").querySelector(".modal-backdrop").onclick = closeModal;
$("modal-want").onclick = (e) => toggleWant(e.currentTarget.dataset.name);
document.addEventListener("keydown", (e) => { if (e.key === "Escape") closeModal(); });

// ---- timer countdown ----
function tickTimer() {
  const el = $("round-timer");
  if (!state || state.phase !== "bidding" || !timerDeadline) { el.textContent = ""; el.classList.remove("urgent"); return; }
  const rem = timerDeadline - (Math.floor(Date.now() / 1000) + clockSkew);
  if (rem <= 0) { el.textContent = "⏱ closing…"; el.classList.add("urgent"); return; }
  const m = Math.floor(rem / 60), s = rem % 60;
  el.textContent = `⏱ ${m}:${String(s).padStart(2, "0")}`;
  el.classList.toggle("urgent", rem <= 10);
}
setInterval(tickTimer, 1000);

// ---- live updates ----
function connectEvents() {
  try {
    const es = new EventSource("/api/events");
    es.onopen = () => setConn(true);
    es.onmessage = () => { setConn(true); refresh(); };
    es.onerror = () => setConn(false); // browser auto-reconnects
  } catch (e) { setConn(false); console.error(e); }
}

consumeMagicLink();
setConn(false);
connectEvents();
refresh();
setInterval(refresh, 15000);
