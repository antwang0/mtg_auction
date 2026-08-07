"use strict";

// Shared helpers loaded (as a classic script) before app.js / admin.js so both
// pages get the same money formatting, parsing and HTML escaping. Mostly defines
// globals; it also mounts the feedback widget (see the bottom of the file).

const TOKEN_KEY = "mtg_auction_token";

const $ = (id) => document.getElementById(id);
const $$ = (sel) => Array.from(document.querySelectorAll(sel));

function fmtUSD(cents) {
  if (cents === null || cents === undefined) return "—";
  const neg = cents < 0, v = Math.abs(cents);
  return (neg ? "-$" : "$") + Math.floor(v / 100) + "." + String(v % 100).padStart(2, "0");
}

// Parse a dollar string into integer cents without going through a binary
// float, so e.g. "1.005" rounds to 101, not 100. Invalid input yields 0.
function toCents(d) {
  const m = String(d).trim().match(/^(\d*)(?:\.(\d*))?$/);
  if (!m || (!m[1] && !m[2])) return 0;
  const frac = (m[2] || "").padEnd(2, "0");
  const round = (m[2] || "").charCodeAt(2) >= 53 ? 1 : 0; // 3rd digit ≥ "5"
  return (m[1] ? parseInt(m[1], 10) : 0) * 100 + parseInt(frac.slice(0, 2), 10) + round;
}

function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}
const esc = escapeHtml;

// Auction phase helpers (shared by both pages). The two trading phases have
// orders open; phaseLabel gives a human label.
function isTrading(s) { return !!s && (s.phase === "primary" || s.phase === "secondary"); }
// Human-readable result of a completed match: the winner's name, or "Draw".
function matchResult(m) {
  if (m.a_wins > m.b_wins) return `${m.a_name} won`;
  if (m.b_wins > m.a_wins) return `${m.b_name} won`;
  return "Draw";
}

// League mode: a recurring sealed-bid bank auction instead of the two phases.
function isLeague(s) { return !!s && s.phase === "league"; }
// Whether the league auction is currently taking bids.
function leagueOpen(s) { return isLeague(s) && !!s.league_open; }
function phaseLabel(p) {
  return p === "primary" ? "Primary (bank issue)" : p === "secondary" ? "Secondary (trading)" : p === "league" ? "League" : p;
}

// Format an epoch second in a fixed UTC-offset zone (the league timezone),
// rather than the viewer's local zone: shift the instant by the offset and
// render it as UTC, then tag it with the offset so it reads unambiguously.
function fmtLeagueTime(epoch, offsetMins) {
  if (epoch == null) return "—";
  const off = offsetMins || 0;
  const shown = new Date((epoch + off * 60) * 1000).toLocaleString(undefined, {
    weekday: "short", month: "short", day: "numeric", hour: "2-digit", minute: "2-digit", timeZone: "UTC",
  });
  const sign = off >= 0 ? "+" : "−";
  const h = Math.floor(Math.abs(off) / 60), m = Math.abs(off) % 60;
  const label = off === 0 ? "UTC" : `UTC${sign}${h}${m ? ":" + String(m).padStart(2, "0") : ""}`;
  return `${shown} ${label}`;
}

// ---- colour-identity filter (shared by the player pages and the admin picker) ----
// A card's `color_identity` is a canonical WUBRG string ("" = colorless). A
// colour control selects a set of WUBRG letters plus a match mode:
//   atmost  — identity ⊆ selected  (the card fits in a deck of these colours)
//   atleast — identity ⊇ selected  (contains every selected colour, maybe more)
//   exactly — identity is precisely the selected set
// The "C" toggle also lets colorless cards through; with nothing selected and C
// off there is no colour filter at all.

// Coloured pips for a colour string ("" = a single colorless pip).
function colorPips(colors) {
  if (!colors) return `<span class="pip pip-C" title="Colorless">C</span>`;
  return colors.split("").map((c) => `<span class="pip pip-${c}" title="${c}">${c}</span>`).join("");
}

