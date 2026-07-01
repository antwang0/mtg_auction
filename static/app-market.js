"use strict";

// Inventory (plan table) and Market (gallery) tabs: filtering, sorting, the
// wants list, and the card modal. Shares state with app-core.js.
const WANTS_KEY = "mtg_auction_wants";

const RARITY_RANK = { common: 0, uncommon: 1, rare: 2, mythic: 3 };
const RARITIES = ["common", "uncommon", "rare", "mythic"];
const KNOWN_TYPES = ["Creature", "Planeswalker", "Instant", "Sorcery", "Artifact", "Enchantment", "Land", "Battle", "Kindred"];

let wants = loadWants();
let planSortKey = "rarity", planSortDir = -1;
let modalCardId = null;

function fmtMV(cmc) { return cmc === null || cmc === undefined ? "—" : String(cmc); }
function shortType(tl) { if (!tl) return "—"; const i = tl.indexOf("—"); return (i >= 0 ? tl.slice(0, i) : tl).trim(); }
function mineOf(c) { return myQty[c.id] || 0; }
// isTrading / phaseLabel live in util.js (shared with admin.js).
function loadWants() { try { return new Set(JSON.parse(localStorage.getItem(WANTS_KEY) || "[]")); } catch { return new Set(); } }
function saveWants() { localStorage.setItem(WANTS_KEY, JSON.stringify([...wants])); }
function star(name) { return wants.has(name) ? "★" : "☆"; }
function defaultPriceCents(id) {
  const c = cardById[id];
  return lastClearByCard[id] ?? (c && c.ref_price) ?? 100;
}

// ---- filtering & sorting ----
function getFilters(prefix) {
  const box = document.querySelector(`.filters[data-prefix="${prefix}"]`);
  const v = (cls) => box.querySelector(cls)?.value ?? "";
  return { q: v(".f-q").trim().toLowerCase(), rarity: v(".f-rarity"), type: v(".f-type"), mvmin: v(".f-mvmin"), mvmax: v(".f-mvmax"), show: v(".f-show"), color: readColorFilter(box.querySelector(".colorsel")) };
}
function cardMatches(c, f) {
  if (f.q && !c.name.toLowerCase().includes(f.q)) return false;
  if (f.rarity && c.rarity !== f.rarity) return false;
  if (f.type && !(c.type_line || "").includes(f.type)) return false;
  if (f.mvmin !== "" && (c.cmc == null || c.cmc < Number(f.mvmin))) return false;
  if (f.mvmax !== "" && (c.cmc == null || c.cmc > Number(f.mvmax))) return false;
  if (f.color && !matchesColorIdentity(c, f.color)) return false;
  if (f.show === "owned" && mineOf(c) <= 0) return false;
  if (f.show === "wanted" && !wants.has(c.name)) return false;
  return true;
}
function sortVal(c, key) {
  switch (key) {
    case "name": return c.name.toLowerCase();
    case "type": return shortType(c.type_line).toLowerCase();
    case "cmc": return c.cmc ?? -1;
    case "rarity": return RARITY_RANK[c.rarity];
    case "ref": return c.ref_price ?? -1;
    case "last": return lastClearByCard[c.id] ?? -1;
    case "supply": return c.supply;
    case "mine": return mineOf(c);
    case "want": return wants.has(c.name) ? 1 : 0;
    default: return 0;
  }
}
function sortCards(cards, key, dir) {
  return cards.slice().sort((a, b) => {
    const va = sortVal(a, key), vb = sortVal(b, key);
    let cmp = va < vb ? -1 : va > vb ? 1 : 0;
    if (cmp === 0) cmp = a.name.localeCompare(b.name);
    return cmp * dir;
  });
}
function populateFilterOptions() {
  const typesPresent = KNOWN_TYPES.filter((t) => state.cards.some((c) => (c.type_line || "").includes(t)));
  const typeSig = typesPresent.join(",");
  $$(".filters").forEach((box) => {
    const rs = box.querySelector(".f-rarity");
    if (rs.dataset.sig !== "r") { RARITIES.forEach((r) => rs.add(new Option(r, r))); rs.dataset.sig = "r"; }
    const ts = box.querySelector(".f-type");
    if (ts.dataset.sig !== typeSig) {
      const cur = ts.value; ts.length = 1;
      typesPresent.forEach((t) => ts.add(new Option(t, t)));
      ts.value = typesPresent.includes(cur) ? cur : "";
      ts.dataset.sig = typeSig;
    }
  });
}

