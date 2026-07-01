"use strict";

// Boot: run after every other script so all renderers and handlers exist.

function consumeMagicLink() {
  const params = new URLSearchParams(location.search);
  const t = params.get("t");
  if (!t) return;
  setToken(t);
  params.delete("t");
  history.replaceState({}, "", location.pathname + (params.toString() ? "?" + params : ""));
}

// Live updates: SSE + adaptive poll fallback (shared helper in util.js).
consumeMagicLink();
startLiveUpdates({ refresh, setConn });
