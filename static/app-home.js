"use strict";

// Home tab, dashboard (holdings/orders/forms), TODO tab, history, players,
// auth controls, and the tab bar. Shares state with app-core.js.
let activeTab = "home";

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

// ---- Home tab: card & order summaries plus the calendar ----
function myPlayerView() {
  return (state && state.me != null) ? state.players.find((p) => p.id === state.me) : null;
}
function stat(label, value) {
  return `<div class="stat"><div class="stat-val">${value}</div><div class="stat-label">${esc(label)}</div></div>`;
}

function renderHome() {
  renderHomeCards();
  renderHomeOrders();
}

function renderHomeCards() {
  const box = $("home-cards");
  if (!box) return;
  const me = myPlayerView();
  if (!me) { box.innerHTML = `<p class="muted">Log in to see your cards.</p>`; return; }
  const holds = (me.holdings || []).slice().sort((a, b) => a.name.localeCompare(b.name));
  const copies = holds.reduce((s, h) => s + h.qty, 0);
  const value = holds.reduce((s, h) => s + ((cardById[h.card] || {}).ref_price || 0) * h.qty, 0);
  box.innerHTML =
    `<div class="stat-row">` +
      stat("Balance", fmtUSD(me.balance)) +
      stat("Distinct", String(holds.length)) +
      stat("Copies", String(copies)) +
      stat("Ref value", fmtUSD(value)) +
    `</div>` +
    (holds.length
      ? `<table class="grid mini"><thead><tr><th>Card</th><th class="num">Qty</th><th class="num">Ref $</th></tr></thead><tbody>` +
        holds.map((h) => {
          const c = cardById[h.card] || {};
          const off = myOfferByCard[h.card] ? ` <span class="muted">(${myOfferByCard[h.card].qty} offered)</span>` : "";
          return `<tr data-card="${h.card}"><td>${esc(h.name)} <span class="pips">${colorPips(c.colors || "")}</span>${off}</td>` +
            `<td class="num">×${h.qty}</td><td class="num">${fmtUSD(c.ref_price ?? null)}</td></tr>`;
        }).join("") +
        `</tbody></table>`
      : `<p class="muted">You don't hold any cards yet.</p>`);
}

