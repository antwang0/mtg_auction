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

// ---- game mode: standard vs league ----
function selectedMode() {
  const r = document.querySelector('input[name="mode"]:checked');
  return r ? r.value : "standard";
}
// League hides everything about packs/phases/pool and shows the league pane.
function syncModePanes() {
  const league = selectedMode() === "league";
  document.querySelectorAll(".mode-pane").forEach((p) => (p.hidden = p.dataset.mode !== selectedMode()));
  document.querySelectorAll("#setup .std-only").forEach((p) => (p.hidden = league));
  $("btn-setup").textContent = league ? "Start league" : "Open packs & deal";
  updateLeagueHint();
}
document.querySelectorAll('input[name="mode"]').forEach((r) => (r.onchange = () => { syncModePanes(); setupPreview(); }));

// The league schedule is expressed in a fixed timezone (a UTC-offset in
// minutes), not each viewer's local zone. Dates entered as calendar days map to
// an epoch *day number* (days since 1970-01-01), independent of any zone.
function leagueTzMins() { return Number($("cfg-lg-tz").value) || 0; }
function leagueCloseHour() { return Number(($("cfg-lg-time").value || "20:00").split(":")[0]) || 0; }

// "YYYY-MM-DD" → epoch day number (0 if blank/invalid).
function dateStrToEpochDay(s) {
  const m = (s || "").match(/^(\d{4})-(\d{2})-(\d{2})$/);
  if (!m) return 0;
  return Math.floor(Date.UTC(+m[1], +m[2] - 1, +m[3]) / 86400000);
}
function epochDayToDateStr(day) {
  return new Date(day * 86400000).toISOString().slice(0, 10);
}
// Today's epoch day in the league timezone, and the coming Sunday from it.
function leagueTodayDay() {
  return Math.floor((Date.now() + leagueTzMins() * 60000) / 86400000);
}
function comingSundayDay() {
  const t = leagueTodayDay();
  const wd = ((t + 4) % 7 + 7) % 7; // 0 = Sunday
  return t + (7 - wd) % 7;
}

// Fill the schedule with its round-aligned defaults unless the host has
// already set dates: matchmaking today (the first N matches are assigned
// immediately), an auction closing at the end of each round except the last —
// every N weeks, rounds − 1 times.
let leagueDatesTouched = false;
function fillLeagueDateDefaults(force = false) {
  if (leagueDatesTouched && !force) return;
  const n = Math.max(1, Number($("cfg-lg-batch").value) || 2);
  const rounds = Math.max(1, Number($("cfg-lg-rounds").value) || 3);
  const today = leagueTodayDay();
  $("cfg-lg-mm").value = epochDayToDateStr(today);
  $("cfg-lg-period").value = Math.min(n, 8);
  $("cfg-lg-first").value = epochDayToDateStr(today + 7 * n);
  $("cfg-lg-last").value = epochDayToDateStr(today + 7 * n * Math.max(1, rounds - 1));
}
["cfg-lg-mm", "cfg-lg-first", "cfg-lg-last", "cfg-lg-period"].forEach((id) =>
  $(id).addEventListener("input", () => { leagueDatesTouched = true; updateLeagueHint(); })
);
// Changing the rhythm re-derives the (untouched) schedule.
["cfg-lg-batch", "cfg-lg-rounds"].forEach((id) =>
  $(id).addEventListener("input", () => { fillLeagueDateDefaults(); updateLeagueHint(); })
);