// Read a colour control's state from its container element (the one holding the
// .cbtn buttons and the .f-cmode mode select).
function readColorFilter(box) {
  const on = box ? Array.from(box.querySelectorAll(".cbtn.active")) : [];
  return {
    colors: on.filter((b) => b.dataset.color).map((b) => b.dataset.color),
    colorless: on.some((b) => b.dataset.facet === "colorless"),
    mode: box?.querySelector(".f-cmode")?.value || "exactly",
  };
}

// Reflect a saved colour-filter state back onto its control (for UI restore).
function applyColorFilter(box, f) {
  if (!box || !f) return;
  box.querySelectorAll(".cbtn").forEach((btn) => {
    const on = (btn.dataset.color && (f.colors || []).includes(btn.dataset.color)) ||
      (btn.dataset.facet === "colorless" && f.colorless);
    btn.classList.toggle("active", !!on);
  });
  const m = box.querySelector(".f-cmode");
  // Only restore the match mode when colours are actually selected. With none
  // picked the mode does nothing (see matchesColorIdentity), and every saved
  // filter from before "exactly" became the default carries the old "at most" —
  // restoring that would pin returning players to it for good.
  const anyColor = (f.colors || []).length > 0 || f.colorless;
  if (m && f.mode && anyColor) m.value = f.mode;
}

// Does a card's colour identity satisfy a colour-filter state (from readColorFilter)?
function matchesColorIdentity(card, f) {
  if (!f.colors.length && !f.colorless) return true; // no colour filter
  const id = card.color_identity || "";
  if (f.colorless && id === "") return true;
  if (!f.colors.length) return false; // only colorless was requested
  const ids = new Set(id.split(""));
  switch (f.mode) {
    case "atleast": return f.colors.every((c) => ids.has(c));               // identity ⊇ selected
    case "exactly": return ids.size === f.colors.length && f.colors.every((c) => ids.has(c));
    default:        return [...ids].every((c) => f.colors.includes(c));      // atmost: identity ⊆ selected
  }
}

// Click handler for a colour control: toggle a button (or clear all) then run
// `onChange`. Returns true if the click hit a colour button.
function handleColorClick(box, e, onChange) {
  const b = e.target.closest(".cbtn");
  if (!b || !box.contains(b)) return false;
  if (b.dataset.facet === "clear") box.querySelectorAll(".cbtn.active").forEach((x) => x.classList.remove("active"));
  else b.classList.toggle("active");
  onChange();
  return true;
}

// Live updates: a Server-Sent Events stream with an adaptive polling fallback.
// While the stream is healthy we poll slowly (just a safety net); when it drops
// we poll quickly so the UI stays fresh, and rebuild the stream if the browser
// gives up on it (some proxies close the connection without it auto-retrying).
//
// `refresh` reloads state; `setConn(live)` updates the live/offline indicator.
function startLiveUpdates({ refresh, setConn }) {
  const SLOW_MS = 30000, FAST_MS = 3000, COALESCE_MS = 250;
  let es = null, pollTimer = null, pollMs = 0, refreshTimer = null;

  // Coalesce bursts of change events (e.g. several players bidding at once)
  // into a single refetch, so N rapid changes don't cost N full state loads
  // per client.
  function queueRefresh() {
    if (refreshTimer) return;
    refreshTimer = setTimeout(() => { refreshTimer = null; refresh(); }, COALESCE_MS);
  }

  function poll(ms) {
    if (ms === pollMs && pollTimer) return; // cadence already set — don't reset it
    pollMs = ms;
    if (pollTimer) clearInterval(pollTimer);
    pollTimer = setInterval(refresh, ms);
  }
  const up = () => { setConn(true); poll(SLOW_MS); };
  const down = () => { setConn(false); poll(FAST_MS); };

  function connect() {
    try {
      if (es) es.close();
      es = new EventSource("/api/events");
      es.onopen = up;
      es.onmessage = () => { up(); queueRefresh(); };
      es.onerror = () => {
        down();
        // readyState 2 (CLOSED) means the browser won't retry on its own.
        if (es && es.readyState === 2) setTimeout(connect, FAST_MS);
      };
    } catch (e) { down(); console.error(e); setTimeout(connect, FAST_MS); }
  }

  down();          // assume offline until the stream opens
  connect();
  refresh();
}

