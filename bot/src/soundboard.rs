//! MyInstants soundboard integration.
//!
//! Fetches sound buttons from https://www.myinstants.com/ (via its search
//! page HTML), exposing a paginated, searchable soundboard in Discord. The
//! selected audio is downloaded, optionally run through an ffmpeg effect, and
//! played in the user's voice channel — similar to how `/speak` works, but
//! using the memes/sounds hosted on MyInstants instead of TTS.

use regex::Regex;
use std::time::Duration;

/// A single sound result from MyInstants.
#[derive(Clone, Debug)]
pub struct SoundItem {
    pub title: String,
    pub url: String,
}

/// A soundboard session: the search results plus pagination/effect state.
/// Stored in `Data` so component-button interactions can resolve it.
#[derive(Clone, Debug)]
pub struct SoundboardSession {
    pub query: String,
    pub items: Vec<SoundItem>,
    pub page: usize,
    pub effect: String,
    pub guild_id: u64,
    /// When the session was created, used to expire stale sessions so they
    /// don't accumulate in memory if the user never interacts again.
    pub created_at: std::time::Instant,
}

/// How long a soundboard session may live before it is considered stale and
/// eligible for eviction (in case the user never interacts with the buttons).
pub const SESSION_TTL: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// Maximum number of sessions kept in memory at once.
pub const MAX_SESSIONS: usize = 25;

/// Results shown per page in the Discord embed.
pub const PAGE_SIZE: usize = 5;

const BASE_URL: &str = "https://www.myinstants.com";

/// Search MyInstants for sounds matching `query`.
///
/// Fetches the search results page and parses each sound button (title + mp3
/// URL). Returns them in page order. On any network/parse failure returns an
/// error string for the user.
pub async fn search(query: &str) -> Result<Vec<SoundItem>, String> {
    let encoded = urlencoding::encode(query);
    let url = format!("{}/en/search/?name={}", BASE_URL, encoded);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {}", e))?;

    let html = client
        .get(&url)
        .header("User-Agent", "Mozilla/5.0 (discord-llm-bot soundboard)")
        .send()
        .await
        .map_err(|e| format!("Failed to reach MyInstants: {}", e))?
        .text()
        .await
        .map_err(|e| format!("Failed to read MyInstants response: {}", e))?;

    // Each sound appears as a <div class="instant"> block containing a
    // play('/media/sounds/xxx.mp3', ...) button and an instant-link title.
    let mp3_re = Regex::new(r#"onclick="play\('([^']+)'"#).unwrap();
    let title_re = Regex::new(r#"class="instant-link[^"]*">([^<]+)</a>"#).unwrap();

    let urls: Vec<String> = mp3_re
        .captures_iter(&html)
        .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
        .collect();
    let titles: Vec<String> = title_re
        .captures_iter(&html)
        .filter_map(|c| c.get(1).map(|m| decode_entities(m.as_str()).trim().to_string()))
        .collect();

    // The two lists should be 1:1 per sound block; guard against a mismatch
    // (e.g. partial parse) by truncating to the shorter.
    let mut items = Vec::new();
    for (url, title) in urls.into_iter().zip(titles) {
        if title.is_empty() {
            continue;
        }
        let full_url = if url.starts_with("http") {
            url
        } else {
            format!("{}{}", BASE_URL, url)
        };
        items.push(SoundItem { title, url: full_url });
    }

    if items.is_empty() {
        return Err("No sounds found for that search on MyInstants.".to_string());
    }
    Ok(items)
}

/// Decode the most common HTML entities found in MyInstants titles.
fn decode_entities(input: &str) -> String {
    input
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
}
