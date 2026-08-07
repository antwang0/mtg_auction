"use strict";

// League mode: the Auction History tab — what every card did at each close,
// and an export of the same. Shares state with app-core.js.
//
// The rows come from /api/league/history rather than /api/state: this is a
// cold, bulky payload that only matters while the tab is open, and /api/state
// is polled by every player. So it is fetched lazily and cached until another
// auction closes.

let ahRows = null;      // rows as served, or null when never loaded
let ahLoadedAt = -1;    // state.rounds_closed the cache was built from
let ahLoading = false;

async function loadAuctionHistory() {
  if (!state || !isLeague(state) || ahLoading) return;
  ahLoading = true;
  const closed = state.rounds_closed ?? 0;
  try {
    const r = await api("/api/league/history");
    ahRows = r.rows || [];
    ahLoadedAt = closed;
    $("ah-error").textContent = "";
  } catch (e) {
    $("ah-error").textContent = e.message;
  } finally {
    ahLoading = false;
  }
  ahRefreshViews();
}

// The Home tab shows the collected count too, so the rows are loaded for any
// league game — but the table itself is only rebuilt when you're looking at
// it, since that's the expensive part and every client polls.
function ahRefreshViews() {
  ahClaimInfo();
  if (activeTab === "history") renderAuctionHistory();
}

// Called on every render: (re)load when the cache is missing or stale —
// another auction has closed since it was built.
function syncAuctionHistory() {
  if (!state || !isLeague(state)) return;
  if (ahRows === null || ahLoadedAt !== (state.rounds_closed ?? 0)) { loadAuctionHistory(); return; }
  ahRefreshViews();
}

// A row's outcome for the logged-in player. Winning without a bid means a
// leftover copy was handed over for free.
function ahOutcome(r) {
  if (r.won) return r.my_bid == null ? "won (free)" : "won";
  return r.my_bid == null ? "" : "outbid";
}

// The rows currently on screen, after the round / show filters and the chosen
// sort. Exports use this too, so what you download is what you see.
function ahShows(r) {
  switch ($("ah-show").value) {
    case "bid": return r.my_bid != null;
    // "Won" deliberately isn't "bid on and won": a free leftover copy is a
    // card you won with no bid, and it still has to be collected.
    case "won": return r.won;
    case "unclaimed": return r.won && !r.claimed;
    default: return true;
  }
}

function ahVisibleRows() {
  const round = $("ah-round").value;
  const rows = (ahRows || []).filter((r) => (!round || String(r.round) === round) && ahShows(r));
  const mode = $("ah-sort").value;
  return rows.sort((a, b) => {
    switch (mode) {
      case "clearing": return b.cleared - a.cleared || a.card_name.localeCompare(b.card_name);
      case "name": return a.card_name.localeCompare(b.card_name) || b.round - a.round;
      // Newest round first, and within a round the auction's own resolution
      // order: rarest first, then by name.
      default: return b.round - a.round ||
        (RARITY_RANK[b.rarity] ?? -9) - (RARITY_RANK[a.rarity] ?? -9) ||
        a.card_name.localeCompare(b.card_name);
    }
  });
}