function renderHomeOrders() {
  const box = $("home-orders");
  if (!box) return;
  const me = myPlayerView();
  if (!me) { box.innerHTML = `<p class="muted">Log in to see your orders.</p>`; return; }
  const bids = state.my_bids || [], offers = state.my_offers || [];
  const row = (o, kind) => {
    const tag = kind === "bid"
      ? `<span class="ord-badge buy">bid</span>` : `<span class="ord-badge sell">ask</span>`;
    return `<tr data-card="${o.card}"><td>${tag} ${esc(o.name)}</td><td class="num">×${o.qty}</td><td class="num">@${fmtUSD(o.price)}</td></tr>`;
  };
  box.innerHTML =
    `<div class="stat-row">` +
      stat("Open bids", String(bids.length)) +
      stat("Open offers", String(offers.length)) +
      stat("Committed", fmtUSD(state.my_committed)) +
      stat("Available", fmtUSD(state.my_available)) +
    `</div>` +
    (bids.length || offers.length
      ? `<table class="grid mini"><tbody>` +
        bids.slice().sort((a, b) => a.name.localeCompare(b.name)).map((o) => row(o, "bid")).join("") +
        offers.slice().sort((a, b) => a.name.localeCompare(b.name)).map((o) => row(o, "offer")).join("") +
        `</tbody></table>`
      : `<p class="muted">No open orders. Place bids and offers from the Market or Inventory.</p>`);
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

// ---- TODO tab: checklist, deliveries, and the combined schedule ----
function renderTodo() {
  renderTodoChecklist();
  renderDeliveries();
  renderTodoSchedule();
}

// Action items the player still needs to handle (used for the list and badge).
function todoActions() {
  const me = state && state.me;
  if (me == null) return [];
  const items = [];
  if (!state.my_has_password) items.push({ text: "Set a login password so you can log in by name", done: false });
  if (state.phase === "primary") items.push({ text: "Acquire your cards — the bank is issuing them in the primary phase", done: false });
  const ds = state.my_deliveries || [];
  const incoming = ds.filter((d) => d.buyer === me && d.status === "pending").length;
  const outgoing = ds.filter((d) => d.seller === me && d.status === "pending").length;
  if (incoming) items.push({ text: `Confirm ${incoming} delivery${incoming === 1 ? "" : " deliveries"} you've received`, done: false });
  if (outgoing) items.push({ text: `Hand off ${outgoing} card lot${outgoing === 1 ? "" : "s"} to buyers before the deadline`, done: false });
  if (state.phase === "secondary" || state.phase === "finished") {
    const hasAvail = !!(ladder && (ladder.my_availability || []).length);
    items.push({ text: "Set your ladder availability so games get scheduled", done: hasAvail });
  }
  return items;
}

function renderTodoChecklist() {
  const ul = $("todo-list");
  if (!ul) return;
  if (!state || state.me == null) { ul.innerHTML = `<li class="muted">Log in to see your to-do list.</li>`; return; }
  const items = todoActions();
  const open = items.filter((i) => !i.done).length;
  const badge = $("todo-badge");
  badge.textContent = open ? String(open) : "";
  badge.classList.toggle("hidden", open === 0);
  ul.innerHTML = items.length
    ? items.map((i) => `<li class="todo-item ${i.done ? "done" : "open"}">${i.done ? "✓" : "○"} ${esc(i.text)}</li>`).join("")
    : `<li class="muted">All caught up — nothing to do right now.</li>`;
}

function deliveryDue(d) {
  if (d.status !== "pending") return "";
  const now = (state && state.server_now) || Math.floor(Date.now() / 1000);
  const rem = d.deadline - now;
  if (rem <= 0) return `<span class="overdue">overdue</span>`;
  const days = Math.floor(rem / 86400), hrs = Math.floor((rem % 86400) / 3600);
  return `<span class="muted">due in ${days > 0 ? days + "d " : ""}${hrs}h</span>`;
}

function renderDeliveries() {
  const box = $("todo-deliveries");
  if (!box) return;
  const me = state && state.me;
  if (me == null) { box.innerHTML = `<p class="muted">Log in to see your deliveries.</p>`; return; }
  const ds = state.my_deliveries || [];
  if (!ds.length) { box.innerHTML = `<p class="muted">No deliveries yet — they appear when your orders fill.</p>`; return; }
  const row = (d, incoming) => {
    const other = incoming ? d.seller_name : d.buyer_name;
    const status = d.status === "pending" ? deliveryDue(d)
      : d.status === "received" ? `<span class="ok">received</span>`
      : `<span class="reversed">reversed</span>`;
    const action = incoming && d.status === "pending"
      ? `<button class="buy d-receive" data-id="${d.id}">Mark received</button>` : "";
    const note = d.note ? `<div class="muted delivery-note">${esc(d.note)}</div>` : "";
    return `<div class="delivery-row"><div class="delivery-what"><b>${d.qty}× ${esc(d.card_name)}</b> ` +
      `<span class="muted">${incoming ? "from" : "to"} ${esc(other)} · ${fmtUSD(d.total)}</span></div>` +
      `<div class="delivery-status">${status} ${action}</div>${note}</div>`;
  };
  const incoming = ds.filter((d) => d.buyer === me);
  const outgoing = ds.filter((d) => d.seller === me);
  let html = "";
  if (incoming.length) html += `<h4>To pick up <span class="muted">— confirm once you have the cards</span></h4>` + incoming.map((d) => row(d, true)).join("");
  if (outgoing.length) html += `<h4>To deliver <span class="muted">— hand the cards over before the deadline</span></h4>` + outgoing.map((d) => row(d, false)).join("");
  box.innerHTML = html;
}

function renderTodoSchedule() {
  const box = $("todo-schedule");
  if (!box || !state) return;
  let html = "";
  if (isTrading(state)) {
    const when = state.round_deadline
      ? `closes ${fmtSlot(state.round_deadline)}`
      : `closes when the host clicks`;
    html += `<div class="sched-row"><b>${phaseLabel(state.phase)}</b> · round ${state.round} of ${state.total_rounds} — ${when}</div>`;
  } else if (state.phase === "finished") {
    html += `<div class="sched-row muted">The auction is finished.</div>`;
  }
  const me = state.me;
  if (me != null) {
    if (state.phase === "primary") {
      html += `<div class="sched-row muted">Ladder games begin after the primary phase.</div>`;
    } else if (state.phase === "secondary" || state.phase === "finished") {
      const mine = ((ladder && ladder.matches) || [])
        .filter((m) => (m.a === me || m.b === me) && m.status === "scheduled")
        .sort((a, b) => a.slot_start - b.slot_start);
      html += mine.length
        ? `<h4>Your upcoming games</h4>` + mine.map((m) => {
            const opp = m.a === me ? m.b_name : m.a_name;
            return `<div class="sched-row">🎲 vs <b>${esc(opp)}</b> · ${fmtSlot(m.slot_start)}</div>`;
          }).join("")
        : `<div class="sched-row muted">No games scheduled — set your availability on the Ladder tab.</div>`;
    }
  }
  box.innerHTML = html || `<p class="muted">Nothing scheduled.</p>`;
}

// ---- actions ----
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

// Tabs
$$(".tab").forEach((t) => (t.onclick = () => {
  activeTab = t.dataset.tab;
  $$(".tab").forEach((x) => x.classList.toggle("active", x === t));
  $("tab-home").classList.toggle("hidden", activeTab !== "home");
  $("tab-inventory").classList.toggle("hidden", activeTab !== "inventory");
  $("tab-market").classList.toggle("hidden", activeTab !== "market");
  $("tab-ladder").classList.toggle("hidden", activeTab !== "ladder");
  $("tab-calendar").classList.toggle("hidden", activeTab !== "calendar");
}));

// Confirm receipt of an incoming delivery.
$("todo-deliveries").addEventListener("click", async (e) => {
  const b = e.target.closest(".d-receive");
  if (!b) return;
  try { await api("/api/deliveries/receive", "POST", { delivery_id: Number(b.dataset.id) }); await refresh(); }
  catch (err) { toastError(err.message); }
});
