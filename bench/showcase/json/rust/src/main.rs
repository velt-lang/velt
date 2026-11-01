//! Typed JSON on the standard corpora: the structs of ../typed.vlt with serde derive, decoded
//! by serde_json, sonic-rs or simd-json (unknown members skipped, serde's default), and written
//! back. Usage: showcase_json_typed <serde|sonic|simd> <canada|citm|twitter> <file> <rounds> <parse|write>
use indexmap::IndexMap;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct Geometry {
    r#type: String,
    coordinates: Vec<Vec<Vec<f64>>>,
}
#[derive(Serialize, Deserialize)]
struct Properties {
    name: String,
}
#[derive(Serialize, Deserialize)]
struct Feature {
    r#type: String,
    properties: Properties,
    geometry: Geometry,
}
#[derive(Serialize, Deserialize)]
struct Canada {
    r#type: String,
    features: Vec<Feature>,
}

#[derive(Serialize, Deserialize)]
struct Hashtag {
    text: String,
}
#[derive(Serialize, Deserialize)]
struct Entities {
    hashtags: Vec<Hashtag>,
}
#[derive(Serialize, Deserialize)]
struct User {
    screen_name: String,
    followers_count: i64,
}
#[derive(Serialize, Deserialize)]
struct Status {
    id_str: String,
    text: String,
    retweet_count: i64,
    favorite_count: i64,
    user: User,
    entities: Entities,
}
#[derive(Serialize, Deserialize)]
struct SearchMetadata {
    count: i64,
}
#[derive(Serialize, Deserialize)]
struct Twitter {
    statuses: Vec<Status>,
    search_metadata: SearchMetadata,
}

#[derive(Serialize, Deserialize)]
#[allow(non_snake_case)]
struct Event {
    id: i64,
    name: String,
    subTopicIds: Vec<i64>,
    topicIds: Vec<i64>,
}
#[derive(Serialize, Deserialize)]
#[allow(non_snake_case)]
struct Price {
    amount: i64,
    audienceSubCategoryId: i64,
    seatCategoryId: i64,
}
#[derive(Serialize, Deserialize)]
#[allow(non_snake_case)]
struct Area {
    areaId: i64,
    blockIds: Vec<i64>,
}
#[derive(Serialize, Deserialize)]
#[allow(non_snake_case)]
struct SeatCategory {
    areas: Vec<Area>,
    seatCategoryId: i64,
}
#[derive(Serialize, Deserialize)]
#[allow(non_snake_case)]
struct Performance {
    id: i64,
    eventId: i64,
    start: i64,
    prices: Vec<Price>,
    seatCategories: Vec<SeatCategory>,
}
#[derive(Serialize, Deserialize)]
#[allow(non_snake_case)]
struct Citm {
    areaNames: IndexMap<String, String>,
    events: IndexMap<String, Event>,
    performances: Vec<Performance>,
}

/// JavaScript's Math.round: halves round up.
fn js_round(x: f64) -> f64 {
    (x + 0.5).floor()
}

/// A JavaScript string's length (UTF-16 code units).
fn js_len(s: &str) -> usize {
    s.encode_utf16().count()
}

fn canada_sum(c: &Canada) -> String {
    let (mut rings, mut points, mut s) = (0u64, 0u64, 0f64);
    for f in &c.features {
        for ring in &f.geometry.coordinates {
            rings += 1;
            for p in ring {
                points += 1;
                s += js_round(p[0] * 100.0) + js_round(p[1] * 100.0);
            }
        }
    }
    // The sum is an integer well inside 2^53, printed as JavaScript prints it.
    format!("{} {rings} {points} {}", c.features.len(), s as i64)
}

fn twitter_sum(t: &Twitter) -> String {
    let (mut text, mut followers, mut tags, mut rt) = (0usize, 0i64, 0usize, 0i64);
    for st in &t.statuses {
        text += js_len(&st.text) + js_len(&st.id_str) + js_len(&st.user.screen_name);
        followers += st.user.followers_count;
        tags += st.entities.hashtags.len();
        rt += st.retweet_count + st.favorite_count;
    }
    format!("{} {text} {followers} {tags} {rt} {}", t.statuses.len(), t.search_metadata.count)
}

fn citm_sum(c: &Citm) -> String {
    let (mut prices, mut amount, mut areas) = (0usize, 0i64, 0usize);
    for p in &c.performances {
        prices += p.prices.len();
        amount += p.prices.iter().map(|x| x.amount).sum::<i64>();
        areas += p.seatCategories.iter().map(|s| s.areas.len()).sum::<usize>();
    }
    let topics: usize = c.events.values().map(|e| e.topicIds.len() + e.subTopicIds.len()).sum();
    format!(
        "{} {} {} {prices} {amount} {areas} {topics}",
        c.areaNames.len(),
        c.events.len(),
        c.performances.len()
    )
}

fn decode<T: DeserializeOwned>(lib: &str, text: &str) -> T {
    match lib {
        "serde" => serde_json::from_str(text).unwrap(),
        "sonic" => sonic_rs::from_str(text).unwrap(),
        _ => {
            let mut bytes = text.as_bytes().to_vec();
            simd_json::serde::from_slice(&mut bytes).unwrap()
        }
    }
}

fn encode<T: Serialize>(lib: &str, v: &T) -> String {
    match lib {
        "serde" => serde_json::to_string(v).unwrap(),
        "sonic" => sonic_rs::to_string(v).unwrap(),
        _ => simd_json::serde::to_string(v).unwrap(),
    }
}

fn run<T: DeserializeOwned + Serialize>(
    lib: &str,
    text: &str,
    rounds: usize,
    write: bool,
    sum: fn(&T) -> String,
) -> String {
    let mut out = String::new();
    if write {
        let v: T = decode(lib, text);
        for _ in 0..rounds {
            out = encode(lib, &v);
        }
    } else {
        for _ in 0..rounds {
            out = sum(&decode::<T>(lib, text));
        }
    }
    out
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (lib, corpus) = (a[1].as_str(), a[2].as_str());
    let text = std::fs::read_to_string(&a[3]).unwrap();
    let rounds: usize = a[4].parse().unwrap();
    let write = a[5] == "write";
    let out = match corpus {
        "canada" => run::<Canada>(lib, &text, rounds, write, canada_sum),
        "twitter" => run::<Twitter>(lib, &text, rounds, write, twitter_sum),
        _ => run::<Citm>(lib, &text, rounds, write, citm_sum),
    };
    println!("{out}");
}
