"use strict";

// The New Game form: card-pool sources, the player list, setup preview and
// validation, the card picker, and the token hand-out table. Shares state
// with admin-core.js.

// Card-pool source: show only the relevant inputs.
function selectedPool() {
  const r = document.querySelector('input[name="pool"]:checked');
  return r ? r.value : "sample";
}
function syncPoolPanes() {
  const pool = selectedPool();
  document.querySelectorAll(".pool-pane").forEach((p) => {
    const which = p.dataset.pool;
    const show = which === pool || (which === "packs" && pool !== "manual");
    p.hidden = !show;
  });
}
document.querySelectorAll('input[name="pool"]').forEach((r) => (r.onchange = syncPoolPanes));
syncPoolPanes();

// ---- player list: one input per player (first is the host) ----
function playerNames() {
  return Array.from($("players-list").querySelectorAll(".player-name")).map((i) => i.value.trim()).filter(Boolean);
}
// Tag the first row "host"; clear the tag from any others.
function markHostRow() {
  Array.from($("players-list").children).forEach((row, i) => {
    let tag = row.querySelector(".host-tag");
    if (i === 0 && !tag) { tag = document.createElement("span"); tag.className = "host-tag"; tag.textContent = "host"; row.insertBefore(tag, row.firstChild); }
    else if (i !== 0 && tag) tag.remove();
  });
}
function addPlayerRow(name = "", focus = false) {
  const row = document.createElement("div");
  row.className = "player-row";
  const input = document.createElement("input");
  input.type = "text"; input.className = "player-name"; input.value = name;
  input.placeholder = "player name"; input.autocomplete = "off";
  // Enter adds (and jumps to) the next row, so a host can rattle off names.
  input.addEventListener("keydown", (e) => { if (e.key === "Enter") { e.preventDefault(); addPlayerRow("", true); } });
  const del = document.createElement("button");
  del.type = "button"; del.className = "ghost player-del"; del.title = "remove player"; del.textContent = "×";
  del.addEventListener("click", () => {
    if ($("players-list").children.length <= 1) { input.value = ""; }  // keep at least one field
    else row.remove();
    markHostRow(); setupPreview();
  });
  row.append(input, del);
  $("players-list").appendChild(row);
  markHostRow();
  if (focus) input.focus();
  setupPreview();
  return input;
}
$("btn-add-player-row").onclick = () => addPlayerRow("", true);
["Alice", "Bob", "Carol", "Dave"].forEach((n) => addPlayerRow(n));

// A round timer entered as a number + a unit (min/hours/days) → whole seconds.
// `id` is the number input; its unit <select> is `${id}-unit` (value = seconds
// per unit). 0 means "manual close only".
function durationSeconds(id) {
  const n = Math.max(0, Number($(id).value) || 0);
  const per = Number($(id + "-unit").value) || 60;
  return Math.round(n * per);
}

// The ladder block hours are entered in the host's local time but stored as
// fixed UTC hours (so every viewer can render them in their own timezone).
// Convert a "HH:MM" local value to the equivalent whole UTC hour.
function blockHourToUtc(timeStr) {
  const h = Number((timeStr || "0:0").split(":")[0]) || 0;
  const d = new Date();
  d.setHours(h, 0, 0, 0);
  return d.getUTCHours();
}
// Echo what the two slots become in UTC so the host can see the conversion.
function updateBlockHint() {
  const m = blockHourToUtc($("cfg-block-morning").value);
  const e = blockHourToUtc($("cfg-block-evening").value);
  const fmt = (h) => String(h).padStart(2, "0") + ":00";
  $("cfg-block-hint").innerHTML =
    `The two daily availability slots, in <strong>your</strong> local time ` +
    `(stored as ${fmt(m)} / ${fmt(e)} UTC). Players see them in their own timezone.`;
}
$("cfg-block-morning").addEventListener("input", updateBlockHint);
$("cfg-block-evening").addEventListener("input", updateBlockHint);
updateBlockHint();