// ---- file export ----
// Shared by the host's card export (admin-manage.js) and the players' auction
// pool export (app-league.js); both read the page's global `state`.
function downloadFile(filename, text, mime) {
  const blob = new Blob([text], { type: mime });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url; a.download = filename;
  document.body.appendChild(a); a.click(); a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
// Filename stem, taken from the set name.
function exportSlug() {
  return ((state && state.set_name) || "cards").replace(/[^a-z0-9]+/gi, "-").replace(/^-|-$/g, "").toLowerCase() || "cards";
}
// Column set for every card export, so the host's pool CSV and a player's
// auction-pool CSV open the same way in a spreadsheet.
const EXPORT_HEADER = ["name", "rarity", "supply", "mana_value", "type", "ref_price_usd"];
// Rows of values -> CSV text, quoting any cell holding a comma, quote or newline.
function toCsv(rows) {
  const cell = (v) => {
    const s = v == null ? "" : String(v);
    return /[",\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
  };
  return rows.map((r) => r.map(cell).join(",")).join("\n") + "\n";
}

// ---- feedback widget ----
// A small "Feedback" button shown on every page that lets anyone file a bug
// report or feature request. Self-contained: posts to /api/reports with the
// stored token (if any), so it doesn't depend on app.js / admin.js.
async function submitReport(kind, text) {
  const headers = { "Content-Type": "application/json" };
  const tok = localStorage.getItem(TOKEN_KEY);
  if (tok) headers["X-Token"] = tok;
  const res = await fetch("/api/reports", { method: "POST", headers, body: JSON.stringify({ kind, text }) });
  const data = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(data.error || `request failed (${res.status})`);
}

function mountReportWidget() {
  if (document.getElementById("report-widget")) return;
  const wrap = document.createElement("div");
  wrap.id = "report-widget";
  wrap.innerHTML =
    `<div id="report-pop" class="hidden">
       <div class="report-head">Report a bug / request a feature</div>
       <div class="report-kind">
         <label><input type="radio" name="report-kind" value="bug" checked /> 🐞 Bug</label>
         <label><input type="radio" name="report-kind" value="feature" /> ✨ Feature</label>
       </div>
       <textarea id="report-text" rows="4" placeholder="Describe the bug, or the feature you'd like…"></textarea>
       <div class="report-actions">
         <button id="report-send" class="primary">Send</button>
         <button id="report-cancel" class="ghost">Cancel</button>
       </div>
       <div id="report-msg" class="report-msg"></div>
     </div>
     <button id="report-fab" title="Report a bug or request a feature">💬 Feedback</button>`;
  document.body.appendChild(wrap);

  const pop = wrap.querySelector("#report-pop");
  const msg = wrap.querySelector("#report-msg");
  const text = wrap.querySelector("#report-text");
  const send = wrap.querySelector("#report-send");
  const close = () => pop.classList.add("hidden");
  wrap.querySelector("#report-fab").onclick = () => {
    pop.classList.toggle("hidden");
    msg.textContent = "";
    if (!pop.classList.contains("hidden")) text.focus();
  };
  wrap.querySelector("#report-cancel").onclick = close;
  send.onclick = async () => {
    const t = text.value.trim();
    if (!t) { msg.textContent = "Please describe it first."; return; }
    const kind = wrap.querySelector('input[name="report-kind"]:checked').value;
    send.disabled = true; // guard against a double-submit
    try {
      await submitReport(kind, t);
      text.value = "";
      msg.textContent = "Thanks! Sent to the host.";
      setTimeout(close, 1200);
    } catch (e) { msg.textContent = e.message; } finally { send.disabled = false; }
  };
  // Dismiss the popover on Escape or a click outside it.
  document.addEventListener("keydown", (e) => { if (e.key === "Escape" && !pop.classList.contains("hidden")) close(); });
  document.addEventListener("click", (e) => { if (!pop.classList.contains("hidden") && !wrap.contains(e.target)) close(); });
}

if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", mountReportWidget);
else mountReportWidget();
