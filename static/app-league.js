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
    ? `— auction ${state.round} · closes ${fmtLeagueTime(state.round_deadline, tz)}`
    : state.league_next_close
    ? `— closed · next auction ${fmtLeagueTime(state.league_next_close, tz)} once the host stocks the pool`
    : `— closed · the host opens the next one by stocking the pool`;

  $("lg-summary").title =
    "Committed is the sum of your resting bids — it may exceed your balance. Winners pay the clearing price, and any bid above your remaining balance is trimmed to it when its card resolves.";
  $("lg-summary").textContent = loggedIn
    ? `Committed ${fmtUSD(state.my_committed)} · Balance ${fmtUSD(state.my_available)} · stipend ${fmtUSD(state.weekly_stipend)} after each close. One bid per card; winners all pay the card's clearing price, and bids over your remaining balance are trimmed to it as cards resolve (rarest first).`
    : "Log in to bid.";

  // Export controls: how much is in the pool, and nothing to download when it's
  // empty.
  const pool = state.house || [];
  const copies = pool.reduce((s, h) => s + h.qty, 0);
  $("lg-export-info").textContent = pool.length
    ? `${pool.length} card${pool.length === 1 ? "" : "s"} · ${copies} cop${copies === 1 ? "y" : "ies"} in the pool`
    : "Nothing to export — the pool is empty.";
  $("lg-export-txt").disabled = !pool.length;
  $("lg-export-csv").disabled = !pool.length;

  renderLeagueMyBids();
  renderLeaguePool();
}

// Auction resolution order: rarest first (RARITY_RANK, from app-market.js,
// ranks common lowest), then name.
function resolutionCmp(a, b) {
  const ra = RARITY_RANK[a.rarity] ?? -9, rb = RARITY_RANK[b.rarity] ?? -9;
  return rb - ra || a.name.localeCompare(b.name);
}

function renderLeagueMyBids() {
  const box = $("lg-mybids");
  // Shown in resolution order — the order the close spends your balance, so
  // it reads top-to-bottom as "what happens to my money".
  const bids = (state.my_league_bids || [])
    .map((b) => ({ ...b, card_info: cardById[b.card] || { rarity: "", name: b.name } }))
    .sort((x, y) => resolutionCmp(x.card_info, y.card_info));
  if (state.me == null || !bids.length) { box.innerHTML = ""; return; }
  box.innerHTML =
    `<h3>Your bids <span class="muted">— in resolution order; each wins at most one copy</span></h3>` +
    `<table class="grid mini"><tbody>` +
    bids.map((b) =>
      `<tr><td><span class="${rarityClass(b.card_info.rarity)}">●</span> ${esc(b.name)}</td><td class="num">@${fmtUSD(b.price)}</td>` +
      `<td><button class="linkbtn lg-cancel" data-id="${b.id}">cancel</button></td></tr>`
    ).join("") +
    `</tbody></table>`;
}

// Is the player part-way through typing a bid? A number input doesn't expose
// its raw text — mid-decimal entries like "1." read back as "" with
// validity.badInput set — so there's nothing to snapshot for the box being
// edited, and the only safe move is to leave the grid alone.
function bidBeingTyped() {
  const el = document.activeElement;
  if (!el || !el.classList || !el.classList.contains("lg-price")) return false;
  return el.value !== "" || (el.validity && el.validity.badInput);
}