// Live setup preview + inline validation. Recomputes a one-line summary of what
// "Open packs & deal" will do, and blocks submit (with the reason) while the
// form has a problem the server would reject anyway.
function setupPreview() {
  const pool = selectedPool();
  const names = playerNames();
  const primaryRounds = Number($("cfg-primary-rounds").value);
  const secondaryRounds = Number($("cfg-secondary-rounds").value);
  const problems = [];

  if (names.length < 2) problems.push("add at least 2 players");
  if (new Set(names.map((n) => n.toLowerCase())).size !== names.length) problems.push("player names must be unique");
  if (!(primaryRounds >= 1) || !(secondaryRounds >= 1)) problems.push("each phase needs at least 1 round");

  let opened = null, openedLabel = "opened";
  if (pool === "manual") {
    opened = parseCardList($("cfg-cardlist").value).reduce((s, r) => s + (r.qty > 0 ? r.qty : 0), 0);
    openedLabel = "listed";
    if (opened === 0) problems.push("paste a card list (one “qty name” per line)");
  } else {
    if (pool === "scryfall" && !$("cfg-set").value.trim()) problems.push("enter a Scryfall set code");
    const packs = Number($("cfg-packs").value), size = Number($("cfg-packsize").value);
    if (packs >= 1 && size >= 1) opened = packs * size;
    else problems.push("packs and cards per pack must be ≥ 1");
  }

  const deals = ["c", "u", "r", "m"].map((k) => Number($("cfg-deal-" + k).value) || 0);
  const perPlayer = deals.reduce((a, b) => a + b, 0);

  let summary = "";
  if (opened != null && names.length) {
    summary = `${names.length} player${names.length === 1 ? "" : "s"} · ${opened} card${opened === 1 ? "" : "s"} ${openedLabel}`;
    summary += perPlayer === 0
      ? " · dealt round-robin (nothing held to the house)"
      : ` · dealing up to ${deals.join("/")} per player (≤${perPlayer} each) → leftovers to the house`;
  }

  const el = $("setup-preview"), btn = $("btn-setup");
  if (problems.length) {
    el.textContent = "Can’t start yet — " + problems.join("; ") + ".";
    el.classList.add("bad");
    btn.disabled = true;
  } else {
    el.textContent = summary;
    el.classList.remove("bad");
    btn.disabled = false;
  }
}

// Recompute on any edit within the setup form (covers typing, number steppers,
// and the pool radios); also after programmatic card-list edits below.
$("setup").addEventListener("input", setupPreview);
$("setup").addEventListener("change", setupPreview);
setupPreview();

// Roll a fresh seed (any non-negative integer reproduces a distinct deal).
$("btn-seed-rand").onclick = () => {
  $("cfg-seed").value = (typeof crypto !== "undefined" && crypto.getRandomValues)
    ? crypto.getRandomValues(new Uint32Array(1))[0]
    : Math.floor(Math.random() * 0xffffffff);
};

// ---- card picker: build the manual list from a set's card list ----
let pickerCards = [];

$("btn-load-set").onclick = async () => {
  const code = $("picker-set").value.trim() || "sample";
  const btn = $("btn-load-set");
  btn.disabled = true;
  $("picker-msg").textContent = "Loading…";
  try {
    const r = await api(`/api/set-cards?set=${encodeURIComponent(code)}`);
    pickerCards = r.cards || [];
    $("picker-msg").textContent = `${r.set_name}: ${pickerCards.length} cards. Click + to add (or type a quantity first).`;
    $("picker-tools").classList.toggle("hidden", pickerCards.length === 0);
    $("picker-filter").value = "";
    renderPicker();
  } catch (e) {
    pickerCards = [];
    $("picker-tools").classList.add("hidden");
    $("picker-list").innerHTML = "";
    $("picker-msg").textContent = `Could not load set: ${e.message}`;
  } finally {
    btn.disabled = false;
  }
};

// Colour-identity filter — shared with the player pages (see util.js for the
// at-most / at-least / exactly semantics).
function shownPickerCards() {
  const q = $("picker-filter").value.trim().toLowerCase();
  const f = readColorFilter($("picker-colors"));
  return pickerCards.filter((c) => (!q || c.name.toLowerCase().includes(q)) && matchesColorIdentity(c, f));
}

function renderPicker() {
  const list = $("picker-list");
  list.innerHTML = "";
  const cards = shownPickerCards();
  if (cards.length === 0) { list.innerHTML = `<p class="muted">No matching cards.</p>`; return; }
  cards.forEach((c) => {
    const row = document.createElement("div");
    row.className = "picker-row";
    row.innerHTML =
      `<input type="number" class="picker-qty" min="1" value="1" title="quantity" />` +
      `<button type="button" class="picker-add" title="add to list">+</button>` +
      `<span class="picker-name">${esc(c.name)}</span>` +
      `<span class="picker-colorcell">${colorPips(c.colors)}</span>` +
      `<span class="picker-rarity rarity-${c.rarity}">${c.rarity[0].toUpperCase()}</span>` +
      `<span class="picker-ref muted">${c.ref_price != null ? fmtUSD(c.ref_price) : "—"}</span>`;
    row.querySelector(".picker-add").onclick = () => {
      const qty = Math.max(1, Number(row.querySelector(".picker-qty").value) || 1);
      addToCardList([{ name: c.name, qty }]);
    };
    list.appendChild(row);
  });
}

$("picker-filter").oninput = renderPicker;
// Toggle colour buttons (the ✕ clears them all), or change the match mode.
$("picker-colors").addEventListener("click", (e) => handleColorClick($("picker-colors"), e, renderPicker));
$("picker-colors").addEventListener("change", (e) => { if (e.target.classList.contains("f-cmode")) renderPicker(); });
$("btn-add-all").onclick = () => addToCardList(shownPickerCards().map((c) => ({ name: c.name, qty: 1 })));

// Parse the textarea into ordered {qty,name} card rows (ignoring comments/blanks).
function parseCardList(text) {
  const rows = [];
  text.split(/\r?\n/).forEach((line) => {
    const t = line.trim();
    if (!t || t.startsWith("#") || t.startsWith("//")) return;
    const m = t.match(/^(\d+)\s*x?\s+(.+)$/i);
    if (m) rows.push({ qty: parseInt(m[1], 10), name: m[2].trim() });
    else rows.push({ qty: 1, name: t });
  });
  return rows;
}

