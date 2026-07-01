"use strict";

// Boot: run after every other script so all renderers and handlers exist.

// ---- magic-link login: ?t=<token> logs you in, then is stripped from the URL ----
function consumeMagicLink() {
  const params = new URLSearchParams(location.search);
  const t = params.get("t");
  if (!t) return;
  setToken(t);
  params.delete("t");
  history.replaceState({}, "", location.pathname + (params.toString() ? "?" + params : ""));
}
// ---- live updates ---- (SSE + adaptive poll fallback live in util.js)
consumeMagicLink();
startLiveUpdates({ refresh, setConn });