function renderPlan() {
  if (!state) return;
  const f = getFilters("inv");
  const rows = sortCards(state.cards.filter((c) => cardMatches(c, f)), planSortKey, planSortDir);
  const tb = $("plan").querySelector("tbody");
  tb.innerHTML = "";
  rows.forEach((c) => {
    const tr = document.createElement("tr");
    tr.dataset.card = c.id;
    if (unaffordable(c)) tr.classList.add("unafford");
    tr.innerHTML =
      `<td class="want-cell"><button class="want-star ${wants.has(c.name) ? "on" : ""}" data-name="${esc(c.name)}">${star(c.name)}</button></td>` +
      `<td>${thumb(c.id)}${esc(c.name)} <span class="pips">${colorPips(c.colors)}</span>${orderBadges(c.id)}</td>` +
      `<td>${esc(shortType(c.type_line))}</td>` +
      `<td class="num">${fmtMV(c.cmc)}</td>` +
      `<td class="${rarityClass(c.rarity)}">${c.rarity}</td>` +
      `<td class="num">${fmtUSD(c.ref_price)}</td>` +
      `<td class="num">${fmtUSD(lastClearByCard[c.id] ?? null)}</td>` +
      `<td class="num">${c.supply}</td>` +
      `<td class="num you">${mineOf(c) || ""}</td>`;
    tb.appendChild(tr);
  });
  document.querySelector('.filters[data-prefix="inv"] .f-count').textContent = `${rows.length} / ${state.cards.length}`;
  $("plan").querySelectorAll("th[data-sort]").forEach((th) => {
    const k = th.dataset.sort;
    th.classList.toggle("sorted", k === planSortKey);
    th.dataset.arrow = k === planSortKey ? (planSortDir === 1 ? " ▲" : " ▼") : "";
  });
}

// Build a market tile element for a card (shared by the gallery and the
// active-orders section).
function galleryTile(c) {
  const mine = mineOf(c);
  const tile = document.createElement("div");
  tile.className = "tile" + (wants.has(c.name) ? " wanted" : "") + (unaffordable(c) ? " unafford" : "");
  tile.dataset.card = c.id;
  const art = c.image
    ? `<img class="tile-img" src="${esc(c.image)}" alt="" loading="lazy" />`
    : `<div class="tile-img no-img ${rarityClass(c.rarity)}">${esc(c.name)}</div>`;
  tile.innerHTML =
    `<button class="want-star ${wants.has(c.name) ? "on" : ""}" data-name="${esc(c.name)}">${star(c.name)}</button>` +
    art +
    `<div class="tile-name">${esc(c.name)}</div>` +
    `<div class="tile-sub muted">${esc(shortType(c.type_line))} · MV ${fmtMV(c.cmc)} <span class="pips">${colorPips(c.colors)}</span></div>` +
    `<div class="tile-foot"><span class="${rarityClass(c.rarity)}">${c.rarity}</span><span class="num">ref ${fmtUSD(c.ref_price)}</span></div>` +
    `<div class="tile-foot muted"><span>last ${fmtUSD(lastClearByCard[c.id] ?? null)}</span><span>sup ${c.supply}${mine ? ` · you ${mine}` : ""}</span></div>` +
    (orderBadges(c.id) ? `<div class="tile-orders">${orderBadges(c.id)}</div>` : "");
  return tile;
}

function renderGallery() {
  if (!state) return;
  const box = document.querySelector('.filters[data-prefix="mkt"]');
  const key = box.querySelector(".f-sort").value;
  const dir = Number(box.querySelector(".f-dir").dataset.dir || "-1");
  const f = getFilters("mkt");
  const rows = sortCards(state.cards.filter((c) => cardMatches(c, f)), key, dir);
  const g = $("gallery");
  g.innerHTML = "";
  rows.forEach((c) => g.appendChild(galleryTile(c)));
  box.querySelector(".f-count").textContent = `${rows.length} / ${state.cards.length}`;
}

// The cards the logged-in player has a live bid or offer on, shown at the top
// of the Market tab. Hidden when logged out or there are no open orders.
function renderMyOrders() {
  const section = $("orders-section"), g = $("orders-gallery");
  if (!section) return;
  const loggedIn = state && state.me != null;
  const ids = [...new Set([...Object.keys(myBidByCard), ...Object.keys(myOfferByCard)].map(Number))];
  if (!loggedIn || ids.length === 0) { section.classList.add("hidden"); g.innerHTML = ""; return; }
  const cards = ids.map((id) => cardById[id]).filter(Boolean).sort((a, b) => a.name.localeCompare(b.name));
  section.classList.remove("hidden");
  g.innerHTML = "";
  cards.forEach((c) => g.appendChild(galleryTile(c)));
  $("orders-mkt-count").textContent = `— ${cards.length} card${cards.length === 1 ? "" : "s"}`;
}

