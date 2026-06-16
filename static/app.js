"use strict";

const TOKEN_KEY = "mtg_auction_token";
const WANTS_KEY = "mtg_auction_wants";

let authToken = localStorage.getItem(TOKEN_KEY) || "";
let state = null;          // last fetched StateView
let cardById = {};         // id -> card
let myQty = {};            // card id -> how many I hold
let wants = loadWants();   // Set of card names I've starred
let activeTab = "inventory";
let planSortKey = "rarity", planSortDir = -1;
let timerDeadline = null;   // epoch second the round closes
let clockSkew = 0;          // server epoch − client epoch (seconds)

const $ = (id) => document.getElementById(id);
const RARITY_RANK = { common: 0, uncommon: 1, rare: 2, mythic: 3 };
const RARITIES = ["common", "uncommon", "rare", "mythic"];
const KNOWN_TYPES = ["Creature", "Planeswalker", "Instant", "Sorcery", "Artifact", "Enchantment", "Land", "Battle", "Kindred"];

// ---- helpers ----
function fmtUSD(cents) {
  if (cents === null || cents === undefined) return "—";
  const neg = cents < 0, v = Math.abs(cents);
  return (neg ? "-$" : "$") + Math.floor(v / 100) + "." + String(v % 100).padStart(2, "0");
}
function toCents(d) { return Math.round(parseFloat(d) * 100); }
function fmtMV(cmc) { return cmc === null || cmc === undefined ? "—" : String(cmc); }
function shortType(tl) {
  if (!tl) return "—";
  const i = tl.indexOf("—");
  return (i >= 0 ? tl.slice(0, i) : tl).trim();
}
function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}
function mineOf(c) { return myQty[c.id] || 0; }
function loadWants() {
  try { return new Set(JSON.parse(localStorage.getItem(WANTS_KEY) || "[]")); }
  catch { return new Set(); }
}
function saveWants() { localStorage.setItem(WANTS_KEY, JSON.stringify([...wants])); }

async function api(path, method = "GET", body = null) {
  const opts = { method, headers: {} };
  if (authToken) opts.headers["X-Token"] = authToken;
  if (body !== null) {
    opts.headers["Content-Type"] = "application/json";
    opts.body = JSON.stringify(body);
  }
  const res = await fetch(path, opts);
  const data = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(data.error || `request failed (${res.status})`);
  return data;
}

async function refresh() {
  try { state = await api("/api/state"); render(); }
  catch (e) { console.error(e); }
}

// ---- top-level render ----
function render() {
  if (!state) return;
  cardById = {};
  state.cards.forEach((c) => (cardById[c.id] = c));

  const inGame = state.phase !== "setup";
  const loggedIn = state.me !== null && state.me !== undefined;

  if (!inGame) $("status").textContent = "No game in progress.";
  else if (state.phase === "finished") $("status").textContent = `${state.set_name} — game over after ${state.total_rounds} rounds.`;
  else $("status").textContent = `${state.set_name} — round ${state.round} of ${state.total_rounds} — debt limit ${fmtUSD(state.debt_limit)}`;

  renderAuth(inGame, loggedIn);
  $("no-game").classList.toggle("hidden", inGame);
  $("game").classList.toggle("hidden", !inGame);
  if (!inGame) return;

  const meView = loggedIn ? state.players.find((p) => p.id === state.me) : null;
  myQty = {};
  if (meView) meView.holdings.forEach((h) => (myQty[h.card] = h.qty));

  $("me-balance").textContent = meView ? fmtUSD(meView.balance) : "";
  $("me-funds").textContent = meView
    ? `Committed ${fmtUSD(state.my_committed)} · Available to bid ${fmtUSD(state.my_available)}`
    : "";
  $("dashboard").classList.toggle("hidden", !loggedIn);
  $("login-prompt").classList.toggle("hidden", loggedIn);

  // Round timer: capture deadline + clock skew for the local countdown.
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
  }

  populateFilterOptions();
  renderPlayers();
  renderHistory();
  renderPlan();
  renderGallery();

  const live = state.phase === "bidding";
  document.querySelectorAll("#bid-form button, #offer-form button").forEach((b) => (b.disabled = !live));
}

