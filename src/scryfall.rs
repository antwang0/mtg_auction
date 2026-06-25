//! Fetches a set's card list (names, rarities, images) from the Scryfall API
//! and turns it into a [`CardPool`]. Results are cached in memory per set code
//! so re-running setup (or starting a new game on the same set) is instant and
//! polite to Scryfall.

use crate::model::{CardPool, PoolCard, Rarity};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// User-Agent Scryfall asks API clients to send.
const USER_AGENT: &str = "mtg_auction/0.1 (draft auction game)";

fn cache() -> &'static Mutex<HashMap<String, CardPool>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CardPool>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Resolve a set code to a card pool. `""` or `"sample"` returns the built-in
/// offline pool; anything else is fetched from Scryfall (and cached).
pub async fn fetch_pool(set: &str) -> Result<CardPool, String> {
    let code = set.trim().to_lowercase();
    if code.is_empty() || code == "sample" {
        return Ok(CardPool::sample());
    }
    if let Some(pool) = cache().lock().unwrap_or_else(|e| e.into_inner()).get(&code).cloned() {
        return Ok(pool);
    }
    let pool = fetch_scryfall(&code).await?;
    cache().lock().unwrap_or_else(|e| e.into_inner()).insert(code, pool.clone());
    Ok(pool)
}

/// Build a manual card pool from a decklist-style text, one `<qty> <name>` per
/// line (e.g. `3 Lightning Bolt`). Card metadata (rarity, image, price, type) is
/// fetched from Scryfall by name as a best effort — names Scryfall doesn't know
/// (typos, custom cards) and an unreachable Scryfall both fall back to a plain
/// card, so a manual pool always works, even offline.
pub async fn fetch_decklist_pool(text: &str) -> Result<CardPool, String> {
    let entries = parse_decklist(text);
    if entries.is_empty() {
        return Err("no cards found in the list — use lines like `3 Lightning Bolt`".into());
    }

    // Best-effort metadata, keyed by lowercased name.
    let names: Vec<&str> = entries.iter().map(|(_, name)| name.as_str()).collect();
    let meta = match build_client() {
        Ok(client) => fetch_collection(&client, &names).await.unwrap_or_default(),
        Err(_) => HashMap::new(),
    };

    let exact: Vec<(PoolCard, u32)> = entries
        .into_iter()
        .map(|(qty, name)| {
            let card = meta.get(&name.to_lowercase()).cloned().unwrap_or(PoolCard {
                name,
                rarity: Rarity::Common,
                image: None,
                ref_price: None,
                type_line: None,
                cmc: None,
                mana_cost: None,
            });
            (card, qty)
        })
        .collect();

    Ok(CardPool { set_name: "Custom list".to_string(), exact: Some(exact), ..Default::default() })
}

/// Parse a decklist text into `(quantity, name)` rows. Each non-empty,
/// non-comment line is an optional leading quantity (`3`, `3x`, default `1`)
/// followed by the card name. Quantities are capped so one line can't blow up
/// the pile; the per-game total is bounded again at setup.
pub fn parse_decklist(text: &str) -> Vec<(u32, String)> {
    let mut rows = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }
        let (qty, name) = match line.split_once(char::is_whitespace) {
            Some((head, rest)) => match head.trim_end_matches(['x', 'X']).parse::<u32>() {
                Ok(q) => (q, rest.trim()),
                Err(_) => (1, line), // no leading count: the whole line is the name
            },
            None => (1, line),
        };
        if name.is_empty() || qty == 0 {
            continue;
        }
        rows.push((qty.min(100_000), name.to_string()));
    }
    rows
}

/// Look up card metadata by exact name via Scryfall's batch `/cards/collection`
/// endpoint (≤75 identifiers per request). Returns a map keyed by lowercased
/// canonical name; names Scryfall doesn't recognise are simply absent.
async fn fetch_collection(client: &reqwest::Client, names: &[&str]) -> Result<HashMap<String, PoolCard>, String> {
    #[derive(Serialize)]
    struct Identifier<'a> {
        name: &'a str,
    }
    #[derive(Serialize)]
    struct CollectionBody<'a> {
        identifiers: Vec<Identifier<'a>>,
    }
    #[derive(Deserialize)]
    struct CollectionResponse {
        data: Vec<ScryCard>,
    }

    let mut out: HashMap<String, PoolCard> = HashMap::new();
    let chunks: Vec<&[&str]> = names.chunks(75).collect();
    for (i, chunk) in chunks.iter().enumerate() {
        let body = CollectionBody { identifiers: chunk.iter().map(|&name| Identifier { name }).collect() };
        let resp = client
            .post("https://api.scryfall.com/cards/collection")
            .header("Accept", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("could not reach Scryfall: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("Scryfall returned HTTP {}", resp.status().as_u16()));
        }
        let parsed: CollectionResponse = resp.json().await.map_err(|e| format!("unexpected Scryfall response: {e}"))?;
        for card in parsed.data {
            let pc = pool_card_from(card);
            out.insert(pc.name.to_lowercase(), pc);
        }
        if i + 1 < chunks.len() {
            tokio::time::sleep(Duration::from_millis(100)).await; // be polite between requests
        }
    }
    Ok(out)
}