// ---- card modal ----
function openModal(id) {
  if (!cardById[id]) return;
  modalCardId = id;
  $("m-qty").value = 1;
  $("m-price").value = (defaultPriceCents(id) / 100).toFixed(2);
  $("m-error").textContent = "";
  $("modal").classList.remove("hidden");
  renderModalInfo();
}
function closeModal() { modalCardId = null; $("modal").classList.add("hidden"); }

function renderModalInfo() {
  const c = cardById[modalCardId];
  if (!c) return;
  const img = $("modal-img");
  if (c.image) { img.src = c.image; img.style.display = "block"; } else { img.removeAttribute("src"); img.style.display = "none"; }
  $("modal-name").textContent = c.name;
  $("modal-meta").innerHTML = [
    c.type_line ? esc(c.type_line) : null,
    c.mana_cost ? `Cost ${esc(c.mana_cost)}` : (c.cmc != null ? `MV ${fmtMV(c.cmc)}` : null),
    `<span class="${rarityClass(c.rarity)}">${c.rarity}</span>`,
    `Ref ${fmtUSD(c.ref_price)}`,
    `Last ${fmtUSD(lastClearByCard[c.id] ?? null)}`,
    `Supply ${c.supply}`,
    `You ${mineOf(c)}`,
  ].filter(Boolean).join(" · ");
  const wb = $("modal-want");
  wb.dataset.name = c.name;
  wb.textContent = wants.has(c.name) ? "★ Wanted — click to unmark" : "☆ Mark as wanted";

  // Your current orders on this card + last round's spread.
  const b = myBidByCard[c.id], o = myOfferByCard[c.id];
  const sp = latestClearByCard[c.id];
  let yours = [];
  if (b) yours.push(`<span class="buyer">Your bid ${fmtUSD(b.price)} ×${b.qty}</span>`);
  if (o) yours.push(`<span class="seller">Your ask ${fmtUSD(o.price)} ×${o.qty}</span>`);
  if (sp) {
    const cleared = sp.cleared != null ? `, cleared ${fmtUSD(sp.cleared)} (×${sp.volume})` : ", no fill";
    yours.push(`<span class="muted">R${sp.round}: bid ${fmtUSD(sp.best_bid)} / ask ${fmtUSD(sp.best_offer)}${cleared}</span>`);
  }
  $("modal-yours").innerHTML = yours.join("<br>") || `<span class="muted">No orders on this card yet.</span>`;

  // Price history.
  const hist = clearHistByCard[c.id] || [];
  $("modal-history").innerHTML = hist.length
    ? `<span class="muted">Cleared:</span> ` + hist.map((h) => `<span class="hist">R${h.round} ${fmtUSD(h.price)}</span>`).join(" ")
    : "";

  modalAfford();
}

// `reveal` shows the over-budget warning (red, with the negative balance). It's
// only set when the player presses Place Bid — while they're still typing an
// amount we keep the preview neutral so a transient large number doesn't flash
// a confusing negative indicator. Returns the computed state for the caller.
function modalAfford(reveal = false) {
  const c = cardById[modalCardId];
  if (!c) return null;
  const loggedIn = state && state.me != null;
  const live = isTrading(state);
  const qty = Math.max(0, Number($("m-qty").value) || 0);
  const price = toCents($("m-price").value);
  const commit = qty * price;
  const existing = myBidByCard[c.id] ? myBidByCard[c.id].qty * myBidByCard[c.id].price : 0;
  const availForBid = (state ? state.my_available : 0) + existing;
  const left = availForBid - commit;
  const owned = mineOf(c);
  const over = left < 0;

  // Don't disable Place Bid just for being over budget — pressing it is what
  // surfaces the shortfall (see modalOrder).
  $("m-bid").disabled = !loggedIn || !live || qty < 1;
  $("m-offer").disabled = !loggedIn || !live || qty < 1 || qty > owned;
  const hold = owned ? ` · you hold ${owned}` : ` · you don't own this`;
  const af = $("m-afford");
  if (!loggedIn) af.textContent = "Log in to trade.";
  else if (!live) af.textContent = "Auction is closed.";
  else if (over && !reveal) af.innerHTML = `Bid commits <b>${fmtUSD(commit)}</b>${hold}`;
  else af.innerHTML = `Bid commits <b>${fmtUSD(commit)}</b> · ${fmtUSD(left)} left${hold}`;
  af.classList.toggle("bad", live && loggedIn && over && reveal);
  return { over, live, loggedIn, qty };
}