function renderAuth(inGame, loggedIn) {
  $("auth").classList.toggle("hidden", !inGame);
  const me = loggedIn ? state.players.find((p) => p.id === state.me) : null;
  $("auth-status").textContent = me ? `Logged in as ${me.name}` : "";
  $("token-input").classList.toggle("hidden", loggedIn);
  $("btn-login").classList.toggle("hidden", loggedIn);
  $("btn-logout").classList.toggle("hidden", !loggedIn);
}

function thumb(cardId) {
  const c = cardById[cardId];
  if (!c || !c.image) return "";
  return `<img class="thumb" src="${escapeHtml(c.image)}" alt="" loading="lazy" data-card="${cardId}" />`;
}
function rarityClass(r) { return "rarity-" + r; }
function star(name) { return wants.has(name) ? "★" : "☆"; }

function updatePreview(selectId, imgId) {
  const c = cardById[Number($(selectId).value)];
  const img = $(imgId);
  if (c && c.image) { img.src = c.image; img.style.display = "block"; }
  else { img.removeAttribute("src"); img.style.display = "none"; }
}

function renderHoldings(meView) {
  const tb = $("my-holdings").querySelector("tbody");
  tb.innerHTML = "";
  if (!meView || meView.holdings.length === 0) {
    tb.innerHTML = `<tr><td class="muted">no cards</td></tr>`;
    return;
  }
  meView.holdings.forEach((h) => {
    const tr = document.createElement("tr");
    tr.innerHTML = `<td>${thumb(h.card)}${escapeHtml(h.name)}</td><td class="num">×${h.qty}</td>`;
    tb.appendChild(tr);
  });
}

function renderCardOptions(sel, items) {
  const prev = sel.value;
  sel.innerHTML = "";
  items.forEach((it) => {
    const o = document.createElement("option");
    o.value = it.id; o.textContent = it.label;
    sel.appendChild(o);
  });
  if (items.some((it) => String(it.id) === prev)) sel.value = prev;
}

function renderOrders(table, orders, kind) {
  const tb = table.querySelector("tbody");
  tb.innerHTML = "";
  if (orders.length === 0) { tb.innerHTML = `<tr><td class="muted">none</td></tr>`; return; }
  orders.forEach((o) => {
    const tr = document.createElement("tr");
    tr.innerHTML = `<td>${escapeHtml(o.name)}</td><td class="num">×${o.qty}</td><td class="num">@${fmtUSD(o.price)}</td>`;
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
    tr.innerHTML = `<td>${escapeHtml(p.name)}${meMark}</td><td class="num"${debt}>${fmtUSD(p.balance)}</td><td class="num">${p.card_count}</td>`;
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
        `<span class="buyer">${escapeHtml(t.buyer_name)}</span> bought ${t.qty}× ` +
        `<b>${escapeHtml(t.card_name)}</b> from <span class="seller">${escapeHtml(t.seller_name)}</span> ` +
        `@ ${fmtUSD(t.price)} <span class="muted">(bid ${fmtUSD(t.bid)} / offer ${fmtUSD(t.offer)})</span>`;
      block.appendChild(line);
    });
    div.appendChild(block);
  });
}

// ---- filtering & sorting ----
function getFilters(prefix) {
  const box = document.querySelector(`.filters[data-prefix="${prefix}"]`);
  const v = (cls) => box.querySelector(cls)?.value ?? "";
  return {
    q: v(".f-q").trim().toLowerCase(),
    rarity: v(".f-rarity"),
    type: v(".f-type"),
    mvmin: v(".f-mvmin"),
    mvmax: v(".f-mvmax"),
    show: v(".f-show"),
  };
}

function cardMatches(c, f) {
  if (f.q && !c.name.toLowerCase().includes(f.q)) return false;
  if (f.rarity && c.rarity !== f.rarity) return false;
  if (f.type && !(c.type_line || "").includes(f.type)) return false;
  if (f.mvmin !== "" && (c.cmc === null || c.cmc === undefined || c.cmc < Number(f.mvmin))) return false;
  if (f.mvmax !== "" && (c.cmc === null || c.cmc === undefined || c.cmc > Number(f.mvmax))) return false;
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
  document.querySelectorAll(".filters").forEach((box) => {
    const rs = box.querySelector(".f-rarity");
    if (rs.dataset.sig !== "r") {
      RARITIES.forEach((r) => rs.add(new Option(r, r)));
      rs.dataset.sig = "r";
    }
    const ts = box.querySelector(".f-type");
    if (ts.dataset.sig !== typeSig) {
      const cur = ts.value;
      ts.length = 1; // keep "all types"
      typesPresent.forEach((t) => ts.add(new Option(t, t)));
      ts.value = typesPresent.includes(cur) ? cur : "";
      ts.dataset.sig = typeSig;
    }
  });
}

