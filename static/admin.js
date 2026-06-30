"use strict";

// TOKEN_KEY, $, fmtUSD, toCents, escapeHtml/esc come from util.js (loaded first).
let authToken = localStorage.getItem(TOKEN_KEY) || "";
let state = null;
let timerDeadline = null;
let clockSkew = 0;
let prevInGame = null; // tracks phase transitions for the New Game form's collapse

// isTrading / phaseLabel live in util.js (shared with app.js).

function toast(html, kind) {
  const t = document.createElement("div");
  t.className = "toast" + (kind ? " " + kind : "");
  t.innerHTML = html;
  $("toasts").appendChild(t);
  setTimeout(() => { t.classList.add("out"); setTimeout(() => t.remove(), 400); }, kind === "error" ? 7000 : 5000);
}
function toastError(msg) { toast(esc(msg), "error"); }

function setConn(live) {
  const el = $("conn");
  el.className = "conn " + (live ? "live" : "down");
  el.textContent = live ? "● live" : "● offline";
  el.title = live ? "Live updates connected" : "Reconnecting…";
}

function consumeMagicLink() {
  const params = new URLSearchParams(location.search);
  const t = params.get("t");
  if (!t) return;
  setToken(t);
  params.delete("t");
  history.replaceState({}, "", location.pathname + (params.toString() ? "?" + params : ""));
}

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

function setToken(token) {
  authToken = token || "";
  if (authToken) localStorage.setItem(TOKEN_KEY, authToken);
  else localStorage.removeItem(TOKEN_KEY);
}

async function refresh() {
  try {
    state = await api("/api/state");
    render();
    if (state.am_admin) await loadLedger();
    if (state.am_admin) await loadLadder();
  } catch (e) {
    console.error(e);
  }
}

function render() {
  if (!state) return;
  const inGame = state.phase !== "setup";

  // Once a game exists, demote the New Game form: relabel it as a reset action
  // and move it below the live management sections (collapsing it on the
  // transition, but never fighting the host once they re-open it).
  const setupSection = $("setup"), main = setupSection.parentElement;
  $("setup-toggle").textContent = inGame ? "⚠ Start a new game (resets the current one)" : "New Game";
  if (inGame && setupSection !== main.lastElementChild) main.appendChild(setupSection);
  else if (!inGame && setupSection !== main.firstElementChild) main.insertBefore(setupSection, main.firstElementChild);
  if (prevInGame !== inGame) { $("setup-details").open = !inGame; prevInGame = inGame; }

  if (!inGame) {
    $("status").textContent = "No game in progress.";
  } else if (state.phase === "finished") {
    $("status").textContent = `${state.set_name} — finished.`;
  } else {
    $("status").textContent = `${state.set_name} — ${phaseLabel(state.phase)} · round ${state.round} of ${state.total_rounds}`;
  }

  // Warn that running setup again replaces the live game (see btn-setup).
  $("setup-warn").classList.toggle("hidden", !inGame);

  // Auth bar
  $("auth").classList.remove("hidden");
  const me = state.me != null ? state.players.find((p) => p.id === state.me) : null;
  $("auth-status").textContent = me ? `${me.name}${state.am_admin ? " (host)" : ""}` : (inGame ? "not logged in" : "");
  ["login-name", "login-pass", "btn-pw-login", "token-input", "btn-login"].forEach((id) =>
    $(id).classList.toggle("hidden", !!me)
  );
  $("btn-logout").classList.toggle("hidden", !me);

  // Controls + ledger + tournament are host-only.
  $("controls").classList.toggle("hidden", !state.am_admin);
  $("manage").classList.toggle("hidden", !state.am_admin || !inGame);
  $("ledger-card").classList.toggle("hidden", !state.am_admin);
  $("trades-card").classList.toggle("hidden", !state.am_admin);
  $("ladder-card").classList.toggle("hidden", !state.am_admin || !inGame);
  $("deliveries-card").classList.toggle("hidden", !state.am_admin || !inGame);
  $("reports-card").classList.toggle("hidden", !state.am_admin);
  if (state.am_admin) renderReports();
  if (state.am_admin && inGame) {
    renderHouse();
    renderDeliveries();
    const cards = state.cards || [];
    const total = cards.reduce((s, c) => s + (c.supply || 0), 0);
    $("export-info").textContent = `${cards.length} distinct · ${total} copies`;
  }
  if (state.am_admin && inGame) {
    const timer = state.round_seconds ? ` · auto-close timer ${state.round_seconds}s` : "";
    $("round-info").textContent =
      state.phase === "finished"
        ? "The game is over."
        : `${phaseLabel(state.phase)} · round ${state.round} of ${state.total_rounds} is open for orders${timer}.`;
    $("btn-close").disabled = !isTrading(state);
  }

  timerDeadline = state.round_deadline ?? null;
  clockSkew = (state.server_now || 0) - Math.floor(Date.now() / 1000);
  tickTimer();
}

