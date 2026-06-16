"use strict";

const TOKEN_KEY = "mtg_auction_token";
let authToken = localStorage.getItem(TOKEN_KEY) || "";
let state = null;
let timerDeadline = null;
let clockSkew = 0;

const $ = (id) => document.getElementById(id);

function fmtUSD(cents) {
  if (cents === null || cents === undefined) return "—";
  const neg = cents < 0;
  const v = Math.abs(cents);
  return (neg ? "-$" : "$") + Math.floor(v / 100) + "." + String(v % 100).padStart(2, "0");
}
function toCents(dollars) { return Math.round(parseFloat(dollars) * 100); }
function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
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
  } catch (e) {
    console.error(e);
  }
}

function render() {
  if (!state) return;
  const inGame = state.phase !== "setup";

  if (!inGame) {
    $("status").textContent = "No game in progress.";
  } else if (state.phase === "finished") {
    $("status").textContent = `${state.set_name} — finished after ${state.total_rounds} rounds.`;
  } else {
    $("status").textContent = `${state.set_name} — round ${state.round} of ${state.total_rounds}`;
  }

  // Auth bar
  $("auth").classList.remove("hidden");
  const me = state.me != null ? state.players.find((p) => p.id === state.me) : null;
  $("auth-status").textContent = me ? `${me.name}${state.am_admin ? " (host)" : ""}` : (inGame ? "not logged in" : "");
  $("token-input").classList.toggle("hidden", !!me);
  $("btn-login").classList.toggle("hidden", !!me);
  $("btn-logout").classList.toggle("hidden", !me);

  // Controls + ledger are host-only.
  $("controls").classList.toggle("hidden", !state.am_admin);
  $("ledger-card").classList.toggle("hidden", !state.am_admin);
  $("trades-card").classList.toggle("hidden", !state.am_admin);
  if (state.am_admin && inGame) {
    const timer = state.round_seconds ? ` · auto-close timer ${state.round_seconds}s` : "";
    $("round-info").textContent =
      state.phase === "finished"
        ? "The game is over."
        : `Round ${state.round} of ${state.total_rounds} is open for orders${timer}.`;
    $("btn-close").disabled = state.phase !== "bidding";
  }

  timerDeadline = state.round_deadline ?? null;
  clockSkew = (state.server_now || 0) - Math.floor(Date.now() / 1000);
  tickTimer();
}

function tickTimer() {
  const el = $("round-timer");
  if (!state || state.phase !== "bidding" || !timerDeadline) { el.textContent = ""; return; }
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

function showTokens(players) {
  const tb = $("token-table").querySelector("tbody");
  tb.innerHTML = "";
  players.forEach((p) => {
    const tr = document.createElement("tr");
    tr.innerHTML =
      `<td>${escapeHtml(p.name)}${p.admin ? " (host)" : ""}</td>` +
      `<td><code>${escapeHtml(p.token)}</code></td>`;
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
  } catch (e) { alert(e.message); }
};

$("btn-logout").onclick = async () => { setToken(""); await refresh(); };

$("btn-setup").onclick = async () => {
  const names = $("cfg-players").value.split(",").map((s) => s.trim()).filter(Boolean);
  const config = {
    player_names: names,
    set: $("cfg-set").value.trim() || "sample",
    starting_money: toCents($("cfg-money").value),
    debt_limit: toCents($("cfg-debt").value),
    rounds: Number($("cfg-rounds").value),
    round_seconds: Number($("cfg-timer").value),
    num_packs: Number($("cfg-packs").value),
    pack_size: Number($("cfg-packsize").value),
    seed: Number($("cfg-seed").value),
  };
  const btn = $("btn-setup");
  btn.disabled = true;
  btn.textContent = "Fetching set & dealing…";
  try {
    const resp = await api("/api/setup", "POST", config);
    const host = resp.players.find((p) => p.admin) || resp.players[0];
    setToken(host.token);
    showTokens(resp.players);
    await refresh();
  } catch (e) {
    alert(e.message);
  } finally {
    btn.disabled = false;
    btn.textContent = "Open packs & deal";
  }
};

$("btn-tokens-done").onclick = () => $("tokens").classList.add("hidden");

$("btn-close").onclick = async () => {
  if (!confirm("Close the auction and match all orders?")) return;
  try {
    await api("/api/close", "POST", {});
    await refresh();
  } catch (e) { $("ctrl-error").textContent = e.message; }
};

setInterval(tickTimer, 1000);

// Live updates via SSE, with a slow poll as a safety net.
try {
  const es = new EventSource("/api/events");
  es.onmessage = () => refresh();
} catch (e) { console.error(e); }

refresh();
setInterval(refresh, 15000);