// ---- inventory planning table ----
function renderPlan() {
  const f = getFilters("inv");
  const rows = sortCards(state.cards.filter((c) => cardMatches(c, f)), planSortKey, planSortDir);
  const tb = $("plan").querySelector("tbody");
  tb.innerHTML = "";
  rows.forEach((c) => {
    const tr = document.createElement("tr");
    tr.dataset.card = c.id;
    const mine = mineOf(c);
    tr.innerHTML =
      `<td class="want-cell"><button class="want-star ${wants.has(c.name) ? "on" : ""}" data-name="${escapeHtml(c.name)}">${star(c.name)}</button></td>` +
      `<td>${thumb(c.id)}${escapeHtml(c.name)}</td>` +
      `<td>${escapeHtml(shortType(c.type_line))}</td>` +
      `<td class="num">${fmtMV(c.cmc)}</td>` +
      `<td class="${rarityClass(c.rarity)}">${c.rarity}</td>` +
      `<td class="num">${fmtUSD(c.ref_price)}</td>` +
      `<td class="num">${c.supply}</td>` +
      `<td class="num you">${mine || ""}</td>`;
    tb.appendChild(tr);
  });
  document.querySelector('.filters[data-prefix="inv"] .f-count').textContent = `${rows.length} / ${state.cards.length}`;
  $("plan").querySelectorAll("th[data-sort]").forEach((th) => {
    const k = th.dataset.sort;
    th.classList.toggle("sorted", k === planSortKey);
    th.dataset.arrow = k === planSortKey ? (planSortDir === 1 ? " ▲" : " ▼") : "";
  });
}

// ---- market gallery ----
function renderGallery() {
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
    tile.className = "tile" + (wants.has(c.name) ? " wanted" : "");
    tile.dataset.card = c.id;
    const art = c.image
      ? `<img class="tile-img" src="${escapeHtml(c.image)}" alt="" loading="lazy" />`
      : `<div class="tile-img no-img ${rarityClass(c.rarity)}">${escapeHtml(c.name)}</div>`;
    tile.innerHTML =
      `<button class="want-star ${wants.has(c.name) ? "on" : ""}" data-name="${escapeHtml(c.name)}">${star(c.name)}</button>` +
      art +
      `<div class="tile-name">${escapeHtml(c.name)}</div>` +
      `<div class="tile-sub muted">${escapeHtml(shortType(c.type_line))} · MV ${fmtMV(c.cmc)}</div>` +
      `<div class="tile-foot"><span class="${rarityClass(c.rarity)}">${c.rarity}</span><span class="num">${fmtUSD(c.ref_price)}</span></div>` +
      `<div class="tile-foot muted"><span>supply ${c.supply}</span><span>you ${mine}</span></div>`;
    g.appendChild(tile);
  });
  box.querySelector(".f-count").textContent = `${rows.length} / ${state.cards.length}`;
}

// ---- card modal ----
function openModal(id) {
  const c = cardById[id];
  if (!c) return;
  const img = $("modal-img");
  if (c.image) { img.src = c.image; img.style.display = "block"; }
  else { img.removeAttribute("src"); img.style.display = "none"; }
  $("modal-name").textContent = c.name;
  const meta = [
    c.type_line ? escapeHtml(c.type_line) : null,
    c.mana_cost ? `Cost ${escapeHtml(c.mana_cost)}` : (c.cmc != null ? `MV ${fmtMV(c.cmc)}` : null),
    `<span class="${rarityClass(c.rarity)}">${c.rarity}</span>`,
    `Ref ${fmtUSD(c.ref_price)}`,
    `Supply ${c.supply}`,
    `You ${mineOf(c)}`,
  ].filter(Boolean).join(" · ");
  $("modal-meta").innerHTML = meta;
  const wb = $("modal-want");
  wb.dataset.name = c.name;
  wb.textContent = wants.has(c.name) ? "★ Wanted — click to unmark" : "☆ Mark as wanted";
  $("modal").classList.remove("hidden");
}
function closeModal() { $("modal").classList.add("hidden"); }

