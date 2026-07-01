"use strict";

// Shared admin state and top-level rendering. TOKEN_KEY, $, fmtUSD, toCents,
// escapeHtml/esc come from util.js (loaded first); the section renderers live
// in admin-setup.js / admin-manage.js, loaded after this file.
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

  $("save-warn").classList.toggle("hidden", state.save_ok !== false);

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

setInterval(tickTimer, 1000);