function tickTimer() {
  const el = $("round-timer");
  if (!isTrading(state) || !timerDeadline) { el.textContent = ""; return; }
  const rem = timerDeadline - (Math.floor(Date.now() / 1000) + clockSkew);
  if (rem <= 0) { el.textContent = "⏱ closing…"; el.classList.add("urgent"); return; }
  const m = Math.floor(rem / 60), s = rem % 60;
  el.textContent = `⏱ ${m}:${String(s).padStart(2, "0")}`;
  el.classList.toggle("urgent", rem <= 10);
}

async function loadLedger() {
  try {
    const log = await api("/api/log");
    renderLedger(log.orders);
    renderHistory(log.trades);
  } catch (e) {
    console.error(e);
  }
}

function renderLedger(orders) {
  const tb = $("ledger").querySelector("tbody");
  tb.innerHTML = "";
  if (!orders.length) {
    tb.innerHTML = `<tr><td colspan="7" class="muted">No orders yet.</td></tr>`;
    return;
  }
  [...orders].reverse().forEach((o) => {
    const tr = document.createElement("tr");
    const cls = o.kind === "bid" ? "buyer" : "seller";
    const action = `<span class="${cls}">${o.kind} ${o.action === "place" ? "placed" : "cancelled"}</span>`;
    const qty = o.action === "place" ? `×${o.qty}` : "—";
    const price = o.action === "place" ? fmtUSD(o.price) : "—";
    tr.innerHTML =
      `<td class="num muted">${o.seq}</td><td>${o.round}</td>` +
      `<td>${escapeHtml(o.player_name)}</td><td>${action}</td>` +
      `<td>${escapeHtml(o.card_name)}</td><td class="num">${qty}</td><td class="num">${price}</td>`;
    tb.appendChild(tr);
  });
}

function renderHistory(history) {
  const div = $("history");
  div.innerHTML = "";
  if (!history.length) {
    div.innerHTML = `<p class="muted">No auctions closed yet.</p>`;
    return;
  }
  [...history].reverse().forEach((r) => {
    const block = document.createElement("div");
    block.className = "round-block";
    const h = document.createElement("h4");
    h.textContent = `Round ${r.round}`;
    block.appendChild(h);
    if (!r.trades.length) {
      block.innerHTML += `<p class="muted">No orders crossed.</p>`;
    } else {
      r.trades.forEach((t) => {
        const line = document.createElement("div");
        line.className = "trade";
        line.innerHTML =
          `<span class="buyer">${escapeHtml(t.buyer_name)}</span> bought ${t.qty}× ` +
          `<b>${escapeHtml(t.card_name)}</b> from <span class="seller">${escapeHtml(t.seller_name)}</span> ` +
          `@ ${fmtUSD(t.price)} <span class="muted">(bid ${fmtUSD(t.bid)} / offer ${fmtUSD(t.offer)})</span>`;
        block.appendChild(line);
      });
    }
    div.appendChild(block);
  });
}

