"use strict";

// Public match schedule: everyone (no login) can see upcoming ladder matches,
// recent results, and the standings. Data comes from the public /api/ladder.

function fmtWhen(epoch) {
  return new Date(epoch * 1000).toLocaleString(undefined, {
    weekday: "short", month: "short", day: "numeric", hour: "2-digit", minute: "2-digit",
  });
}

function renderMatches(l) {
  const matches = l.matches || [];
  const upcoming = matches
    .filter((m) => m.status === "scheduled")
    .sort((a, b) => a.slot_start - b.slot_start);
  const played = matches
    .filter((m) => m.status === "completed")
    .sort((a, b) => b.slot_start - a.slot_start);

  $("status").textContent = `${upcoming.length} upcoming · ${played.length} played`;

  $("m-upcoming").innerHTML = upcoming.length
    ? `<table class="grid"><thead><tr><th>When</th><th>Match</th></tr></thead><tbody>` +
      upcoming.map((m) =>
        `<tr><td>${esc(fmtWhen(m.slot_start))}</td>` +
        `<td><b>${esc(m.a_name)}</b> <span class="muted">vs</span> <b>${esc(m.b_name)}</b></td></tr>`
      ).join("") + `</tbody></table>`
    : `<p class="muted">No matches scheduled yet.</p>`;

  $("m-results").innerHTML = played.length
    ? `<table class="grid"><thead><tr><th>When</th><th>Match</th><th>Result</th></tr></thead><tbody>` +
      played.map((m) => {
        const aWon = m.a_wins > m.b_wins, bWon = m.b_wins > m.a_wins;
        return `<tr><td>${esc(fmtWhen(m.slot_start))}</td>` +
          `<td>${aWon ? "<b>" : ""}${esc(m.a_name)}${aWon ? "</b>" : ""} <span class="muted">vs</span> ` +
          `${bWon ? "<b>" : ""}${esc(m.b_name)}${bWon ? "</b>" : ""}</td>` +
          `<td>${aWon || bWon ? esc(matchResult(m)) : "Draw"}</td></tr>`;
      }).join("") + `</tbody></table>`
    : `<p class="muted">No results yet.</p>`;

  const tb = $("m-standings").querySelector("tbody");
  tb.innerHTML = (l.standings || []).map((s) =>
    `<tr><td>${s.rank}</td><td>${esc(s.name)}</td><td class="num">${s.elo}</td>` +
    `<td class="num">${s.wins}-${s.losses}-${s.draws}</td></tr>`
  ).join("") || `<tr><td colspan="4" class="muted">No players yet.</td></tr>`;
}

async function load() {
  try {
    const res = await fetch("/api/ladder");
    if (!res.ok) throw new Error(`request failed (${res.status})`);
    renderMatches(await res.json());
  } catch (e) {
    $("status").textContent = "Could not load matches — retrying…";
    console.error(e);
  }
}

load();
setInterval(load, 60_000);
