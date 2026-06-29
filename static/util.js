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
function phaseLabel(p) {
  return p === "primary" ? "Primary (bank issue)" : p === "secondary" ? "Secondary (trading)" : p;
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
    mode: box?.querySelector(".f-cmode")?.value || "atmost",
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
  if (m && f.mode) m.value = f.mode;
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
  const SLOW_MS = 30000, FAST_MS = 3000;
  let es = null, pollTimer = null, pollMs = 0;

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
      es.onmessage = () => { up(); refresh(); };
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