function renderAuctionHistory() {
  if (!state || !isLeague(state)) return;
  const box = $("ah-table");
  ahClaimInfo();

  // Keep the round picker in step with the data, preserving the selection.
  const sel = $("ah-round");
  const rounds = [...new Set((ahRows || []).map((r) => r.round))].sort((a, b) => b - a);
  const want = `<option value="">all rounds</option>` +
    rounds.map((n) => `<option value="${n}">Round ${n}</option>`).join("");
  if (sel.dataset.built !== want) {
    const prev = sel.value;
    sel.innerHTML = want;
    sel.dataset.built = want;
    if (rounds.some((n) => String(n) === prev)) sel.value = prev;
  }

  if (ahRows === null) { box.innerHTML = `<p class="muted">Loading…</p>`; ahExportInfo(0); return; }
  if (!ahRows.length) {
    // Rounds that closed before this tab existed left no per-card record, so
    // don't tell someone with a season behind them that nothing has happened.
    box.innerHTML = (state.rounds_closed || 0) > 0
      ? `<p class="muted">No per-card results are stored for the ${state.rounds_closed} auction${state.rounds_closed === 1 ? "" : "s"} that have closed — they ran before this tab existed. The host can rebuild them from the order ledger (Round Control on the admin page).</p>`
      : `<p class="muted">No auction has closed yet — results appear here after the first close.</p>`;
    $("ah-status").textContent = "";
    ahExportInfo(0);
    return;
  }

  const rows = ahVisibleRows();
  $("ah-status").textContent = `— ${rounds.length} auction${rounds.length === 1 ? "" : "s"} closed`;
  ahExportInfo(rows.length);
  if (!rows.length) { box.innerHTML = `<p class="muted">No cards match this filter.</p>`; return; }

  const loggedIn = state.me != null;
  box.innerHTML =
    `<table class="grid mini"><thead><tr>` +
    `<th class="num">Round</th><th>Card</th><th class="num">Copies</th>` +
    `<th class="num" title="The one price every winner paid">Clearing</th>` +
    `<th class="num" title="Highest bid that won nothing">Cover</th>` +
    `<th class="num" title="Highest bid of the round">High</th>` +
    (loggedIn ? `<th class="num">Your bid</th><th></th><th title="Tick off the cards you've physically collected">Collected</th>` : "") +
    `</tr></thead><tbody>` +
    rows.map((r) => {
      const outcome = ahOutcome(r);
      const cls = r.won ? "buy" : outcome ? "sell" : "";
      // Only a card you won can be collected, so other rows leave the cell empty.
      const claim = r.won
        ? `<input type="checkbox" class="ah-claim" data-round="${r.round}" data-card="${r.card}"${r.claimed ? " checked" : ""} title="collected from the host" />`
        : "";
      const mine = loggedIn
        ? `<td class="num">${fmtUSD(r.my_bid)}</td>` +
          `<td>${outcome ? `<span class="ord-badge ${cls}">${outcome}</span>` : ""}</td>` +
          `<td>${claim}</td>`
        : "";
      return `<tr data-card="${r.card}"><td class="num">${r.round}</td>` +
        `<td><span class="${rarityClass(r.rarity)}">●</span> ${esc(r.card_name)}</td>` +
        `<td class="num">×${r.copies}</td><td class="num">${fmtUSD(r.cleared)}</td>` +
        `<td class="num">${fmtUSD(r.cover)}</td><td class="num">${fmtUSD(r.high)}</td>${mine}</tr>`;
    }).join("") +
    `</tbody></table>`;
}

// The pickup checklist, drawn in two places: the History tab and Home. Counts
// cover every card you've won, not just the filtered view, because "mark all"
// ticks off the lot.
const AH_CLAIM_WIDGETS = [
  ["ah-claim-row", "ah-claim-info", "ah-claim-all"],
  ["home-claim-row", "home-claim-info", "home-claim-all"],
];

function ahClaimInfo() {
  const wins = (ahRows || []).filter((r) => r.won);
  const done = wins.filter((r) => r.claimed).length;
  const show = state && isLeague(state) && state.me != null && wins.length > 0;
  const all = wins.length > 0 && done === wins.length;
  const text = `${done} of ${wins.length} won card${wins.length === 1 ? "" : "s"} collected`;
  AH_CLAIM_WIDGETS.forEach(([row, info, btn]) => {
    $(row).classList.toggle("hidden", !show);
    if (!show) return;
    $(info).textContent = text;
    $(btn).textContent = all ? "Unmark all collected" : "Mark all collected";
    $(btn).dataset.claimed = all ? "1" : "0";
  });
}