function toggleWant(name) {
  if (wants.has(name)) wants.delete(name); else wants.add(name);
  saveWants();
  renderPlan();
  renderGallery();
  if (!$("modal").classList.contains("hidden")) {
    const wb = $("modal-want");
    if (wb.dataset.name === name) wb.textContent = wants.has(name) ? "★ Wanted — click to unmark" : "☆ Mark as wanted";
    // refresh stars in any open list
  }
}

function setError(msg) { $("order-error").textContent = msg || ""; }

// ---- actions ----
function setToken(t) {
  authToken = t || "";
  if (authToken) localStorage.setItem(TOKEN_KEY, authToken);
  else localStorage.removeItem(TOKEN_KEY);
}

$("btn-login").onclick = async () => {
  const token = $("token-input").value.trim();
  if (!token) return;
  try { await api("/api/login", "POST", { token }); setToken(token); $("token-input").value = ""; await refresh(); }
  catch (e) { alert(e.message); }
};
$("btn-logout").onclick = async () => { setToken(""); await refresh(); };

async function cancelOrder(kind, card) {
  try {
    await api(kind === "bid" ? "/api/bid" : "/api/offer", "POST", { player: state.me, card, qty: 0, price: 0 });
    setError(""); await refresh();
  } catch (e) { setError(e.message); }
}

$("bid-card").onchange = () => updatePreview("bid-card", "bid-preview");
$("offer-card").onchange = () => updatePreview("offer-card", "offer-preview");

$("bid-form").onsubmit = async (e) => {
  e.preventDefault();
  try {
    await api("/api/bid", "POST", { player: state.me, card: Number($("bid-card").value), qty: Number($("bid-qty").value), price: toCents($("bid-price").value) });
    setError(""); await refresh();
  } catch (e) { setError(e.message); }
};
$("offer-form").onsubmit = async (e) => {
  e.preventDefault();
  try {
    await api("/api/offer", "POST", { player: state.me, card: Number($("offer-card").value), qty: Number($("offer-qty").value), price: toCents($("offer-price").value) });
    setError(""); await refresh();
  } catch (e) { setError(e.message); }
};

// Tabs
document.querySelectorAll(".tab").forEach((t) => {
  t.onclick = () => {
    activeTab = t.dataset.tab;
    document.querySelectorAll(".tab").forEach((x) => x.classList.toggle("active", x === t));
    $("tab-inventory").classList.toggle("hidden", activeTab !== "inventory");
    $("tab-market").classList.toggle("hidden", activeTab !== "market");
  };
});

// Filter bars: re-render the relevant view on any change.
document.querySelectorAll(".filters").forEach((box) => {
  const prefix = box.dataset.prefix;
  const rerender = () => (prefix === "inv" ? renderPlan() : renderGallery());
  box.addEventListener("input", rerender);
  box.addEventListener("change", rerender);
});
// Market sort direction toggle.
document.querySelector('.filters[data-prefix="mkt"] .f-dir').onclick = (e) => {
  const b = e.currentTarget;
  const dir = Number(b.dataset.dir || "-1") * -1;
  b.dataset.dir = dir;
  b.textContent = dir === 1 ? "▲" : "▼";
  renderGallery();
};

// Planning table header sorting.
$("plan").querySelectorAll("th[data-sort]").forEach((th) => {
  th.onclick = () => {
    const k = th.dataset.sort;
    if (planSortKey === k) planSortDir = -planSortDir;
    else { planSortKey = k; planSortDir = k === "name" || k === "type" ? 1 : -1; }
    renderPlan();
  };
});

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

// ---- Round timer countdown ----
function tickTimer() {
  const el = $("round-timer");
  if (!state || state.phase !== "bidding" || !timerDeadline) { el.textContent = ""; return; }
  const nowSec = Math.floor(Date.now() / 1000) + clockSkew;
  const rem = timerDeadline - nowSec;
  if (rem <= 0) { el.textContent = "⏱ closing…"; el.classList.add("urgent"); return; }
  const m = Math.floor(rem / 60), s = rem % 60;
  el.textContent = `⏱ ${m}:${String(s).padStart(2, "0")}`;
  el.classList.toggle("urgent", rem <= 10);
}
setInterval(tickTimer, 1000);

// ---- Live updates: refresh on any server-pushed change (SSE), with a slow
// poll as a safety net if the stream drops. ----
function connectEvents() {
  try {
    const es = new EventSource("/api/events");
    es.onmessage = () => refresh();
  } catch (e) { console.error(e); }
}
connectEvents();
refresh();
setInterval(refresh, 15000);