// ---- ELO ladder ----
// Rendered in the host's local timezone (slots are UTC instants server-side).
function fmtSlot(epoch) {
  return new Date(epoch * 1000).toLocaleString(undefined, { weekday: "short", month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
}

async function loadLadder() {
  try { renderLadder(await api("/api/ladder")); } catch (e) { console.error(e); }
}

// Host override controls (report finalises immediately, no confirmation).
function overrideControls(m) {
  return `<span class="report-row">
    <input class="rep-aw" type="number" min="0" value="${m.a_wins || 2}" data-mid="${m.id}" title="${esc(m.a_name)} game wins" />
    – <input class="rep-bw" type="number" min="0" value="${m.b_wins || 0}" data-mid="${m.id}" title="${esc(m.b_name)} game wins" />
    <input class="rep-d" type="number" min="0" value="${m.draws || 0}" data-mid="${m.id}" title="draws" />
    <button class="primary rep-go" data-mid="${m.id}">set result</button>
  </span>`;
}

function renderLadder(l) {
  const matches = l.matches || [];
  const sched = matches.filter((m) => m.status === "scheduled").length;
  $("ladder-info").textContent = `${sched} upcoming match${sched === 1 ? "" : "es"} · the scheduler also runs automatically as availability changes.`;

  const stb = $("standings").querySelector("tbody");
  stb.innerHTML = "";
  (l.standings || []).forEach((s) => {
    const tr = document.createElement("tr");
    tr.innerHTML =
      `<td>${s.rank}</td><td>${esc(s.name)}</td><td class="num">${s.elo}</td>` +
      `<td class="num">${s.wins}-${s.losses}-${s.draws}</td><td class="num">${s.scheduled}</td><td class="num">${s.cancellations}</td>`;
    stb.appendChild(tr);
  });

  const box = $("ladder-matches");
  box.innerHTML = "";
  if (!matches.length) {
    box.innerHTML = `<p class="muted">No matches yet — players set availability and a weekly target on the game page.</p>`;
    return;
  }
  const tbl = document.createElement("table");
  tbl.className = "grid";
  tbl.innerHTML = `<thead><tr><th>When</th><th>Match</th><th>Result / override</th></tr></thead>`;
  const body = document.createElement("tbody");
  matches.slice().sort((a, b) => a.slot_start - b.slot_start).forEach((m) => {
    const tr = document.createElement("tr");
    const td = (html) => { const d = document.createElement("td"); d.innerHTML = html; return d; };
    tr.appendChild(td(fmtSlot(m.slot_start)));
    tr.appendChild(td(`${esc(m.a_name)} <span class="muted">vs</span> ${esc(m.b_name)}`));
    if (m.status === "completed") {
      tr.appendChild(td(`<b>${m.a_wins}–${m.b_wins}</b>`));
    } else if (m.status === "cancelled") {
      const who = m.cancelled_by === m.a ? m.a_name : m.b_name;
      tr.appendChild(td(`<span class="muted">cancelled by ${esc(who)}</span>`));
    } else if (m.status === "expired") {
      tr.appendChild(td(`<div class="muted">expired (no-show) — record it if it was played:</div>${overrideControls(m)}`));
    } else {
      const pending = m.proposed_by != null ? `<div class="muted">reported ${m.a_wins}–${m.b_wins}, awaiting confirmation</div>` : "";
      tr.appendChild(td(pending + overrideControls(m)));
    }
    body.appendChild(tr);
  });
  tbl.appendChild(body);
  box.appendChild(tbl);
}

$("btn-schedule").onclick = async () => {
  try {
    const r = await api("/api/ladder/schedule", "POST", {});
    $("tourney-error").textContent = "";
    toast(`Scheduled ${r.created} new match${r.created === 1 ? "" : "es"}.`);
    await refresh();
  } catch (e) { $("tourney-error").textContent = e.message; }
};

// Delegated: host sets a result directly (override).
$("ladder-matches").addEventListener("click", async (e) => {
  const go = e.target.closest(".rep-go");
  if (!go) return;
  const mid = Number(go.dataset.mid);
  const val = (cls) => Math.max(0, Number(document.querySelector(`.${cls}[data-mid="${mid}"]`).value) || 0);
  try {
    await api("/api/ladder/report", "POST", { match_id: mid, a_wins: val("rep-aw"), b_wins: val("rep-bw"), draws: val("rep-d") });
    $("tourney-error").textContent = "";
    await refresh();
  } catch (err) { $("tourney-error").textContent = err.message; }
});

function showTokens(players) {
  const tb = $("token-table").querySelector("tbody");
  tb.innerHTML = "";
  players.forEach((p) => {
    // A magic link logs that player in directly (the host link points at /admin).
    const link = `${location.origin}/${p.admin ? "admin" : ""}?t=${encodeURIComponent(p.token)}`;
    const tr = document.createElement("tr");
    tr.innerHTML = `<td>${esc(p.name)}${p.admin ? " (host)" : ""}</td>`;
    const td = document.createElement("td");
    const input = document.createElement("input");
    input.className = "linkfield"; input.readOnly = true; input.value = link;
    input.onclick = () => input.select();
    const btn = document.createElement("button");
    btn.className = "ghost copy"; btn.textContent = "copy link";
    btn.onclick = async () => {
      try { await navigator.clipboard.writeText(link); toast("Link copied — share it privately."); }
      catch { input.select(); toast("Press Ctrl/Cmd-C to copy."); }
    };
    td.appendChild(input); td.appendChild(btn);
    tr.appendChild(td);
    tb.appendChild(tr);
  });
  $("tokens").classList.remove("hidden");
}

// ---- Actions ----

$("btn-login").onclick = async () => {
  const token = $("token-input").value.trim();
  if (!token) return;
  try {
    await api("/api/login", "POST", { token });
    setToken(token);
    $("token-input").value = "";
    await refresh();
  } catch (e) { toastError(`Login failed: ${e.message}`); }
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

$("btn-logout").onclick = async () => { setToken(""); await refresh(); };

// Card-pool source: show only the relevant inputs.
function selectedPool() {
  const r = document.querySelector('input[name="pool"]:checked');
  return r ? r.value : "sample";
}
function syncPoolPanes() {
  const pool = selectedPool();
  document.querySelectorAll(".pool-pane").forEach((p) => {
    const which = p.dataset.pool;
    const show = which === pool || (which === "packs" && pool !== "manual");
    p.hidden = !show;
  });
}
document.querySelectorAll('input[name="pool"]').forEach((r) => (r.onchange = syncPoolPanes));
syncPoolPanes();

// ---- player list: one input per player (first is the host) ----
function playerNames() {
  return Array.from($("players-list").querySelectorAll(".player-name")).map((i) => i.value.trim()).filter(Boolean);
}
// Tag the first row "host"; clear the tag from any others.
function markHostRow() {
  Array.from($("players-list").children).forEach((row, i) => {
    let tag = row.querySelector(".host-tag");
    if (i === 0 && !tag) { tag = document.createElement("span"); tag.className = "host-tag"; tag.textContent = "host"; row.insertBefore(tag, row.firstChild); }
    else if (i !== 0 && tag) tag.remove();
  });
}
function addPlayerRow(name = "", focus = false) {
  const row = document.createElement("div");
  row.className = "player-row";
  const input = document.createElement("input");
  input.type = "text"; input.className = "player-name"; input.value = name;
  input.placeholder = "player name"; input.autocomplete = "off";
  // Enter adds (and jumps to) the next row, so a host can rattle off names.
  input.addEventListener("keydown", (e) => { if (e.key === "Enter") { e.preventDefault(); addPlayerRow("", true); } });
  const del = document.createElement("button");
  del.type = "button"; del.className = "ghost player-del"; del.title = "remove player"; del.textContent = "×";
  del.addEventListener("click", () => {
    if ($("players-list").children.length <= 1) { input.value = ""; }  // keep at least one field
    else row.remove();
    markHostRow(); setupPreview();
  });
  row.append(input, del);
  $("players-list").appendChild(row);
  markHostRow();
  if (focus) input.focus();
  setupPreview();
  return input;
}
$("btn-add-player-row").onclick = () => addPlayerRow("", true);
["Alice", "Bob", "Carol", "Dave"].forEach((n) => addPlayerRow(n));

// A round timer entered as a number + a unit (min/hours/days) → whole seconds.
// `id` is the number input; its unit <select> is `${id}-unit` (value = seconds
// per unit). 0 means "manual close only".
function durationSeconds(id) {
  const n = Math.max(0, Number($(id).value) || 0);
  const per = Number($(id + "-unit").value) || 60;
  return Math.round(n * per);
}

// The ladder block hours are entered in the host's local time but stored as
// fixed UTC hours (so every viewer can render them in their own timezone).
// Convert a "HH:MM" local value to the equivalent whole UTC hour.
function blockHourToUtc(timeStr) {
  const h = Number((timeStr || "0:0").split(":")[0]) || 0;
  const d = new Date();
  d.setHours(h, 0, 0, 0);
  return d.getUTCHours();
}
// Echo what the two slots become in UTC so the host can see the conversion.
function updateBlockHint() {
  const m = blockHourToUtc($("cfg-block-morning").value);
  const e = blockHourToUtc($("cfg-block-evening").value);
  const fmt = (h) => String(h).padStart(2, "0") + ":00";
  $("cfg-block-hint").innerHTML =
    `The two daily availability slots, in <strong>your</strong> local time ` +
    `(stored as ${fmt(m)} / ${fmt(e)} UTC). Players see them in their own timezone.`;
}
$("cfg-block-morning").addEventListener("input", updateBlockHint);
$("cfg-block-evening").addEventListener("input", updateBlockHint);
updateBlockHint();

// Live setup preview + inline validation. Recomputes a one-line summary of what
// "Open packs & deal" will do, and blocks submit (with the reason) while the
// form has a problem the server would reject anyway.
function setupPreview() {
  const pool = selectedPool();
  const names = playerNames();
  const primaryRounds = Number($("cfg-primary-rounds").value);
  const secondaryRounds = Number($("cfg-secondary-rounds").value);
  const problems = [];

  if (names.length < 2) problems.push("add at least 2 players");
  if (new Set(names.map((n) => n.toLowerCase())).size !== names.length) problems.push("player names must be unique");
  if (!(primaryRounds >= 1) || !(secondaryRounds >= 1)) problems.push("each phase needs at least 1 round");

  let opened = null, openedLabel = "opened";
  if (pool === "manual") {
    opened = parseCardList($("cfg-cardlist").value).reduce((s, r) => s + (r.qty > 0 ? r.qty : 0), 0);
    openedLabel = "listed";
    if (opened === 0) problems.push("paste a card list (one “qty name” per line)");
  } else {
    if (pool === "scryfall" && !$("cfg-set").value.trim()) problems.push("enter a Scryfall set code");
    const packs = Number($("cfg-packs").value), size = Number($("cfg-packsize").value);
    if (packs >= 1 && size >= 1) opened = packs * size;
    else problems.push("packs and cards per pack must be ≥ 1");
  }

  const deals = ["c", "u", "r", "m"].map((k) => Number($("cfg-deal-" + k).value) || 0);
  const perPlayer = deals.reduce((a, b) => a + b, 0);

  let summary = "";
  if (opened != null && names.length) {
    summary = `${names.length} player${names.length === 1 ? "" : "s"} · ${opened} card${opened === 1 ? "" : "s"} ${openedLabel}`;
    summary += perPlayer === 0
      ? " · dealt round-robin (nothing held to the house)"
      : ` · dealing up to ${deals.join("/")} per player (≤${perPlayer} each) → leftovers to the house`;
  }

  const el = $("setup-preview"), btn = $("btn-setup");
  if (problems.length) {
    el.textContent = "Can’t start yet — " + problems.join("; ") + ".";
    el.classList.add("bad");
    btn.disabled = true;
  } else {
    el.textContent = summary;
    el.classList.remove("bad");
    btn.disabled = false;
  }
}

// Recompute on any edit within the setup form (covers typing, number steppers,
// and the pool radios); also after programmatic card-list edits below.
$("setup").addEventListener("input", setupPreview);
$("setup").addEventListener("change", setupPreview);
setupPreview();

// Roll a fresh seed (any non-negative integer reproduces a distinct deal).
$("btn-seed-rand").onclick = () => {
  $("cfg-seed").value = (typeof crypto !== "undefined" && crypto.getRandomValues)
    ? crypto.getRandomValues(new Uint32Array(1))[0]
    : Math.floor(Math.random() * 0xffffffff);
};

// ---- card picker: build the manual list from a set's card list ----
let pickerCards = [];

$("btn-load-set").onclick = async () => {
  const code = $("picker-set").value.trim() || "sample";
  const btn = $("btn-load-set");
  btn.disabled = true;
  $("picker-msg").textContent = "Loading…";
  try {
    const r = await api(`/api/set-cards?set=${encodeURIComponent(code)}`);
    pickerCards = r.cards || [];
    $("picker-msg").textContent = `${r.set_name}: ${pickerCards.length} cards. Click + to add (or type a quantity first).`;
    $("picker-tools").classList.toggle("hidden", pickerCards.length === 0);
    $("picker-filter").value = "";
    renderPicker();
  } catch (e) {
    pickerCards = [];
    $("picker-tools").classList.add("hidden");
    $("picker-list").innerHTML = "";
    $("picker-msg").textContent = `Could not load set: ${e.message}`;
  } finally {
    btn.disabled = false;
  }
};

// Colour-identity filter — shared with the player pages (see util.js for the
// at-most / at-least / exactly semantics).
function shownPickerCards() {
  const q = $("picker-filter").value.trim().toLowerCase();
  const f = readColorFilter($("picker-colors"));
  return pickerCards.filter((c) => (!q || c.name.toLowerCase().includes(q)) && matchesColorIdentity(c, f));
}

function renderPicker() {
  const list = $("picker-list");
  list.innerHTML = "";
  const cards = shownPickerCards();
  if (cards.length === 0) { list.innerHTML = `<p class="muted">No matching cards.</p>`; return; }
  cards.forEach((c) => {
    const row = document.createElement("div");
    row.className = "picker-row";
    row.innerHTML =
      `<input type="number" class="picker-qty" min="1" value="1" title="quantity" />` +
      `<button type="button" class="picker-add" title="add to list">+</button>` +
      `<span class="picker-name">${esc(c.name)}</span>` +
      `<span class="picker-colorcell">${colorPips(c.colors)}</span>` +
      `<span class="picker-rarity rarity-${c.rarity}">${c.rarity[0].toUpperCase()}</span>` +
      `<span class="picker-ref muted">${c.ref_price != null ? fmtUSD(c.ref_price) : "—"}</span>`;
    row.querySelector(".picker-add").onclick = () => {
      const qty = Math.max(1, Number(row.querySelector(".picker-qty").value) || 1);
      addToCardList([{ name: c.name, qty }]);
    };
    list.appendChild(row);
  });
}

$("picker-filter").oninput = renderPicker;
// Toggle colour buttons (the ✕ clears them all), or change the match mode.
$("picker-colors").addEventListener("click", (e) => handleColorClick($("picker-colors"), e, renderPicker));
$("picker-colors").addEventListener("change", (e) => { if (e.target.classList.contains("f-cmode")) renderPicker(); });
$("btn-add-all").onclick = () => addToCardList(shownPickerCards().map((c) => ({ name: c.name, qty: 1 })));

// Parse the textarea into ordered {qty,name} card rows (ignoring comments/blanks).
function parseCardList(text) {
  const rows = [];
  text.split(/\r?\n/).forEach((line) => {
    const t = line.trim();
    if (!t || t.startsWith("#") || t.startsWith("//")) return;
    const m = t.match(/^(\d+)\s*x?\s+(.+)$/i);
    if (m) rows.push({ qty: parseInt(m[1], 10), name: m[2].trim() });
    else rows.push({ qty: 1, name: t });
  });
  return rows;
}

// Merge additions into the card-list textarea (summing quantities by name),
// switch the pool source to "manual", and keep focus on building.
function addToCardList(additions) {
  const rows = parseCardList($("cfg-cardlist").value);
  const byName = new Map(rows.map((r) => [r.name.toLowerCase(), r]));
  additions.forEach((a) => {
    const ex = byName.get(a.name.toLowerCase());
    if (ex) ex.qty += a.qty;
    else { const r = { qty: a.qty, name: a.name }; rows.push(r); byName.set(a.name.toLowerCase(), r); }
  });
  $("cfg-cardlist").value = rows.filter((r) => r.qty > 0).map((r) => `${r.qty} ${r.name}`).join("\n");
  const manual = document.querySelector('input[name="pool"][value="manual"]');
  if (manual && !manual.checked) { manual.checked = true; syncPoolPanes(); }
  setupPreview(); // programmatic edits don't fire the form's input listener
}

$("btn-setup").onclick = async () => {
  const pool = selectedPool();
  // A blank Scryfall code used to fall back to the sample set silently; make
  // the host fix it instead of quietly drafting a different pool.
  if (pool === "scryfall" && !$("cfg-set").value.trim()) {
    toastError("Enter a Scryfall set code (e.g. dom), or pick another card pool source.");
    $("cfg-set").focus();
    return;
  }
  // Running setup on a live game wipes it (players, holdings, orders, tokens).
  if (state && state.phase !== "setup" &&
      !confirm("Start a new game? This replaces the game in progress and invalidates every player's current token.")) {
    return;
  }
  const names = playerNames();
  const config = {
    player_names: names,
    pool_source: pool,
    set: $("cfg-set").value.trim() || "sample",
    card_list: $("cfg-cardlist").value,
    starting_money: toCents($("cfg-money").value),
    debt_limit: toCents($("cfg-debt").value),
    primary_rounds: Number($("cfg-primary-rounds").value),
    secondary_rounds: Number($("cfg-secondary-rounds").value),
    primary_round_seconds: durationSeconds("cfg-primary-timer"),
    secondary_round_seconds: durationSeconds("cfg-secondary-timer"),
    num_packs: Number($("cfg-packs").value),
    pack_size: Number($("cfg-packsize").value),
    seed: Number($("cfg-seed").value),
    deal_commons: Number($("cfg-deal-c").value) || 0,
    deal_uncommons: Number($("cfg-deal-u").value) || 0,
    deal_rares: Number($("cfg-deal-r").value) || 0,
    deal_mythics: Number($("cfg-deal-m").value) || 0,
    house_offer_stdev_pct: Number($("cfg-house-stdev").value) || 0,
    house_offer_cap_pct: Number($("cfg-house-cap").value) || 0,
    starting_elo: Number($("cfg-elo-start").value),
    elo_k: Number($("cfg-elo-k").value),
    cancel_penalty: Number($("cfg-elo-cancel").value),
    max_games_per_week: Number($("cfg-elo-maxgames").value),
    schedule_window_days: Number($("cfg-elo-window").value),
    ladder_block_hours: [blockHourToUtc($("cfg-block-morning").value), blockHourToUtc($("cfg-block-evening").value)],
  };
  const btn = $("btn-setup");
  btn.disabled = true;
  btn.textContent = "Fetching & dealing…";
  try {
    const resp = await api("/api/setup", "POST", config);
    const host = resp.players.find((p) => p.admin) || resp.players[0];
    setToken(host.token);
    showTokens(resp.players);
    $("setup-details").open = false; // tuck the form away now a game is running
    await refresh();
  } catch (e) {
    toastError(e.message);
  } finally {
    btn.textContent = "Open packs & deal";
    setupPreview(); // re-enable only if the form is still valid
  }
};

// ---- mid-game management ----
$("btn-offer-house").onclick = async () => {
  try {
    const r = await api("/api/house/offer", "POST", {});
    toast(`Listed ${r.listed} house card${r.listed === 1 ? "" : "s"}.`);
    await refresh();
  } catch (e) { $("manage-error").textContent = e.message; }
};

$("btn-add-cards").onclick = async () => {
  const card_list = $("add-cardlist").value;
  if (!card_list.trim()) return;
  const btn = $("btn-add-cards");
  btn.disabled = true;
  try {
    const r = await api("/api/cards/add", "POST", { card_list });
    toast(`Added ${r.added} card${r.added === 1 ? "" : "s"} to the house.`);
    $("add-cardlist").value = "";
    await refresh();
  } catch (e) { $("manage-error").textContent = e.message; }
  finally { btn.disabled = false; }
};

$("btn-add-player").onclick = async () => {
  const name = $("add-player-name").value.trim();
  if (!name) return;
  try {
    const r = await api("/api/players/add", "POST", { name });
    $("add-player-name").value = "";
    showTokens([{ id: r.player, name: r.name, token: r.token, admin: false }]);
    toast(`Added ${esc(r.name)}.`);
    await refresh();
  } catch (e) { $("manage-error").textContent = e.message; }
};

function renderHouse() {
  const tb = $("house-table").querySelector("tbody");
  tb.innerHTML = "";
  const house = (state && state.house) || [];
  const total = house.reduce((s, h) => s + h.qty, 0);
  $("house-info").textContent = house.length
    ? `${total} card${total === 1 ? "" : "s"} held across ${house.length} name${house.length === 1 ? "" : "s"} · house balance ${fmtUSD(state.house_balance || 0)}`
    : "The house holds no unallocated cards.";
  house.forEach((h) => {
    const tr = document.createElement("tr");
    tr.innerHTML = `<td>${esc(h.name)}</td><td class="num">×${h.qty}</td>`;
    tb.appendChild(tr);
  });
}

// ---- card export ----
function downloadFile(filename, text, mime) {
  const blob = new Blob([text], { type: mime });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url; a.download = filename;
  document.body.appendChild(a); a.click(); a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
function exportSlug() {
  return (state && state.set_name || "cards").replace(/[^a-z0-9]+/gi, "-").replace(/^-|-$/g, "").toLowerCase() || "cards";
}
function sortedCards() {
  return [...((state && state.cards) || [])].sort((a, b) => a.name.localeCompare(b.name));
}
// A `quantity name` decklist of the whole pool — pastes back into a new game.
function exportDecklist() {
  const lines = sortedCards().filter((c) => c.supply > 0).map((c) => `${c.supply} ${c.name}`);
  if (!lines.length) return;
  downloadFile(`${exportSlug()}-decklist.txt`, lines.join("\n") + "\n", "text/plain");
}
// A richer CSV of the card catalog.
function exportCsv() {
  const cards = sortedCards();
  if (!cards.length) return;
  const cell = (v) => {
    const s = v == null ? "" : String(v);
    return /[",\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
  };
  const rows = [["name", "rarity", "supply", "mana_value", "type", "ref_price_usd"]];
  cards.forEach((c) => rows.push([
    c.name, c.rarity, c.supply, c.cmc ?? "", c.type_line ?? "",
    c.ref_price != null ? (c.ref_price / 100).toFixed(2) : "",
  ]));
  downloadFile(`${exportSlug()}-cards.csv`, rows.map((r) => r.map(cell).join(",")).join("\n") + "\n", "text/csv");
}
$("btn-export-decklist").onclick = exportDecklist;
$("btn-export-csv").onclick = exportCsv;

function deliveryDeadline(d) {
  if (d.status !== "pending") return "—";
  const now = (state && state.server_now) || Math.floor(Date.now() / 1000);
  const rem = d.deadline - now;
  if (rem <= 0) return "overdue";
  const days = Math.floor(rem / 86400), hrs = Math.floor((rem % 86400) / 3600);
  return `${days > 0 ? days + "d " : ""}${hrs}h`;
}

function renderDeliveries() {
  const tb = $("deliveries-table").querySelector("tbody");
  tb.innerHTML = "";
  const ds = (state && state.all_deliveries) || [];
  if (!ds.length) {
    tb.innerHTML = `<tr><td colspan="8" class="muted">No deliveries yet.</td></tr>`;
    return;
  }
  [...ds].reverse().forEach((d) => {
    const tr = document.createElement("tr");
    const reverseBtn = d.status === "reversed" ? "" : `<button class="ghost d-reverse" data-id="${d.id}">Reverse</button>`;
    const note = d.note ? `<div class="muted">${esc(d.note)}</div>` : "";
    tr.innerHTML =
      `<td>${esc(d.card_name)}</td><td class="num">×${d.qty}</td>` +
      `<td>${esc(d.seller_name)}</td><td>${esc(d.buyer_name)}</td>` +
      `<td class="num">${fmtUSD(d.total)}</td>` +
      `<td class="dstat-${d.status}">${d.status}${note}</td>` +
      `<td>${deliveryDeadline(d)}</td><td>${reverseBtn}</td>`;
    tb.appendChild(tr);
  });
}

$("deliveries-table").addEventListener("click", async (e) => {
  const b = e.target.closest(".d-reverse");
  if (!b) return;
  if (!confirm("Reverse this delivery? Cards and money are returned (no penalty).")) return;
  try {
    await api("/api/deliveries/reverse", "POST", { delivery_id: Number(b.dataset.id) });
    await refresh();
  } catch (err) { $("deliveries-error").textContent = err.message; }
});

function renderReports() {
  const box = $("reports-list");
  const reports = (state && state.reports) || [];
  const open = reports.filter((r) => !r.resolved).length;
  $("reports-count").textContent = reports.length
    ? `(${open} open · ${reports.length} total)` : "(none yet)";
  if (!reports.length) { box.innerHTML = `<p class="muted">No feedback submitted yet.</p>`; return; }
  // Open first, then newest first within each group.
  const sorted = [...reports].sort((a, b) => (a.resolved - b.resolved) || (b.created - a.created));
  box.innerHTML = sorted.map((r) => {
    const when = new Date(r.created * 1000).toLocaleString();
    const tag = r.kind === "bug" ? `<span class="rep-bug">🐞 bug</span>` : `<span class="rep-feature">✨ feature</span>`;
    return `<div class="report-row ${r.resolved ? "resolved" : ""}">
        <div class="report-meta">${tag} <span class="muted">— ${esc(r.reporter_name)} · ${esc(when)}</span></div>
        <div class="report-body">${esc(r.text)}</div>
        <div class="report-row-actions">
          <button class="ghost rep-amend" data-id="${r.id}">Amend</button>
          <button class="ghost rep-toggle" data-id="${r.id}" data-resolved="${r.resolved ? 1 : 0}">${r.resolved ? "Reopen" : "Mark done"}</button>
          <button class="ghost rep-del" data-id="${r.id}">Delete</button>
        </div>
      </div>`;
  }).join("");
}

// Swap a report row into an inline edit form (kind + text) for the host.
function enterAmendReport(row, id) {
  const r = ((state && state.reports) || []).find((x) => x.id === Number(id));
  if (!r) return;
  row.querySelector(".report-body").innerHTML =
    `<select class="rep-kind">
       <option value="bug"${r.kind === "bug" ? " selected" : ""}>🐞 bug</option>
       <option value="feature"${r.kind === "feature" ? " selected" : ""}>✨ feature</option>
     </select>
     <textarea class="rep-text" rows="3">${esc(r.text)}</textarea>`;
  row.querySelector(".report-row-actions").innerHTML =
    `<button class="primary rep-save" data-id="${id}">Save</button>
     <button class="ghost rep-cancel">Cancel</button>`;
  row.querySelector(".rep-text").focus();
}

$("reports-list").addEventListener("click", async (e) => {
  const amend = e.target.closest(".rep-amend");
  const save = e.target.closest(".rep-save");
  const cancel = e.target.closest(".rep-cancel");
  const toggle = e.target.closest(".rep-toggle");
  const del = e.target.closest(".rep-del");
  if (amend) { enterAmendReport(amend.closest(".report-row"), amend.dataset.id); return; }
  if (cancel) { renderReports(); return; }
  try {
    if (save) {
      const row = save.closest(".report-row");
      await api("/api/reports/amend", "POST", {
        report_id: Number(save.dataset.id),
        kind: row.querySelector(".rep-kind").value,
        text: row.querySelector(".rep-text").value,
      });
      await refresh();
    } else if (toggle) {
      await api("/api/reports/resolve", "POST", { report_id: Number(toggle.dataset.id), resolved: toggle.dataset.resolved !== "1" });
      await refresh();
    } else if (del) {
      if (!confirm("Delete this feedback permanently?")) return;
      await api("/api/reports/delete", "POST", { report_id: Number(del.dataset.id) });
      await refresh();
    }
  } catch (err) { $("reports-error").textContent = err.message; }
});

$("btn-tokens-done").onclick = () => $("tokens").classList.add("hidden");

$("btn-close").onclick = async () => {
  if (!confirm("Close the auction and match all orders?")) return;
  try {
    await api("/api/close", "POST", {});
    await refresh();
  } catch (e) { $("ctrl-error").textContent = e.message; }
};

setInterval(tickTimer, 1000);

// Live updates: SSE + adaptive poll fallback (shared helper in util.js).
consumeMagicLink();
startLiveUpdates({ refresh, setConn });
