"use strict";

// Shared helpers loaded (as a classic script) before app.js / admin.js so both
// pages get the same money formatting, parsing and HTML escaping. Keep this file
// dependency-free and side-effect-free — it only defines globals.

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