#[derive(Deserialize)]
struct CardList {
    data: Vec<ScryCard>,
    has_more: bool,
    next_page: Option<String>,
}

#[derive(Deserialize)]
struct ScryCard {
    name: String,
    rarity: String,
    #[serde(default)]
    set_name: String,
    #[serde(default)]
    image_uris: Option<ImageUris>,
    #[serde(default)]
    card_faces: Option<Vec<CardFace>>,
    #[serde(default)]
    prices: Option<Prices>,
    #[serde(default)]
    type_line: Option<String>,
    #[serde(default)]
    cmc: Option<f64>,
    #[serde(default)]
    mana_cost: Option<String>,
}

#[derive(Deserialize)]
struct Prices {
    /// Scryfall's TCGplayer-derived market price, as a string like "1.23".
    usd: Option<String>,
}

/// Parse a Scryfall dollar string (e.g. "1.23") into whole cents.
///
/// Done by integer arithmetic on the decimal digits rather than through an
/// `f64`, so prices never pick up binary floating-point rounding error (e.g.
/// "0.29" must be exactly 29 cents, not 28). A third fractional digit, if
/// present, rounds the result half-up.
fn parse_price_cents(s: &str) -> Option<i64> {
    let s = s.trim();
    let (whole, frac) = s.split_once('.').unwrap_or((s, ""));
    if whole.is_empty() && frac.is_empty() {
        return None;
    }
    // Both parts must be pure ASCII digits (reject "1.2.3", "1e3", "abc", "-1").
    if !whole.bytes().all(|b| b.is_ascii_digit()) || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let dollars: i64 = if whole.is_empty() { 0 } else { whole.parse().ok()? };
    let f = frac.as_bytes();
    let digit = |i: usize| f.get(i).map_or(0, |b| (b - b'0') as i64);
    // First two fractional digits are the cents; a third rounds half-up.
    let mut cents = digit(0) * 10 + digit(1);
    if f.get(2).is_some_and(|b| *b >= b'5') {
        cents += 1; // carries cleanly into dollars via the addition below
    }
    dollars.checked_mul(100)?.checked_add(cents)
}

#[derive(Deserialize)]
struct CardFace {
    #[serde(default)]
    image_uris: Option<ImageUris>,
}

#[derive(Deserialize)]
struct ImageUris {
    normal: Option<String>,
    small: Option<String>,
}

impl ImageUris {
    fn best(self) -> Option<String> {
        self.normal.or(self.small)
    }
}

fn rarity_from(s: &str) -> Rarity {
    match s {
        "common" => Rarity::Common,
        "uncommon" => Rarity::Uncommon,
        "mythic" => Rarity::Mythic,
        _ => Rarity::Rare, // rare, special, bonus, ...
    }
}

/// A reqwest client carrying the User-Agent Scryfall asks clients to send.
fn build_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .map_err(|e| format!("could not create HTTP client: {e}"))
}

/// Convert a raw Scryfall card into a [`PoolCard`], picking the best image
/// (front face for double-faced cards) and parsing its USD price into cents.
fn pool_card_from(card: ScryCard) -> PoolCard {
    let ScryCard { name, rarity, image_uris, card_faces, prices, type_line, cmc, mana_cost, .. } = card;
    let image = image_uris.and_then(ImageUris::best).or_else(|| {
        card_faces
            .and_then(|faces| faces.into_iter().next())
            .and_then(|f| f.image_uris)
            .and_then(ImageUris::best)
    });
    let ref_price = prices.and_then(|p| p.usd).as_deref().and_then(parse_price_cents);
    let mana_cost = mana_cost.filter(|s| !s.is_empty());
    PoolCard { name, rarity: rarity_from(&rarity), image, ref_price, type_line, cmc, mana_cost }
}

