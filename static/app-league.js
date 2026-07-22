"use strict";

// League mode: the weekly Auction tab (the house pool, sealed bids) and the
// manual inventory editor. Shares state with app-core.js.

// ---- Auction tab ----
function renderLeague() {
  if (!state || !isLeague(state)) return;
  const open = leagueOpen(state);
  const loggedIn = state.me != null;

  const tz = state.league_tz_offset_mins;
  $("lg-status").textContent = state.league_ended
    ? `— the league's last auction has closed`
    : open
    ? `— week ${state.round} · closes ${fmtLeagueTime(state.round_deadline, tz)}`
    : state.league_next_close
    ? `— closed · next auction ${fmtLeagueTime(state.league_next_close, tz)} once the host stocks the pool`
    : `— closed · the host opens the next one by stocking the pool`;

  $("lg-summary").textContent = loggedIn
    ? `Committed ${fmtUSD(state.my_committed)} · Available to bid ${fmtUSD(state.my_available)} · stipend ${fmtUSD(state.weekly_stipend)} after each close`
    : "Log in to bid.";

  renderLeagueMyBids();
  renderLeaguePool();
}

function renderLeagueMyBids() {
  const box = $("lg-mybids");
  const bids = state.my_league_bids || [];
  if (state.me == null || !bids.length) { box.innerHTML = ""; return; }
  box.innerHTML =
    `<h3>Your bids <span class="muted">— each wins at most one copy</span></h3>` +
    `<table class="grid mini"><tbody>` +
    bids.map((b) =>
      `<tr><td>${esc(b.name)}</td><td class="num">@${fmtUSD(b.price)}</td>` +
      `<td><button class="linkbtn lg-cancel" data-id="${b.id}">cancel</button></td></tr>`
    ).join("") +
    `</tbody></table>`;
}

function renderLeaguePool() {
  const g = $("lg-pool");
  g.innerHTML = "";
  const pool = state.house || [];
  if (!pool.length) {
    g.innerHTML = `<p class="muted">The pool is empty — the host hasn't stocked this week's cards yet.</p>`;
    return;
  }
  const open = leagueOpen(state);
  const loggedIn = state.me != null;
  const myBids = {};
  (state.my_league_bids || []).forEach((b) => (myBids[b.card] = myBids[b.card] || []).push(b));

  pool.forEach((h) => {
    const c = cardById[h.card];
    if (!c) return;
    const tile = document.createElement("div");
    tile.className = "tile lg-tile";
    const art = c.image
      ? `<img class="tile-img" src="${esc(c.image)}" alt="" loading="lazy" data-card="${c.id}" />`
      : `<div class="tile-img no-img ${rarityClass(c.rarity)}" data-card="${c.id}">${esc(c.name)}</div>`;
    const mine = (myBids[c.id] || [])
      .map((b) => `<span class="ord-badge buy">bid ${fmtUSD(b.price)} <button class="lg-cancel lg-x" data-id="${b.id}" title="cancel this bid">✕</button></span>`)
      .join(" ");
    tile.innerHTML =
      art +
      `<div class="tile-name">${esc(c.name)}</div>` +
      `<div class="tile-sub muted">${esc(c.type_line || "")} <span class="pips">${colorPips(c.colors)}</span></div>` +
      `<div class="tile-foot"><span class="${rarityClass(c.rarity)}">${c.rarity}</span><span class="num">×${h.qty} available</span></div>` +
      (mine ? `<div class="tile-orders">${mine}</div>` : "") +
      (open && loggedIn
        ? `<div class="lg-bidrow"><input class="lg-price" type="number" min="0" step="0.01" placeholder="$" title="your bid per copy" />` +
          `<button class="buy lg-bid" data-card="${c.id}">Bid</button></div>`
        : "");
    g.appendChild(tile);
  });
}

// Delegated auction actions: place a bid on a pool card, cancel a resting bid.
$("tab-auction").addEventListener("click", async (e) => {
  const cancel = e.target.closest(".lg-cancel");
  if (cancel) {
    e.stopPropagation();
    try { await api("/api/league/bid/cancel", "POST", { bid_id: Number(cancel.dataset.id) }); $("lg-error").textContent = ""; await refresh(); }
    catch (err) { $("lg-error").textContent = err.message; }
    return;
  }
  const bid = e.target.closest(".lg-bid");
  if (bid) {
    e.stopPropagation();
    const input = bid.closest(".lg-bidrow").querySelector(".lg-price");
    const price = toCents(input.value);
    if (!input.value.trim()) { input.focus(); return; }
    try {
      await api("/api/league/bid", "POST", { card: Number(bid.dataset.card), price });
      $("lg-error").textContent = "";
      input.value = "";
      await refresh();
    } catch (err) { $("lg-error").textContent = err.message; }
  }
});
// Enter in a price box places the bid.
$("tab-auction").addEventListener("keydown", (e) => {
  if (e.key === "Enter" && e.target.classList.contains("lg-price")) {
    e.preventDefault();
    e.target.closest(".lg-bidrow").querySelector(".lg-bid").click();
  }
});

// ---- manual inventory (league mode) ----
$("btn-inv-add").onclick = async () => {
  const card_list = $("inv-cardlist").value;
  if (!card_list.trim()) return;
  const btn = $("btn-inv-add");
  btn.disabled = true;
  $("inv-msg").textContent = "Looking up cards…";
  try {
    const r = await api("/api/inventory/add", "POST", { card_list });
    $("inv-cardlist").value = "";
    $("inv-msg").textContent = `Added ${r.added} card${r.added === 1 ? "" : "s"}.`;
    await refresh();
  } catch (e) { $("inv-msg").textContent = e.message; }
  finally { btn.disabled = false; }
};

// Remove one copy of a holding. Buttons appear both on the Inventory tab
// (standard-mode holdings table) and the Home "Your Cards" table (league mode,
// where the Inventory tab is hidden). Home rows also open the card modal on
// click, so stop propagation there.
async function removeOneCopy(card) {
  try { await api("/api/inventory/remove", "POST", { card, qty: 1 }); await refresh(); }
  catch (err) { toastError(err.message); }
}
$("my-holdings").addEventListener("click", (e) => {
  const b = e.target.closest(".inv-remove");
  if (b) removeOneCopy(Number(b.dataset.card));
});
$("home-cards").addEventListener("click", (e) => {
  const b = e.target.closest(".home-inv-remove");
  if (!b) return;
  e.stopPropagation(); // don't also open the card modal
  removeOneCopy(Number(b.dataset.card));
});