// Merge additions into the card-list textarea (summing quantities by name),
// switch the pool source to "manual", and keep focus on building.
function addToCardList(additions) {
  const rows = parseCardList($("cfg-cardlist").value);
  const byName = new Map(rows.map((r) => [r.name.toLowerCase(), r]));
  additions.forEach((a) => {
    const ex = byName.get(a.name.toLowerCase());
    if (ex) ex.qty += a.qty;
    else { const r = { qty: a.qty, name: a.name }; rows.push(r); byName.set(a.name.toLowerCase(), r); }
  });
  $("cfg-cardlist").value = rows.filter((r) => r.qty > 0).map((r) => `${r.qty} ${r.name}`).join("\n");
  const manual = document.querySelector('input[name="pool"][value="manual"]');
  if (manual && !manual.checked) { manual.checked = true; syncPoolPanes(); }
  setupPreview(); // programmatic edits don't fire the form's input listener
}

$("btn-setup").onclick = async () => {
  const pool = selectedPool();
  // A blank Scryfall code used to fall back to the sample set silently; make
  // the host fix it instead of quietly drafting a different pool.
  if (pool === "scryfall" && !$("cfg-set").value.trim()) {
    toastError("Enter a Scryfall set code (e.g. dom), or pick another card pool source.");
    $("cfg-set").focus();
    return;
  }
  // Running setup on a live game wipes it (players, holdings, orders, tokens).
  if (state && state.phase !== "setup" &&
      !confirm("Start a new game? This replaces the game in progress and invalidates every player's current token.")) {
    return;
  }
  const names = playerNames();
  const config = {
    player_names: names,
    pool_source: pool,
    set: $("cfg-set").value.trim() || "sample",
    card_list: $("cfg-cardlist").value,
    starting_money: toCents($("cfg-money").value),
    debt_limit: toCents($("cfg-debt").value),
    primary_rounds: Number($("cfg-primary-rounds").value),
    secondary_rounds: Number($("cfg-secondary-rounds").value),
    primary_round_seconds: durationSeconds("cfg-primary-timer"),
    secondary_round_seconds: durationSeconds("cfg-secondary-timer"),
    num_packs: Number($("cfg-packs").value),
    pack_size: Number($("cfg-packsize").value),
    seed: Number($("cfg-seed").value),
    deal_commons: Number($("cfg-deal-c").value) || 0,
    deal_uncommons: Number($("cfg-deal-u").value) || 0,
    deal_rares: Number($("cfg-deal-r").value) || 0,
    deal_mythics: Number($("cfg-deal-m").value) || 0,
    house_offer_stdev_pct: Number($("cfg-house-stdev").value) || 0,
    house_offer_cap_pct: Number($("cfg-house-cap").value) || 0,
    starting_elo: Number($("cfg-elo-start").value),
    elo_k: Number($("cfg-elo-k").value),
    cancel_penalty: Number($("cfg-elo-cancel").value),
    max_games_per_week: Number($("cfg-elo-maxgames").value),
    schedule_window_days: Number($("cfg-elo-window").value),
    ladder_block_hours: [blockHourToUtc($("cfg-block-morning").value), blockHourToUtc($("cfg-block-evening").value)],
  };
  const btn = $("btn-setup");
  btn.disabled = true;
  btn.textContent = "Fetching & dealing…";
  try {
    const resp = await api("/api/setup", "POST", config);
    const host = resp.players.find((p) => p.admin) || resp.players[0];
    setToken(host.token);
    showTokens(resp.players);
    $("setup-details").open = false; // tuck the form away now a game is running
    await refresh();
  } catch (e) {
    toastError(e.message);
  } finally {
    btn.textContent = "Open packs & deal";
    setupPreview(); // re-enable only if the form is still valid
  }
};

function showTokens(players) {
  const tb = $("token-table").querySelector("tbody");
  tb.innerHTML = "";
  players.forEach((p) => {
    // A magic link logs that player in directly (the host link points at /admin).
    const link = `${location.origin}/${p.admin ? "admin" : ""}?t=${encodeURIComponent(p.token)}`;
    const tr = document.createElement("tr");
    tr.innerHTML = `<td>${esc(p.name)}${p.admin ? " (host)" : ""}</td>`;
    const td = document.createElement("td");
    const input = document.createElement("input");
    input.className = "linkfield"; input.readOnly = true; input.value = link;
    input.onclick = () => input.select();
    const btn = document.createElement("button");
    btn.className = "ghost copy"; btn.textContent = "copy link";
    btn.onclick = async () => {
      try { await navigator.clipboard.writeText(link); toast("Link copied — share it privately."); }
      catch { input.select(); toast("Press Ctrl/Cmd-C to copy."); }
    };
    td.appendChild(input); td.appendChild(btn);
    tr.appendChild(td);
    tb.appendChild(tr);
  });
  $("tokens").classList.remove("hidden");
}

$("btn-tokens-done").onclick = () => $("tokens").classList.add("hidden");