async function modalOrder(kind) {
  if (modalCardId == null) return;
  const price = toCents($("m-price").value);
  // A bid at/above your own offer (or an offer at/below your own bid) would
  // cross — you'd trade with yourself. The server rejects it; catch it here too
  // for instant feedback. (The matcher also never self-trades.)
  if (kind === "bid") {
    const a = modalAfford(true);
    if (a && a.over) { $("m-error").textContent = "This bid is more than you can cover."; return; }
    const o = myOfferByCard[modalCardId];
    if (o && price >= o.price) {
      $("m-error").textContent = `This bid would cross your own offer (${fmtUSD(o.price)}) — keep it below your offer price.`;
      return;
    }
  } else if (kind === "offer") {
    const b = myBidByCard[modalCardId];
    if (b && price <= b.price) {
      $("m-error").textContent = `This offer would cross your own bid (${fmtUSD(b.price)}) — keep it above your bid price.`;
      return;
    }
  }
  const card = modalCardId;
  const qty = Number($("m-qty").value);
  try {
    await api(kind === "bid" ? "/api/bid" : "/api/offer", "POST", { player: state.me, card, qty, price });
    $("m-error").textContent = "";
    await refresh();
  } catch (e) { $("m-error").textContent = e.message; }
}

function toggleWant(name) {
  if (wants.has(name)) wants.delete(name); else wants.add(name);
  saveWants();
  renderPlan(); renderGallery();
  if (modalCardId != null) renderModalInfo();
}

// Modal trade controls.
$("m-qty").oninput = () => { $("m-error").textContent = ""; modalAfford(); };
$("m-price").oninput = () => { $("m-error").textContent = ""; modalAfford(); };
$$(".step").forEach((b) => (b.onclick = () => {
  const cents = Math.max(0, toCents($("m-price").value) + Number(b.dataset.delta));
  $("m-price").value = (cents / 100).toFixed(2);
  modalAfford();
}));
$("m-ref").onclick = () => { const c = cardById[modalCardId]; if (c && c.ref_price != null) { $("m-price").value = (c.ref_price / 100).toFixed(2); modalAfford(); } };
$("m-last").onclick = () => { const p = lastClearByCard[modalCardId]; if (p != null) { $("m-price").value = (p / 100).toFixed(2); modalAfford(); } };
$("m-bid").onclick = () => modalOrder("bid");
$("m-offer").onclick = () => modalOrder("offer");

// Filter bars
$$(".filters").forEach((box) => {
  const prefix = box.dataset.prefix;
  const rerender = () => { (prefix === "inv" ? renderPlan() : renderGallery()); saveUi(); };
  box.addEventListener("input", rerender);
  box.addEventListener("change", rerender);
  // Colour buttons are <button> toggles, so they don't fire input/change.
  const colorsel = box.querySelector(".colorsel");
  if (colorsel) colorsel.addEventListener("click", (e) => handleColorClick(colorsel, e, rerender));
});
document.querySelector('.filters[data-prefix="mkt"] .f-dir').onclick = (e) => {
  const b = e.currentTarget;
  const dir = Number(b.dataset.dir || "-1") * -1;
  b.dataset.dir = dir; b.textContent = dir === 1 ? "▲" : "▼";
  renderGallery(); saveUi();
};

// Plan header sorting
$("plan").querySelectorAll("th[data-sort]").forEach((th) => (th.onclick = () => {
  const k = th.dataset.sort;
  if (planSortKey === k) planSortDir = -planSortDir;
  else { planSortKey = k; planSortDir = (k === "name" || k === "type") ? 1 : -1; }
  renderPlan(); saveUi();
}));

// Click-to-enlarge / want-star (delegated). Stars handled first.
document.addEventListener("click", (e) => {
  const s = e.target.closest(".want-star");
  if (s) { e.stopPropagation(); toggleWant(s.dataset.name); return; }
  if (e.target.closest(".modal")) return;
  const el = e.target.closest("[data-card]");
  if (el) openModal(Number(el.dataset.card));
});

$("modal-close").onclick = closeModal;
$("modal").querySelector(".modal-backdrop").onclick = closeModal;
$("modal-want").onclick = (e) => toggleWant(e.currentTarget.dataset.name);
document.addEventListener("keydown", (e) => { if (e.key === "Escape") closeModal(); });