async fn fetch_scryfall(code: &str) -> Result<CardPool, String> {
    let client = build_client()?;

    let mut pool = CardPool { set_name: code.to_uppercase(), ..Default::default() };
    let mut next: Option<String> = None;
    let mut first = true;

    // Follow Scryfall's pagination, with a hard cap as a runaway guard.
    for _ in 0..30 {
        let req = match &next {
            None if first => client
                .get("https://api.scryfall.com/cards/search")
                .query(&[("q", format!("set:{code} game:paper")), ("unique", "cards".into()), ("order", "name".into())]),
            Some(url) => client.get(url),
            None => break,
        };
        first = false;

        let resp = req
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| format!("could not reach Scryfall: {e}"))?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(format!("set '{code}' was not found (check the Scryfall set code)"));
        }
        if !resp.status().is_success() {
            return Err(format!("Scryfall returned HTTP {}", resp.status().as_u16()));
        }

        let list: CardList = resp.json().await.map_err(|e| format!("unexpected Scryfall response: {e}"))?;
        for card in list.data {
            // Adopt the set's display name from the first card that carries one.
            if pool.set_name == code.to_uppercase() && !card.set_name.is_empty() {
                pool.set_name = card.set_name.clone();
            }
            let pc = pool_card_from(card);
            match pc.rarity {
                Rarity::Common => pool.commons.push(pc),
                Rarity::Uncommon => pool.uncommons.push(pc),
                Rarity::Rare => pool.rares.push(pc),
                Rarity::Mythic => pool.mythics.push(pc),
            }
        }

        if list.has_more {
            next = list.next_page;
            // Scryfall asks for ~100ms between requests.
            tokio::time::sleep(Duration::from_millis(100)).await;
        } else {
            break;
        }
    }

    if pool.is_empty() {
        return Err(format!("set '{code}' has no draftable cards"));
    }
    Ok(pool)
}

#[cfg(test)]
mod tests {
    use super::{parse_decklist, parse_price_cents};

    #[test]
    fn parses_quantities_and_names() {
        let list = parse_decklist("3 Lightning Bolt\n1 Black Lotus\n2 Counterspell");
        assert_eq!(list, vec![
            (3, "Lightning Bolt".to_string()),
            (1, "Black Lotus".to_string()),
            (2, "Counterspell".to_string()),
        ]);
    }

    #[test]
    fn decklist_is_lenient() {
        let list = parse_decklist(
            "  4x Forest \n\
             # a comment\n\
             \n\
             // another comment\n\
             Sol Ring\n\
             0 Skip Me\n\
             2 Sword of Fire and Ice",
        );
        assert_eq!(list, vec![
            (4, "Forest".to_string()),          // "4x" prefix and surrounding space
            (1, "Sol Ring".to_string()),        // no count defaults to 1
            (2, "Sword of Fire and Ice".to_string()), // multi-word names kept whole
        ]);
        // blank lines, comments, and a zero quantity are dropped.
    }

    #[test]
    fn decklist_caps_absurd_quantities() {
        let list = parse_decklist("999999999 Island");
        assert_eq!(list, vec![(100_000, "Island".to_string())]);
    }

    #[test]
    fn parses_plain_dollar_strings() {
        assert_eq!(parse_price_cents("1.23"), Some(123));
        assert_eq!(parse_price_cents("0.25"), Some(25));
        assert_eq!(parse_price_cents("1234.56"), Some(123456));
        assert_eq!(parse_price_cents(" 9.99 "), Some(999));
    }

    #[test]
    fn handles_missing_or_short_fractions() {
        assert_eq!(parse_price_cents("5"), Some(500));
        assert_eq!(parse_price_cents("1.2"), Some(120));
        assert_eq!(parse_price_cents(".5"), Some(50));
        assert_eq!(parse_price_cents("1."), Some(100));
    }

    #[test]
    fn no_binary_float_rounding_error() {
        // "0.29" through an f64 is 28.999…; integer parsing keeps it exact.
        assert_eq!(parse_price_cents("0.29"), Some(29));
        assert_eq!(parse_price_cents("4.10"), Some(410));
    }

    #[test]
    fn third_decimal_rounds_half_up() {
        assert_eq!(parse_price_cents("1.005"), Some(101));
        assert_eq!(parse_price_cents("1.004"), Some(100));
        assert_eq!(parse_price_cents("1.999"), Some(200)); // carries into dollars
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_price_cents(""), None);
        assert_eq!(parse_price_cents("abc"), None);
        assert_eq!(parse_price_cents("1.2.3"), None);
        assert_eq!(parse_price_cents("1e3"), None);
        assert_eq!(parse_price_cents("-1.00"), None);
    }
}
