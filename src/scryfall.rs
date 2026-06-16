//! Fetches a set's card list (names, rarities, images) from the Scryfall API
//! and turns it into a [`CardPool`]. Results are cached in memory per set code
//! so re-running setup (or starting a new game on the same set) is instant and
//! polite to Scryfall.

use crate::model::{CardPool, PoolCard, Rarity};
use serde::Deserialize;
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
    if let Some(pool) = cache().lock().unwrap().get(&code).cloned() {
        return Ok(pool);
    }
    let pool = fetch_scryfall(&code).await?;
    cache().lock().unwrap().insert(code, pool.clone());
    Ok(pool)
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
fn parse_price_cents(s: &str) -> Option<i64> {
    let f: f64 = s.trim().parse().ok()?;
    Some((f * 100.0).round() as i64)
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

async fn fetch_scryfall(code: &str) -> Result<CardPool, String> {
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .map_err(|e| format!("could not create HTTP client: {e}"))?;

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
            let ScryCard { name, rarity, set_name, image_uris, card_faces, prices, type_line, cmc, mana_cost } = card;
            if pool.set_name == code.to_uppercase() && !set_name.is_empty() {
                pool.set_name = set_name;
            }
            let image = image_uris.and_then(ImageUris::best).or_else(|| {
                card_faces
                    .and_then(|faces| faces.into_iter().next())
                    .and_then(|f| f.image_uris)
                    .and_then(ImageUris::best)
            });
            let ref_price = prices.and_then(|p| p.usd).as_deref().and_then(parse_price_cents);
            let mana_cost = mana_cost.filter(|s| !s.is_empty());
            let pc = PoolCard { name, rarity: rarity_from(&rarity), image, ref_price, type_line, cmc, mana_cost };
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