// The full season, laid out so the host can sanity-check the schedule before
// starting: one line per round (match window + the auction that closes with
// it), built from the same inputs the server will use.
function updateLeagueHint() {
  const n = Math.max(1, Number($("cfg-lg-batch").value) || 2);
  const rounds = Math.max(1, Number($("cfg-lg-rounds").value) || 3);
  const mm = dateStrToEpochDay($("cfg-lg-mm").value) || leagueTodayDay();
  const first = dateStrToEpochDay($("cfg-lg-first").value);
  const last = dateStrToEpochDay($("cfg-lg-last").value);
  const period = Math.max(1, Number($("cfg-lg-period").value) || n);
  const hh = String(leagueCloseHour()).padStart(2, "0");
  const tz = $("cfg-lg-tz").selectedOptions[0].textContent.split(" ")[0];
  // The auction series: first, then every `period` weeks up to `last`.
  const auctions = [];
  if (first) {
    for (let a = first; (!last || a <= last) && auctions.length < 20; a += 7 * period) auctions.push(a);
  }
  // Match play-by deadlines snap to the auction series (its cadence continues
  // past the last auction for the final round); without a first-auction date
  // they fall back to N weeks per round.
  const closeDay = (r) => (first ? first + (r - 1) * 7 * period : mm + r * 7 * n);
  const lines = [
    `<b>Season preview</b> — ${rounds} round${rounds === 1 ? "" : "s"} × ${n} match${n === 1 ? "" : "es"} ` +
    `(${rounds * n} matches per player), ${auctions.length} auction${auctions.length === 1 ? "" : "s"}:`,
  ];
  for (let r = 1; r <= rounds; r++) {
    const start = r === 1 ? mm : closeDay(r - 1);
    let line = `Round ${r}: matches assigned ${epochDayToDateStr(start)}, play by ${epochDayToDateStr(closeDay(r))} at ${hh}:00 ${tz}`;
    line += r <= auctions.length ? ` · auction ${r} closes at the same time` : ` (no auction)`;
    lines.push(line);
  }
  for (let a = rounds + 1; a <= auctions.length; a++) {
    lines.push(`Auction ${a} closes ${epochDayToDateStr(auctions[a - 1])} at ${hh}:00 ${tz} (after the last round!)`);
  }
  lines.push(`Season ends ${epochDayToDateStr(closeDay(rounds))}. Rounds are strictly synchronized: everyone's next-round matches post together when the previous round closes.`);
  $("cfg-lg-hint").innerHTML = lines.join("<br>");
}
// Changing the timezone re-derives the default dates (host edits stick).
$("cfg-lg-tz").addEventListener("change", () => { fillLeagueDateDefaults(); updateLeagueHint(); });
$("cfg-lg-time").addEventListener("input", updateLeagueHint);
$("cfg-lg-period").addEventListener("input", updateLeagueHint);
fillLeagueDateDefaults(true);
syncModePanes();

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
  // A comma-separated list in a name field ("john,test1,test2") is a roster,
  // not one player: split it into rows. Runs on change/Enter (not on every
  // keystroke) so it catches any entry path — paste, middle-click, drag-drop.
  const splitIfList = () => {
    const v = input.value;
    if (!v.includes(",")) return false;
    input.value = "";
    $("import-players-msg").textContent = importNames(parseCsvNames(v));
    return true;
  };
  input.addEventListener("change", splitIfList);
  // Enter adds (and jumps to) the next row, so a host can rattle off names.
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") { e.preventDefault(); if (!splitIfList()) addPlayerRow("", true); }
  });
  // Pasting a comma-separated (or multi-line) list imports it immediately.
  input.addEventListener("paste", (e) => {
    const text = (e.clipboardData || window.clipboardData).getData("text") || "";
    if (!/[,\n]/.test(text)) return; // an ordinary single-name paste
    e.preventDefault();
    $("import-players-msg").textContent = importNames(parseCsvNames(text));
  });
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
const DEFAULT_PLAYERS = ["Alice", "Bob", "Carol", "Dave"];
DEFAULT_PLAYERS.forEach((n) => addPlayerRow(n));

// ---- import players from a CSV / text file or a pasted list ----
// A single line containing commas is a comma-separated list of names
// ("Alice, Bob, Carol"). Otherwise each line is one player; the name is the
// first CSV field (quotes handled), so a plain name list or a multi-column CSV
// both work. A leading header row like "name" / "player" is skipped.
function parseCsvNames(text) {
  const lines = text.split(/\r?\n/).filter((l) => l.trim());
  if (lines.length === 1 && lines[0].includes(",") && !lines[0].includes('"')) {
    return lines[0].split(",").map((n) => n.trim()).filter(Boolean);
  }
  const names = [];
  lines.forEach((line) => {
    const quoted = line.match(/^\s*"((?:[^"]|"")*)"/); // a leading "quoted" field
    const raw = quoted ? quoted[1].replace(/""/g, '"') : line.split(",")[0];
    const name = raw.trim();
    if (name) names.push(name);
  });
  if (names.length && /^(name|player|players|player[ _]?name)$/i.test(names[0])) names.shift();
  return names;
}

