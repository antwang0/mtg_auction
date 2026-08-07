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
  renderAuctionHistory();
}

// Called on every render: load when the tab is open and the cache is missing
// or stale (another auction has closed since it was built).
function syncAuctionHistory() {
  if (!state || !isLeague(state) || activeTab !== "history") return;
  if (ahRows !== null && ahLoadedAt === (state.rounds_closed ?? 0)) { renderAuctionHistory(); return; }
  loadAuctionHistory();
}

// A row's outcome for the logged-in player. Winning without a bid means a
// leftover copy was handed over for free.
function ahOutcome(r) {
  if (r.won) return r.my_bid == null ? "won (free)" : "won";
  return r.my_bid == null ? "" : "outbid";
}

// The rows currently on screen, after the round / mine-only filters and the
// chosen sort. Exports use this too, so what you download is what you see.
function ahVisibleRows() {
  const round = $("ah-round").value;
  const mineOnly = $("ah-mine").checked;
  const rows = (ahRows || []).filter((r) =>
    (!round || String(r.round) === round) && (!mineOnly || r.my_bid != null));
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
    box.innerHTML = `<p class="muted">No auction has closed yet — results appear here after the first close.</p>`;
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
    (loggedIn ? `<th class="num">Your bid</th><th></th>` : "") +
    `</tr></thead><tbody>` +
    rows.map((r) => {
      const outcome = ahOutcome(r);
      const cls = r.won ? "buy" : outcome ? "sell" : "";
      const mine = loggedIn
        ? `<td class="num">${fmtUSD(r.my_bid)}</td>` +
          `<td>${outcome ? `<span class="ord-badge ${cls}">${outcome}</span>` : ""}</td>`
        : "";
      return `<tr data-card="${r.card}"><td class="num">${r.round}</td>` +
        `<td><span class="${rarityClass(r.rarity)}">●</span> ${esc(r.card_name)}</td>` +
        `<td class="num">×${r.copies}</td><td class="num">${fmtUSD(r.cleared)}</td>` +
        `<td class="num">${fmtUSD(r.cover)}</td><td class="num">${fmtUSD(r.high)}</td>${mine}</tr>`;
    }).join("") +
    `</tbody></table>`;
}

function ahExportInfo(n) {
  $("ah-export-info").textContent = n ? `${n} row${n === 1 ? "" : "s"}` : "Nothing to export yet.";
  $("ah-export-txt").disabled = !n;
  $("ah-export-csv").disabled = !n;
}

// ---- export ----
// Both formats carry the same columns as the table, so a spreadsheet and a
// `cut -f` pipeline see the same history. Cents are written as plain dollars.
const AH_EXPORT_HEADER = ["round", "card", "rarity", "copies", "clearing_usd", "cover_usd", "high_usd", "your_bid_usd", "result"];
const ahUsd = (c) => (c == null ? "" : (c / 100).toFixed(2));

function ahExportRows() {
  return ahVisibleRows().map((r) => [
    r.round, r.card_name, r.rarity, r.copies,
    ahUsd(r.cleared), ahUsd(r.cover), ahUsd(r.high), ahUsd(r.my_bid),
    ahOutcome(r) || "no bid",
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
["ah-round", "ah-mine", "ah-sort"].forEach((id) => {
  $(id).addEventListener("change", renderAuctionHistory);
});
// Load on first open of the tab; syncAuctionHistory keeps it fresh after that.
document.querySelector('.tab[data-tab="history"]').addEventListener("click", syncAuctionHistory);
// Card names open the usual card modal.
$("ah-table").addEventListener("click", (e) => {
  const tr = e.target.closest("tr[data-card]");
  if (tr) openModal(Number(tr.dataset.card));
});