function ahExportInfo(n) {
  $("ah-export-info").textContent = n ? `${n} row${n === 1 ? "" : "s"}` : "Nothing to export yet.";
  $("ah-export-txt").disabled = !n;
  $("ah-export-csv").disabled = !n;
}

// ---- export ----
// Both formats carry the same columns as the table, so a spreadsheet and a
// `cut -f` pipeline see the same history. Cents are written as plain dollars.
const AH_EXPORT_HEADER = ["round", "card", "rarity", "copies", "clearing_usd", "cover_usd", "high_usd", "your_bid_usd", "result", "collected"];
const ahUsd = (c) => (c == null ? "" : (c / 100).toFixed(2));

function ahExportRows() {
  return ahVisibleRows().map((r) => [
    r.round, r.card_name, r.rarity, r.copies,
    ahUsd(r.cleared), ahUsd(r.cover), ahUsd(r.high), ahUsd(r.my_bid),
    ahOutcome(r) || "no bid",
    r.won ? (r.claimed ? "yes" : "no") : "",
  ]);
}

$("ah-export-csv").onclick = () => {
  const rows = ahExportRows();
  if (!rows.length) return;
  downloadFile(`${exportSlug()}-auction-history.csv`, toCsv([AH_EXPORT_HEADER, ...rows]), "text/csv");
};
// Tab-separated, with `#` comment lines, matching /matches.
$("ah-export-txt").onclick = () => {
  const rows = ahExportRows();
  if (!rows.length) return;
  const out = [
    `# auction history — ${state.set_name || "league"}`,
    `# clearing = price every winner paid; cover = highest bid that won nothing`,
    `# your_bid is your own bid only; other players' bids are sealed`,
    `# ${AH_EXPORT_HEADER.join("\t")}`,
    ...rows.map((r) => r.join("\t")),
  ].join("\n") + "\n";
  downloadFile(`${exportSlug()}-auction-history.txt`, out, "text/plain");
};

// Re-render (no refetch) when the view controls change.
["ah-round", "ah-show", "ah-sort"].forEach((id) => {
  $(id).addEventListener("change", renderAuctionHistory);
});

// ---- claiming ----
// The tick is applied to the cached row and re-rendered straight away so it
// feels instant; if the server rejects it we resync rather than leave a lie
// on screen.
async function ahClaim(apply, path, body) {
  apply();
  ahRefreshViews();
  try {
    await api(path, "POST", body);
    $("ah-error").textContent = "";
  } catch (err) {
    $("ah-error").textContent = err.message;
    loadAuctionHistory();
  }
}

$("ah-table").addEventListener("change", (e) => {
  const box = e.target.closest(".ah-claim");
  if (!box) return;
  const round = Number(box.dataset.round), card = Number(box.dataset.card), claimed = box.checked;
  const row = (ahRows || []).find((r) => r.round === round && r.card === card);
  ahClaim(() => { if (row) row.claimed = claimed; }, "/api/league/claim", { round, card, claimed });
});

// Same action from either widget.
function ahClaimAll(btnId) {
  const claimed = $(btnId).dataset.claimed !== "1";
  ahClaim(
    () => (ahRows || []).forEach((r) => { if (r.won) r.claimed = claimed; }),
    "/api/league/claim/all", { claimed });
}
$("ah-claim-all").onclick = () => ahClaimAll("ah-claim-all");
$("home-claim-all").onclick = () => ahClaimAll("home-claim-all");
// Load on first open of the tab; syncAuctionHistory keeps it fresh after that.
document.querySelector('.tab[data-tab="history"]').addEventListener("click", syncAuctionHistory);
// Card names open the usual card modal — but not when the click was meant for
// the row's collected tick.
$("ah-table").addEventListener("click", (e) => {
  if (e.target.closest(".ah-claim")) return;
  const tr = e.target.closest("tr[data-card]");
  if (tr) openModal(Number(tr.dataset.card));
});