// `force` marks a redraw the player asked for themselves — a filter or sort
// change, or a ★ toggle — where redrawing straight away is the expected
// response to their own click. Background refreshes leave it unset.
function renderLeaguePool(opts) {
  if (!state || !isLeague(state)) return;
  const g = $("lg-pool");
  // A rebuild replaces every tile, which would wipe a bid that's been typed but
  // not submitted. Live updates land here every few seconds (see
  // startLiveUpdates in util.js), so hold the rebuild back while a price box is
  // being filled in — the next refresh redraws the grid once it's submitted or
  // abandoned.
  if (!(opts && opts.force) && bidBeingTyped()) return;
  // For the rebuilds we do run, carry any typed amounts (and the focused box)
  // across, keyed by card so they survive tiles being reordered by the sort.
  const typed = new Map();
  let focusedCard = null;
  g.querySelectorAll(".lg-price").forEach((el) => {
    if (el.value !== "") typed.set(el.dataset.bidCard, el.value);
    if (el === document.activeElement) focusedCard = el.dataset.bidCard;
  });

  g.innerHTML = "";
  const pool = state.house || [];
  const count = document.querySelector('.filters[data-prefix="lg"] .f-count');
  if (!pool.length) {
    count.textContent = "";
    g.innerHTML = `<p class="muted">The pool is empty — the host hasn't stocked this week's cards yet.</p>`;
    return;
  }
  const open = leagueOpen(state);
  const loggedIn = state.me != null;
  const myBids = {};
  (state.my_league_bids || []).forEach((b) => (myBids[b.card] = myBids[b.card] || []).push(b));

  // Same filter bar as the Market tab (name, rarity, type, mana value, colour
  // identity, wanted ★) — cardMatches lives in app-market.js. "you've bid" is
  // the auction's own facet, so it's applied here rather than in cardMatches.
  const f = getFilters("lg");
  // Sort the grid by the selected method (default: auction resolution order).
  const mode = $("lg-sort") ? $("lg-sort").value : "resolution";
  const items = pool
    .map((h) => ({ h, c: cardById[h.card] }))
    .filter((x) => x.c && cardMatches(x.c, f) && (f.show !== "bid" || myBids[x.c.id]))
    .sort((x, y) => {
      switch (mode) {
        case "name": return x.c.name.localeCompare(y.c.name);
        case "price": return (y.c.ref_price || 0) - (x.c.ref_price || 0) || x.c.name.localeCompare(y.c.name);
        case "mv": return (x.c.cmc || 0) - (y.c.cmc || 0) || x.c.name.localeCompare(y.c.name);
        case "mybid": {
          const bx = myBids[x.c.id] ? 0 : 1, by = myBids[y.c.id] ? 0 : 1;
          return bx - by || resolutionCmp(x.c, y.c);
        }
        default: return resolutionCmp(x.c, y.c);
      }
    });

  count.textContent = `${items.length} / ${pool.length}`;
  if (!items.length) {
    g.innerHTML = `<p class="muted">No cards in the pool match your filters.</p>`;
    return;
  }

  items.forEach(({ h, c }) => {
    const tile = document.createElement("div");
    tile.className = "tile lg-tile" + (wants.has(c.name) ? " wanted" : "");
    const art = c.image
      ? `<img class="tile-img" src="${esc(c.image)}" alt="" loading="lazy" data-card="${c.id}" />`
      : `<div class="tile-img no-img ${rarityClass(c.rarity)}" data-card="${c.id}">${esc(c.name)}</div>`;
    const mine = (myBids[c.id] || [])
      .map((b) => `<span class="ord-badge buy">bid ${fmtUSD(b.price)} <button class="lg-cancel lg-x" data-id="${b.id}" title="cancel this bid">✕</button></span>`)
      .join(" ");
    tile.innerHTML =
      // The Market tab is hidden in league mode, so this is where players mark
      // what they're after; the delegated handler in app-market.js catches it.
      `<button class="want-star ${wants.has(c.name) ? "on" : ""}" data-name="${esc(c.name)}" title="mark as wanted — filter on it with 'wanted ★'">${star(c.name)}</button>` +
      art +
      `<div class="tile-name">${esc(c.name)}</div>` +
      `<div class="tile-sub muted">${esc(c.type_line || "")} <span class="pips">${colorPips(c.colors)}</span></div>` +
      `<div class="tile-foot"><span class="${rarityClass(c.rarity)}">${c.rarity}</span><span class="num">×${h.qty} available</span></div>` +
      (mine ? `<div class="tile-orders">${mine}</div>` : "") +
      (open && loggedIn
        ? `<div class="lg-bidrow"><input class="lg-price" type="number" min="0.01" step="0.01" placeholder="$" title="your bid per copy" data-bid-card="${c.id}" />` +
          `<button class="buy lg-bid" data-card="${c.id}">Bid</button></div>`
        : "");
    g.appendChild(tile);
  });

  if (typed.size || focusedCard !== null) {
    g.querySelectorAll(".lg-price").forEach((el) => {
      const v = typed.get(el.dataset.bidCard);
      if (v !== undefined) el.value = v;
      if (el.dataset.bidCard === focusedCard) el.focus();
    });
  }
}

// Re-sort the grid immediately when the sort method changes.
$("lg-sort").addEventListener("change", () => renderLeaguePool({ force: true }));

// ---- pool export ----
// The whole auction inventory, in the two formats the host can export (see
// admin-manage.js): a `quantity name` decklist and a CSV with the same columns.
// Players use these to plan their bids in a spreadsheet. `supply` is the number
// of copies in this week's pool.
function poolForExport() {
  return (state && state.house || [])
    .map((h) => ({ h, c: cardById[h.card] || {} }))
    .sort((x, y) => x.h.name.localeCompare(y.h.name));
}
$("lg-export-txt").onclick = () => {
  const lines = poolForExport().filter(({ h }) => h.qty > 0).map(({ h }) => `${h.qty} ${h.name}`);
  if (!lines.length) return;
  downloadFile(`${exportSlug()}-auction-pool.txt`, lines.join("\n") + "\n", "text/plain");
};
$("lg-export-csv").onclick = () => {
  const pool = poolForExport();
  if (!pool.length) return;
  const rows = [EXPORT_HEADER];
  pool.forEach(({ h, c }) => rows.push([
    h.name, c.rarity ?? "", h.qty, c.cmc ?? "", c.type_line ?? "",
    c.ref_price != null ? (c.ref_price / 100).toFixed(2) : "",
  ]));
  downloadFile(`${exportSlug()}-auction-pool.csv`, toCsv(rows), "text/csv");
};

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
    // Everyone already implicitly bids $0, so an explicit one is rejected by
    // the server — say so here rather than making the round trip.
    if (price <= 0) { $("lg-error").textContent = "A bid must be more than $0."; input.focus(); return; }
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

// Remove one copy of a holding, from the Inventory tab's holdings table. The
// Home "Your Cards" table is a read-only list of what you own — league cards
// come from the auction, not from hand-curated inventory.
async function removeOneCopy(card) {
  try { await api("/api/inventory/remove", "POST", { card, qty: 1 }); await refresh(); }
  catch (err) { toastError(err.message); }
}
$("my-holdings").addEventListener("click", (e) => {
  const b = e.target.closest(".inv-remove");
  if (b) removeOneCopy(Number(b.dataset.card));
});
