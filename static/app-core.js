"use strict";

// Shared state and top-level rendering. TOKEN_KEY, $, $$, fmtUSD, toCents,
// escapeHtml/esc come from util.js (loaded first); the tab-specific renderers
// (renderPlan, renderGallery, renderLadder, renderTodo, ...) live in
// app-home.js / app-market.js / app-ladder.js, loaded after this file.
const UI_KEY = "mtg_auction_ui";

let authToken = localStorage.getItem(TOKEN_KEY) || "";
let state = null;
let cardById = {};
let myQty = {};                 // card id -> copies I hold
let myBidByCard = {}, myOfferByCard = {};
let lastClearByCard = {};       // card id -> last cleared price (cents)
let latestClearByCard = {};     // card id -> {round, best_bid, best_offer, cleared, volume}
let clearHistByCard = {};       // card id -> [{round, price}]
let timerDeadline = null, clockSkew = 0;
let prevBalance = null, prevHistoryLen = null;
let uiRestored = false;
let ladder = null;

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
  else if (state.phase === "finished") $("status").textContent = `${state.set_name} — game over.`;
  else $("status").textContent = `${state.set_name} — ${phaseLabel(state.phase)} · round ${state.round} of ${state.total_rounds} — debt limit ${fmtUSD(state.debt_limit)}`;

  // Per-round results toast when a new round closes. `rounds_closed` counts
  // every close; `history` itself only carries the most recent rounds.
  const closed = state.rounds_closed ?? state.history.length;
  if (prevHistoryLen !== null && closed > prevHistoryLen && loggedIn && state.history.length) {
    roundToast(state.history[state.history.length - 1]);
  }
  prevHistoryLen = closed;

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
  renderMyOrders();
  renderHome();
  renderTodo();
  renderMonthCalendar();
  if (modalCardId !== null) renderModalInfo();

  const live = isTrading(state);
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
    return { q: v(".f-q"), rarity: v(".f-rarity"), mvmin: v(".f-mvmin"), mvmax: v(".f-mvmax"), show: v(".f-show"), color: readColorFilter(b.querySelector(".colorsel")) };
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
    applyColorFilter(b.querySelector(".colorsel"), vals.color);
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

// ---- timer countdown ----
function tickTimer() {
  const el = $("round-timer");
  if (!isTrading(state) || !timerDeadline) { el.textContent = ""; el.classList.remove("urgent"); return; }
  const rem = timerDeadline - (Math.floor(Date.now() / 1000) + clockSkew);
  if (rem <= 0) { el.textContent = "⏱ closing…"; el.classList.add("urgent"); return; }
  const d = Math.floor(rem / 86400), h = Math.floor((rem % 86400) / 3600);
  const m = Math.floor((rem % 3600) / 60), s = rem % 60;
  const pad = (n) => String(n).padStart(2, "0");
  // Roll up into days/hours for long rounds; keep the ticking seconds once
  // we're under an hour, where they actually matter.
  const t = d > 0 ? `${d}d ${h}h ${m}m` : h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${m}:${pad(s)}`;
  el.textContent = `⏱ ${t}`;
  el.classList.toggle("urgent", rem <= 10);
}
setInterval(tickTimer, 1000);