// Merge names into the roster (replacing the untouched sample roster, skipping
// duplicates, capping at 200) and return a summary message.
function importNames(names) {
  if (!names.length) return "No names found.";
  const current = playerNames();
  const isDefaults = current.length === DEFAULT_PLAYERS.length && current.every((n, i) => n === DEFAULT_PLAYERS[i]);
  if (isDefaults) $("players-list").innerHTML = "";
  const have = new Set(playerNames().map((n) => n.toLowerCase()));
  let added = 0, dupes = 0, capped = 0;
  for (const name of names) {
    if (have.has(name.toLowerCase())) { dupes++; continue; }
    if ($("players-list").querySelectorAll(".player-name").length >= 200) { capped++; continue; }
    addPlayerRow(name);
    have.add(name.toLowerCase());
    added++;
  }
  pruneEmptyPlayerRows();
  markHostRow();
  setupPreview();
  return `Imported ${added} player${added === 1 ? "" : "s"}` +
    (dupes ? `, skipped ${dupes} duplicate${dupes === 1 ? "" : "s"}` : "") +
    (capped ? `, ${capped} over the 200-player limit` : "") + ".";
}

// Drop empty name rows (but always leave at least one field to type in).
function pruneEmptyPlayerRows() {
  Array.from($("players-list").children).forEach((row) => {
    const inp = row.querySelector(".player-name");
    if (inp && !inp.value.trim() && $("players-list").children.length > 1) row.remove();
  });
}

$("btn-import-players").onclick = () => $("import-players-csv").click();
$("import-players-csv").addEventListener("change", async (e) => {
  const file = e.target.files && e.target.files[0];
  const msg = $("import-players-msg");
  if (!file) return;
  try {
    msg.textContent = importNames(parseCsvNames(await file.text()));
  } catch (err) {
    msg.textContent = "Could not read that file: " + err.message;
  }
  e.target.value = ""; // let the same file be re-imported
});

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
  const league = selectedMode() === "league";
  const names = playerNames();
  const primaryRounds = Number($("cfg-primary-rounds").value);
  const secondaryRounds = Number($("cfg-secondary-rounds").value);
  const problems = [];

  if (names.length < 2) problems.push("add at least 2 players");
  if (new Set(names.map((n) => n.toLowerCase())).size !== names.length) problems.push("player names must be unique");

  // League games skip the pack/phase/pool settings entirely.
  if (league) {
    const first = dateStrToEpochDay($("cfg-lg-first").value);
    const last = dateStrToEpochDay($("cfg-lg-last").value);
    const mm = dateStrToEpochDay($("cfg-lg-mm").value);
    if (!first) problems.push("set a first auction date");
    if (last && last < first) problems.push("the last auction date is before the first");
    if (mm && first && mm > first) problems.push("matchmaking must start on or before the first auction");
    const el = $("setup-preview"), btn = $("btn-setup");
    if (problems.length) {
      el.textContent = "Can’t start yet — " + problems.join("; ") + ".";
      el.classList.add("bad");
      btn.disabled = true;
    } else {
      el.textContent =
        `${names.length} players · league — ${$("cfg-lg-packs").value} packs each, ` +
        `$${$("cfg-lg-stipend").value} stipend per close · matchmaking ${$("cfg-lg-mm").value || "?"}, ` +
        `first auction ${$("cfg-lg-first").value}${last ? `, last ${$("cfg-lg-last").value}` : " (no end)"}`;
      el.classList.remove("bad");
      btn.disabled = false;
    }
    return;
  }

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
  const league = selectedMode() === "league";
  // A blank Scryfall code used to fall back to the sample set silently; make
  // the host fix it instead of quietly drafting a different pool.
  if (!league && pool === "scryfall" && !$("cfg-set").value.trim()) {
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
    mode: selectedMode(),
    league_packs_per_player: Number($("cfg-lg-packs").value) || 0,
    weekly_stipend: toCents($("cfg-lg-stipend").value),
    league_tz_offset_mins: leagueTzMins(),
    league_close_hour: leagueCloseHour(),
    league_period_weeks: Number($("cfg-lg-period").value) || 1,
    league_pending_per_player: Number($("cfg-lg-batch").value) || 2,
    league_rounds: Number($("cfg-lg-rounds").value) || 3,
    league_first_auction_day: dateStrToEpochDay($("cfg-lg-first").value),
    league_last_auction_day: dateStrToEpochDay($("cfg-lg-last").value),
    league_matchmaking_start_day: dateStrToEpochDay($("cfg-lg-mm").value),
    player_names: names,
    pool_source: pool,
    // League games use the set code to pin card lookups (image/rarity) to the
    // set being played; standard games use it as the Scryfall pool source.
    set: (league ? $("cfg-lg-set").value.trim() : $("cfg-set").value.trim()) || "sample",
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
  btn.textContent = league ? "Starting league…" : "Fetching & dealing…";
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
    btn.textContent = league ? "Start league" : "Open packs & deal";
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
