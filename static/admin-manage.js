"use strict";

// Live-game management: the ELO ladder, house inventory, mid-game additions,
// card export, deliveries, feedback reports, and closing rounds. Shares state
// with admin-core.js.

// ---- ELO ladder ----
// Rendered in the host's local timezone (slots are UTC instants server-side).
function fmtSlot(epoch) {
  return new Date(epoch * 1000).toLocaleString(undefined, { weekday: "short", month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
}

async function loadLadder() {
  try { renderLadder(await api("/api/ladder")); } catch (e) { console.error(e); }
}

// Host override controls (report finalises immediately, no confirmation).
function overrideControls(m, league) {
  const btn = (aw, bw, dw, who, label) =>
    `<button class="primary rep-win" data-mid="${m.id}" data-aw="${aw}" data-bw="${bw}" data-dw="${dw}" data-who="${esc(who)}">${esc(label)}</button>`;
  // League matches are best-of-three, so record the margin too.
  const wins = league
    ? btn(2, 0, 0, m.a_name, `${m.a_name} 2-0`) + btn(2, 1, 0, m.a_name, `${m.a_name} 2-1`) +
      btn(1, 2, 0, m.b_name, `${m.b_name} 2-1`) + btn(0, 2, 0, m.b_name, `${m.b_name} 2-0`)
    : btn(1, 0, 0, m.a_name, m.a_name) + btn(0, 1, 0, m.b_name, m.b_name);
  return `<span class="report-row winrow">
    <span class="muted">Winner:</span>
    ${wins}
    <button class="ghost rep-win" data-mid="${m.id}" data-aw="${league ? 1 : 0}" data-bw="${league ? 1 : 0}" data-dw="1" data-who="draw">Draw</button>
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
  const league = !!l.league;
  $("btn-draw-unreported").hidden = !league;
  $("pairings-box").hidden = !league;
  if (!matches.length) {
    box.innerHTML = league
      ? `<p class="muted">No matches yet — they're assigned automatically once matchmaking opens.</p>`
      : `<p class="muted">No matches yet — players set availability and a weekly target on the game page.</p>`;
    return;
  }
  const tbl = document.createElement("table");
  tbl.className = "grid";
  tbl.innerHTML = `<thead><tr><th>${league ? "Play by" : "When"}</th><th>Match</th><th>Result / override</th><th></th></tr></thead>`;
  const body = document.createElement("tbody");
  matches.slice().sort((a, b) => a.slot_start - b.slot_start).forEach((m) => {
    const tr = document.createElement("tr");
    const td = (html) => { const d = document.createElement("td"); d.innerHTML = html; return d; };
    tr.appendChild(td(fmtSlot(m.slot_start)));
    tr.appendChild(td(`${esc(m.a_name)} <span class="muted">vs</span> ${esc(m.b_name)}`));
    if (m.status === "completed") {
      // Show the winner, plus a way to correct a mistaken result.
      tr.appendChild(td(`<div><b>${esc(matchResult(m))}</b></div><div class="muted">correct:</div>${overrideControls(m, league)}`));
    } else if (m.status === "cancelled") {
      const who = m.cancelled_by === m.a ? m.a_name : m.b_name;
      tr.appendChild(td(`<span class="muted">cancelled by ${esc(who)}</span>`));
    } else if (m.status === "expired") {
      tr.appendChild(td(`<div class="muted">unreported — record it if it was played:</div>${overrideControls(m, league)}`));
    } else {
      tr.appendChild(td(overrideControls(m, league)));
    }
    tr.appendChild(td(`<button class="linkbtn m-del" data-mid="${m.id}" title="delete this match from the record (reverts any ELO; standings recompute)">delete</button>`));
    body.appendChild(tr);
  });
  tbl.appendChild(body);
  box.appendChild(tbl);
}

$("btn-draw-unreported").onclick = async () => {
  try {
    const r = await api("/api/ladder/draw-unreported", "POST", {});
    $("tourney-error").textContent = "";
    toast(`Recorded ${r.recorded} unreported match${r.recorded === 1 ? "" : "es"} as 1-1 draws.`);
    await refresh();
  } catch (e) { $("tourney-error").textContent = e.message; }
};

$("btn-schedule").onclick = async () => {
  try {
    const r = await api("/api/ladder/schedule", "POST", {});
    $("tourney-error").textContent = "";
    toast(`Scheduled ${r.created} new match${r.created === 1 ? "" : "es"}.`);
    await refresh();
  } catch (e) { $("tourney-error").textContent = e.message; }
};

// Delegated: host sets a result directly (override) by clicking the winner,
// or deletes a match from the record entirely.
$("ladder-matches").addEventListener("click", async (e) => {
  const del = e.target.closest(".m-del");
  if (del) {
    if (!confirm("Delete this match from the record? Any applied ELO is reverted and standings recompute without it.")) return;
    try {
      await api("/api/ladder/delete", "POST", { match_id: Number(del.dataset.mid) });
      $("tourney-error").textContent = "";
      await refresh();
    } catch (err) { $("tourney-error").textContent = err.message; }
    return;
  }
  const go = e.target.closest(".rep-win");
  if (!go) return;
  const prompt = go.dataset.who === "draw" ? "Set this match as a draw?" : `Set ${go.dataset.who} as the winner?`;
  if (!confirm(prompt)) return;
  try {
    await api("/api/ladder/report", "POST", { match_id: Number(go.dataset.mid), a_wins: Number(go.dataset.aw), b_wins: Number(go.dataset.bw), draws: Number(go.dataset.dw) });
    $("tourney-error").textContent = "";
    await refresh();
  } catch (err) { $("tourney-error").textContent = err.message; }
});

// ---- mid-game management ----
// League: (re)open the weekly auction over the current (carried-over) pool.
$("btn-league-open").onclick = async () => {
  try {
    const r = await api("/api/league/open", "POST", {});
    toast(`Auction open — closes ${new Date(r.closes * 1000).toLocaleString()}.`);
    await refresh();
  } catch (e) { $("manage-error").textContent = e.message; }
};

$("btn-offer-house").onclick = async () => {
  try {
    const r = await api("/api/house/offer", "POST", {});
    toast(`Listed ${r.listed} house card${r.listed === 1 ? "" : "s"}.`);
    await refresh();
  } catch (e) { $("manage-error").textContent = e.message; }
};

$("btn-save-set").onclick = async () => {
  try {
    await api("/api/set-code", "POST", { set: $("manage-set").value });
    toast("Set code saved — future card lookups use it. Re-add a card to refresh its image/rarity.");
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

// Host: show every player's login link (e.g. to re-send a lost one).
$("btn-show-tokens").onclick = async () => {
  try {
    const r = await api("/api/tokens");
    showTokens(r.players);
    $("tokens").scrollIntoView({ behavior: "smooth", block: "nearest" });
  } catch (e) { $("manage-error").textContent = e.message; }
};

$("btn-remove-player").onclick = async () => {
  const name = $("remove-player-name").value.trim();
  if (!name) return;
  if (!confirm(`Remove ${name} from the game? Their cards go back to the house and their upcoming matches are dropped.`)) return;
  try {
    const r = await api("/api/players/remove", "POST", { name });
    $("remove-player-name").value = "";
    toast(`Removed ${esc(r.removed)}.`);
    await refresh();
  } catch (e) { $("manage-error").textContent = e.message; }
};

$("btn-pairings").onclick = async () => {
  const text = $("pairings-text").value;
  if (!text.trim()) return;
  try {
    const r = await api("/api/ladder/pairings", "POST", { text });
    $("pairings-text").value = "";
    $("tourney-error").textContent = "";
    toast(`Set ${r.created} pairing${r.created === 1 ? "" : "s"}.`);
    await refresh();
  } catch (e) { $("tourney-error").textContent = e.message; }
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
    // The host can settle a pending hand-off on the players' behalf.
    const receiveBtn = d.status === "pending" ? `<button class="buy d-received" data-id="${d.id}">Mark received</button> ` : "";
    const reverseBtn = d.status === "reversed" ? "" : `<button class="ghost d-reverse" data-id="${d.id}">Reverse</button>`;
    const note = d.note ? `<div class="muted">${esc(d.note)}</div>` : "";
    tr.innerHTML =
      `<td>${esc(d.card_name)}</td><td class="num">×${d.qty}</td>` +
      `<td>${esc(d.seller_name)}</td><td>${esc(d.buyer_name)}</td>` +
      `<td class="num">${fmtUSD(d.total)}</td>` +
      `<td class="dstat-${d.status}">${d.status}${note}</td>` +
      `<td>${deliveryDeadline(d)}</td><td>${receiveBtn}${reverseBtn}</td>`;
    tb.appendChild(tr);
  });
}

$("deliveries-table").addEventListener("click", async (e) => {
  const recv = e.target.closest(".d-received");
  if (recv) {
    try {
      await api("/api/deliveries/receive", "POST", { delivery_id: Number(recv.dataset.id) });
      await refresh();
    } catch (err) { $("deliveries-error").textContent = err.message; }
    return;
  }
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

$("btn-close").onclick = async () => {
  if (!confirm("Close the auction and match all orders?")) return;
  try {
    await api("/api/close", "POST", {});
    await refresh();
  } catch (e) { $("ctrl-error").textContent = e.message; }
};
