use std::sync::{Arc, OnceLock};

static LLM_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

/// A reqwest DNS resolver that resolves hostnames to IPv4 addresses only.
/// See src/tts.rs `Ipv4OnlyResolver` for the rationale: Docker bridge
/// containers have no IPv6 connectivity, but DNS still returns IPv6 addresses
/// (e.g. for Cloudflare-fronted hosts), causing unreliable connections.
#[derive(Clone)]
struct Ipv4OnlyResolver;

impl reqwest::dns::Resolve for Ipv4OnlyResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        Box::pin(async move {
            let host = name.as_str().to_string();
            let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .collect();
            let ipv4: Vec<std::net::SocketAddr> =
                addrs.iter().filter(|a| a.is_ipv4()).copied().collect();
            let chosen: Vec<std::net::SocketAddr> = if ipv4.is_empty() { addrs } else { ipv4 };
            // Coerce into the `Box<dyn Iterator<Item = SocketAddr> + Send>`
            // trait object reqwest expects (a concrete Box<IntoIter> won't
            // unify with it).
            let iter: Box<dyn Iterator<Item = std::net::SocketAddr> + Send> = Box::new(chosen.into_iter());
            Ok(iter)
        })
    }
}

fn llm_client() -> &'static reqwest::Client {
    LLM_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .dns_resolver(Arc::new(Ipv4OnlyResolver))
            .build()
            .expect("Failed to build LLM HTTP client")
    })
}

/// A single LLM provider endpoint configuration.
struct LlmEndpoint {
    base_url: String,
    api_key: String,
    model: String,
}

/// Check if any LLM endpoints are configured.
pub fn is_configured() -> bool {
    !get_endpoint_configs().is_empty()
}

/// Parse endpoints with proper indexing for api_keys.
fn get_endpoint_configs() -> Vec<LlmEndpoint> {
    let endpoints: Vec<String> = std::env::var("LLM_ENDPOINTS")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let api_keys: Vec<String> = std::env::var("LLM_API_KEYS")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        // Filter empty entries so an unset LLM_API_KEYS results in an empty
        // vec and the per-endpoint "ollama" default below is used (instead of
        // the empty string, which would send an empty Authorization header).
        .filter(|s| !s.is_empty())
        .collect();

    let models: Vec<String> = std::env::var("LLM_MODELS")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if endpoints.is_empty() || models.is_empty() {
        return Vec::new();
    }

    let n = endpoints.len().min(models.len());
    (0..n)
        .map(|i| LlmEndpoint {
            base_url: endpoints[i].clone(),
            api_key: api_keys.get(i).cloned().unwrap_or_else(|| "ollama".to_string()),
            model: models[i].clone(),
        })
        .collect()
}

// ── Structured JSON protocol for phrase generation ────────────────────────
//
// The phrase-generation features (ask / welcome / goodbye / here_i_am /
// eavesdrop_response) sometimes get a "refusal" from the LLM instead of the
// requested phrase (e.g. "I'm sorry, I can't do that" / "Mi dispiace, non
// posso accontentare questa richiesta"). Speaking that boilerplate through
// TTS sounds broken — and persisting it into the shared sentence database
// would later be played aloud by other bots via /random. To catch refusals
// reliably, the system prompts mandate a JSON reply
// `{"text": ..., "refused": true|false}`; the helpers below parse that
// protocol, detect refusal boilerplate in models that ignore it, and
// classify each completion so callers can stay silent on refusals.

/// Marker prefix for refusal errors returned by the phrase-generation
/// functions. Callers can distinguish "the LLM refused" from infrastructure
/// failures via [`is_refusal_error`].
pub const REFUSAL_PREFIX: &str = "REFUSED:";

/// Whether an error string returned by an LLM function is a refusal.
pub fn is_refusal_error(err: &str) -> bool {
    err.starts_with(REFUSAL_PREFIX)
}

/// JSON response-format instruction appended to the system prompts (English).
const JSON_FORMAT_EN: &str = "RESPONSE FORMAT (mandatory): your ENTIRE reply must be one single JSON object and nothing else — no markdown fences, no explanations: {\"text\": \"<the single sentence to say out loud>\", \"refused\": <true|false>}. If you refuse to fulfil the request for ANY reason (safety, policy, or anything else), set \"refused\": true; in that case \"text\" may be empty. If you comply, set \"refused\": false and put ONLY the single spoken sentence inside \"text\".";

/// JSON response-format instruction appended to the system prompts (Italian).
const JSON_FORMAT_IT: &str = "FORMATO RISPOSTA (obbligatorio): l'INTERA risposta deve essere un solo oggetto JSON e nient'altro — niente markdown, niente spiegazioni: {\"text\": \"<la singola frase da dire ad alta voce>\", \"refused\": <true|false>}. Se rifiuti di eseguire la richiesta per QUALSIASI motivo (sicurezza, policy o altro), imposta \"refused\": true; in tal caso \"text\" può essere vuoto. Se esegui la richiesta, imposta \"refused\": false e metti SOLO la singola frase da pronunciare dentro \"text\".";

/// Pick the JSON response-format instruction matching the configured language.
fn json_format_for(lang: &str) -> &'static str {
    if lang.starts_with("eng") {
        JSON_FORMAT_EN
    } else {
        JSON_FORMAT_IT
    }
}

/// A structured LLM phrase response parsed from the JSON protocol.
struct StructuredPhrase {
    text: String,
    refused: bool,
}

/// Parse the `{"text": ..., "refused": ...}` JSON object out of a raw LLM
/// completion. Tolerant by design: models often wrap the object in markdown
/// fences or leading prose, so the first `{` through the last `}` is treated
/// as the object. Returns `None` when the output is not usable JSON — callers
/// then fall back to plain-text handling.
fn parse_structured_phrase(raw: &str) -> Option<StructuredPhrase> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end < start {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(&raw[start..=end]).ok()?;

    // "refused" is a boolean in the protocol, but some models emit "true" as
    // a string — accept both spellings.
    let refused = match value.get("refused") {
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::String(s)) => {
            matches!(s.trim().to_lowercase().as_str(), "true" | "yes" | "1")
        }
        _ => false,
    };

    // Accept a couple of common key aliases so slightly-off models still work.
    let text = ["text", "content", "response", "phrase"]
        .iter()
        .find_map(|k| value.get(*k).and_then(|v| v.as_str()))
        .unwrap_or("")
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim()
        .to_string();

    if text.is_empty() && !refused {
        // Neither a usable text nor a refusal flag — not a structured response.
        return None;
    }

    Some(StructuredPhrase { text, refused })
}

/// Detect refusal / policy-dodge boilerplate in an LLM response (e.g.
/// "I'm sorry, I can't do that" / "Mi dispiace, non posso accontentare questa
/// richiesta"). Safety net for models that ignore the JSON protocol, and a
/// second check on the parsed text in case the model claims `refused: false`
/// but answers with refusal wording anyway.
///
/// Generic tokens ("sorry" / "non posso" alone) are deliberately avoided:
/// the bot's roast personality easily produces legitimate sentences with them.
pub fn looks_like_refusal(text: &str) -> bool {
    let lower = text.to_lowercase();

    // Unambiguous refusal phrases — safe as standalone matches.
    const STRONG: &[&str] = &[
        // English
        "cannot comply", "can't comply", "cannot generate", "can't generate",
        "cannot fulfill", "cannot fulfil", "can't fulfill", "can't fulfil",
        "cannot do that", "can't do that", "cannot help with", "can't help with",
        "cannot provide", "can't provide", "cannot assist", "can't assist",
        "unable to comply", "unable to fulfill", "unable to fulfil",
        "unable to help", "unable to assist",
        "must decline", "have to decline", "will not comply", "won't comply",
        "as an ai", "i'm an ai", "i am an ai", "as a language model",
        "against my guidelines", "against my policy", "against my policies",
        "against the guidelines", "against the policy", "content policy",
        "not appropriate", "inappropriate content",
        "i'm not comfortable", "i am not comfortable",
        // Italian
        "non posso accontentare", "non posso aiutarti", "non posso aiutare",
        "non posso rispondere", "non posso esaudire", "non posso soddisfare",
        "non posso complire", "non posso fare questo", "non posso farlo",
        "non riesco a soddisfare", "non riesco ad aiutarti", "non riesco a complire",
        "non mi è permesso", "non mi é permesso", "non mi e permesso",
        "non mi è concesso", "non mi é concesso",
        "non sono autorizzato", "non sono autorizzata", "non sono in grado",
        "contro le mie linee guida", "linee guida", "politiche di contenuto",
        "contenuto inappropriato", "non è appropriato", "non é appropriato",
        "come modello linguistico", "in quanto modello", "in quanto ia",
    ];
    if STRONG.iter().any(|p| lower.contains(p)) {
        return true;
    }

    // Apology + first-person inability co-occurring covers remaining wordings
    // ("Sorry, I really can't say that", "Mi dispiace, ma non è possibile").
    const APOLOGIES: &[&str] = &[
        "i'm sorry", "i am sorry", "sorry but", "sorry,",
        "mi dispiace", "mi spiace", "mi rammarico",
    ];
    const INABILITIES: &[&str] = &[
        "i can't", "i cannot", "i can not", "i won't", "i will not",
        "i'm unable", "i am unable",
        "non posso", "non riesco", "non è possibile", "non é possibile",
    ];

    APOLOGIES.iter().any(|a| lower.contains(a))
        && INABILITIES.iter().any(|i| lower.contains(i))
}

/// Classification of a raw LLM completion for the phrase features.
enum PhraseOutcome {
    /// A validated, cleaned phrase ready for TTS (already truncated to 200 chars).
    Ok(String),
    /// The LLM refused (JSON flag or refusal boilerplate) — do not speak it.
    Refused,
    /// Garbage/invalid output — worth retrying.
    Invalid,
}

/// Validate that an LLM-generated phrase is actually a spoken sentence and
/// not a system artifact (e.g. "user safety: safe", metadata, classification
/// labels, JSON, or empty content after stripping reasoning markers). Also
/// verifies that the user's name appears in the response when one is expected.
fn validate_phrase(raw: &str, expected_user: &str, min_len: usize) -> Option<String> {
    let trimmed = raw.trim().trim_matches('"').trim_matches('\'').trim();

    if trimmed.is_empty() {
        return None;
    }

    // Take only the first line — reasoning models sometimes prepend
    // multi-line reasoning before the actual answer.
    let first_line = trimmed.lines().next().unwrap_or(trimmed);
    let cleaned = first_line.trim().trim_matches('"').trim_matches('\'').trim().to_string();

    if cleaned.is_empty() {
        return None;
    }

    let lower = cleaned.to_lowercase();

    // Reject system/metadata patterns that reasoning models leak.
    const GARBAGE_PATTERNS: &[&str] = &[
        "user safety",
        "safe",
        "unsafe",
        "safety:",
        "content policy",
        "policy:",
        "flag:",
        "category:",
        "rating:",
        "sentiment:",
        "label:",
        "classification:",
        "moderation:",
        "toxicity:",
        "output:",
        "response:",
        "result:",
        "answer:",
        "greeting:",
        "welcome:",
        "goodbye:",
    ];

    // If the cleaned phrase is EXACTLY one of the garbage patterns (e.g.
    // the model returned just "safe" or "user safety: safe"), reject it.
    for pattern in GARBAGE_PATTERNS {
        if lower == *pattern {
            log::warn!("llm::validate_phrase: rejected garbage response '{}'", cleaned);
            return None;
        }
        // Also reject "pattern: value" style responses (e.g. "user safety: safe")
        if lower.starts_with(&format!("{}:", pattern)) || lower.starts_with(&format!("{} :", pattern)) {
            // But allow if the rest after the colon looks like a real sentence
            // (longer than 15 chars and doesn't look like a label)
            let after_colon = lower.split(':').nth(1).unwrap_or("").trim();
            if after_colon.len() < 15 || GARBAGE_PATTERNS.contains(&after_colon) {
                log::warn!("llm::validate_phrase: rejected garbage response '{}'", cleaned);
                return None;
            }
        }
    }

    // Reject responses that are too short (likely just a label)
    if cleaned.chars().count() < min_len {
        log::warn!("llm::validate_phrase: rejected too-short response '{}'", cleaned);
        return None;
    }

    // Reject responses that look like JSON or key-value pairs
    if cleaned.starts_with('{') || cleaned.starts_with('[') || cleaned.contains(":\")") {
        log::warn!("llm::validate_phrase: rejected JSON-like response '{}'", cleaned);
        return None;
    }

    // Verify the user's name appears in the response (case-insensitive).
    // The LLM is explicitly instructed to include it, so a missing name
    // means the response is off-topic or garbage.
    let user_lower = expected_user.to_lowercase();
    if !lower.contains(&user_lower) && !expected_user.is_empty() {
        log::warn!(
            "llm::validate_phrase: rejected response '{}' (missing username '{}')",
            cleaned, expected_user
        );
        return None;
    }

    Some(cleaned)
}

/// Process a raw LLM completion for the phrase features: prefer the JSON
/// protocol (`{"text", "refused"}`), fall back to plain-text handling when the
/// model ignores it, and short-circuit refusals either way. `min_len` is the
/// minimum accepted phrase length (1 for /ask answers, 5 for personality
/// phrases).
fn process_phrase_response(raw: &str, expected_user: &str, min_len: usize) -> PhraseOutcome {
    // 1) Structured JSON path (the format the system prompts mandate).
    if let Some(sp) = parse_structured_phrase(raw) {
        if sp.refused || looks_like_refusal(&sp.text) {
            log::warn!(
                "llm::process_phrase_response: refusal signalled (refused flag: {}, preview: {:?})",
                sp.refused,
                sp.text.chars().take(120).collect::<String>()
            );
            return PhraseOutcome::Refused;
        }
        return match validate_phrase(&sp.text, expected_user, min_len) {
            Some(cleaned) => PhraseOutcome::Ok(truncate_for_tts(cleaned)),
            None => PhraseOutcome::Invalid,
        };
    }

    // 2) Plain-text fallback: the model ignored JSON mode.
    if looks_like_refusal(raw) {
        log::warn!(
            "llm::process_phrase_response: refusal boilerplate in plain response (preview: {:?})",
            raw.chars().take(120).collect::<String>()
        );
        return PhraseOutcome::Refused;
    }
    match validate_phrase(raw, expected_user, min_len) {
        Some(cleaned) => PhraseOutcome::Ok(truncate_for_tts(cleaned)),
        None => PhraseOutcome::Invalid,
    }
}

/// Truncate to 200 characters at a UTF-8 char boundary (Google TTS limit),
/// matching the /speak command limit.
fn truncate_for_tts(s: String) -> String {
    if s.chars().count() > 200 {
        let truncated: String = s.chars().take(200).collect();
        format!("{truncated}...")
    } else {
        s
    }
}

/// Generate a response from the LLM using the user's question and
/// database sentences as context. Rotates through configured endpoints
/// on rate-limit (429) or connection errors, trying the next provider.
///
/// The system prompt instructs the LLM to act as a Discord voice bot
/// that speaks in the style of the sentences stored in the database,
/// and to keep responses short (one line, max 200 chars) since they
/// will be converted to speech via TTS.
/// A conversation message for the LLM (role + content).
#[derive(Clone)]
pub struct ConversationMessage {
    pub role: String,
    pub content: String,
}

pub async fn ask(
    question: &str,
    db_sentences: &[String],
    bot_nickname: &str,
    history: &[ConversationMessage],
) -> Result<String, String> {
    let endpoints = get_endpoint_configs();
    if endpoints.is_empty() {
        return Err("No LLM endpoints configured".to_string());
    }

    // Web search: give the LLM fresh web context for the question (MCP
    // gateway first, then SearXNG direct, then DuckDuckGo). Failures are
    // non-fatal — the answer just loses the web context.
    let web_context = match web_search(question).await {
        Some(ctx) => {
            log::info!("llm::ask: web context acquired ({} chars)", ctx.len());
            format!(
                "\n\nWeb search results for the question (may contain the answer):\n{ctx}\nUse these results if they are relevant; ignore them if not."
            )
        }
        None => {
            log::info!("llm::ask: no web context available");
            String::new()
        }
    };

    // Build the system prompt with database context.
    // Limit to 30 sentences to keep the prompt size reasonable.
    let sentences_sample: Vec<&str> = db_sentences.iter().take(30).map(|s| s.as_str()).collect();
    let sentences_text = sentences_sample
        .iter()
        .map(|s| format!("- {}", s))
        .collect::<Vec<_>>()
        .join("\n");

    let lang = std::env::var("LANG").unwrap_or_else(|_| "ita".to_string());
    let (lang_instruction, personality) = match lang.as_str() {
        "eng" => (
            "Respond in English.",
            "You are a humorous Discord voice bot. Keep your answer to a single short sentence (max 200 characters). Be funny and casual.",
        ),
        _ => (
            "Rispondi in italiano.",
            "Sei un bot vocale Discord umoristico. Rispondi con una singola frase breve (massimo 200 caratteri). Sii divertente e alla mano.",
        ),
    };

    let system_prompt = format!(
        "{personality}\n\
        You are known as \"{bot_nickname}\".\n\
        {lang_instruction}\n\
        Here are some example phrases that reflect your personality and style:\n\
        {sentences_text}\n\
        Answer the user's question concisely. Do not use markdown, emojis, or multi-line responses. \
        Just give the spoken answer text.\n\
        {json_format}",
        json_format = json_format_for(&lang),
    );

    // Build the messages array: system prompt, conversation history, then
    // the current question. The history allows the LLM to have context of
    // previous questions and answers in this guild.
    let mut messages: Vec<serde_json::Value> = vec![
        serde_json::json!({"role": "system", "content": system_prompt})
    ];
    // Add conversation history (last 10 messages from this guild)
    // to keep the prompt size manageable.
    let history_len = history.len();
    let start = if history_len > 10 { history_len - 10 } else { 0 };
    for msg in &history[start..] {
        messages.push(serde_json::json!({"role": msg.role, "content": msg.content}));
    }
    // Add the current question, enriched with the web-search context.
    let enriched_question = format!("{question}{web_context}");
    messages.push(serde_json::json!({"role": "user", "content": enriched_question}));

    let client = llm_client();
    let mut last_error = String::new();

    for (i, endpoint) in endpoints.iter().enumerate() {
        let url = format!("{}/chat/completions", endpoint.base_url.trim_end_matches('/'));

        let body = serde_json::json!({
            "model": endpoint.model,
            "messages": messages,
            "stream": false,
            "temperature": 0.7,
            "response_format": {"type": "json_object"},
            // Generous token budget: reasoning models (e.g. gpt-oss:20b-cloud)
            // spend a lot on their internal chain-of-thought before the final
            // answer. A too-low max_tokens makes them exhaust the budget on
            // reasoning and return an empty "content" field. The response is
            // truncated to 200 chars for TTS afterwards anyway.
            "max_tokens": 500
        });

        // Reasoning models (e.g. gpt-oss:20b-cloud) intermittently return an HTTP
        // 200 with empty content because their chain-of-thought exhausts the token
        // budget. Retry the same endpoint a couple of times before moving on.
        let mut empty_retries: u32 = 0;
        const MAX_EMPTY_RETRIES: u32 = 2;

        let attempt = loop {
            log::info!(
                "llm::ask: trying endpoint {}/{} (model: {}, url: {})",
                i + 1,
                endpoints.len(),
                endpoint.model,
                url
            );

            let resp = client
                .post(&url)
                .header("Content-Type", "application/json")
                .header("Authorization", format!("Bearer {}", endpoint.api_key))
                .json(&body)
                .send()
                .await;

            match resp {
                Ok(r) => {
                    let status = r.status();

                    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                        log::warn!(
                            "llm::ask: endpoint {} returned 429 (rate limited), trying next",
                            endpoint.base_url
                        );
                        break Err(format!("Rate limited at {}", endpoint.base_url));
                    }

                    if !status.is_success() {
                        let body_text = r.text().await.unwrap_or_default();
                        log::warn!(
                            "llm::ask: endpoint {} returned status {}: {}",
                            endpoint.base_url, status, body_text
                        );
                        break Err(format!("HTTP {} at {}: {}", status, endpoint.base_url, body_text));
                    }

                    let json: serde_json::Value = match r.json().await {
                        Ok(j) => j,
                        Err(e) => {
                            log::warn!(
                                "llm::ask: failed to parse JSON from {}: {}",
                                endpoint.base_url, e
                            );
                            break Err(format!("JSON parse error at {}: {}", endpoint.base_url, e));
                        }
                    };

                    // Extract the response text from OpenAI-compatible format:
                    // choices[0].message.content
                    let content = json
                        .get("choices")
                        .and_then(|c| c.get(0))
                        .and_then(|c| c.get("message"))
                        .and_then(|m| m.get("content"))
                        .and_then(|c| c.as_str())
                        .unwrap_or("");

                    if content.is_empty() {
                        // Log the raw response body — for reasoning models the
                        // "reasoning" field often explains why content is empty
                        // (e.g. token budget exhausted on chain-of-thought).
                        log::warn!(
                            "llm::ask: endpoint {} returned empty content, raw body: {}",
                            endpoint.base_url,
                            json
                        );
                        empty_retries += 1;
                        if empty_retries <= MAX_EMPTY_RETRIES {
                            log::warn!(
                                "llm::ask: endpoint {} returned empty content, retrying (attempt {}/{})",
                                endpoint.base_url, empty_retries, MAX_EMPTY_RETRIES
                            );
                            continue;
                        }
                        log::warn!(
                            "llm::ask: endpoint {} returned empty content after {} retries, trying next",
                            endpoint.base_url, MAX_EMPTY_RETRIES
                        );
                        break Err(format!("Empty response at {}", endpoint.base_url));
                    }

                    match process_phrase_response(content, "", 1) {
                        PhraseOutcome::Ok(cleaned) => {
                            log::info!(
                                "llm::ask: success from endpoint {} (model: {}, response length: {})",
                                endpoint.base_url,
                                endpoint.model,
                                cleaned.len()
                            );
                            break Ok(cleaned);
                        }
                        PhraseOutcome::Refused => {
                            // The LLM refused — terminal, never speak it and
                            // never persist it (it would poison the shared
                            // sentence database and resurface via /random).
                            log::warn!(
                                "llm::ask: endpoint {} refused the request, skipping ask",
                                endpoint.base_url
                            );
                            break Ok(REFUSAL_PREFIX.to_string() + "ask");
                        }
                        PhraseOutcome::Invalid => {
                            empty_retries += 1;
                            if empty_retries <= MAX_EMPTY_RETRIES {
                                log::warn!(
                                    "llm::ask: endpoint {} returned garbage/invalid response, retrying (attempt {}/{})",
                                    endpoint.base_url, empty_retries, MAX_EMPTY_RETRIES
                                );
                                continue;
                            }
                            break Err(format!("Invalid/garbage response at {}", endpoint.base_url));
                        }
                    }
                }
                Err(e) => {
                    log::warn!(
                        "llm::ask: endpoint {} connection failed (URL: {}), error: {}, trying next",
                        endpoint.base_url, url, e
                    );
                    break Err(format!("Connection error at {}: {}", endpoint.base_url, e));
                }
            }
        };

        match attempt {
            Ok(response) => return Ok(response),
            Err(e) => {
                last_error = e;
                continue;
            }
        }
    }

    Err(format!("All LLM endpoints failed. Last error: {}", last_error))
}

/// Translate text to the target language using the LLM.
/// Uses the same endpoint rotation as `ask`. The system prompt
/// instructs the LLM to translate without adding commentary.
pub async fn translate(text: &str, target_lang: &str) -> Result<String, String> {
    let endpoints = get_endpoint_configs();
    if endpoints.is_empty() {
        return Err("No LLM endpoints configured".to_string());
    }

    let lang = std::env::var("LANG").unwrap_or_else(|_| "ita".to_string());
    let bot_lang = match lang.as_str() {
        "eng" => "English",
        _ => "Italian",
    };

    let system_prompt = format!(
        "You are a translation bot. Translate the user's text to {target_lang}. \
        Output ONLY the translation — no explanations, no quotes, no extra text. \
        Keep the same tone and register. If the text is already in {target_lang}, \
        output it unchanged. The bot's interface language is {bot_lang}."
    );

    let client = llm_client();
    let mut last_error = String::new();

    for (i, endpoint) in endpoints.iter().enumerate() {
        let url = format!("{}/chat/completions", endpoint.base_url.trim_end_matches('/'));

        let body = serde_json::json!({
            "model": endpoint.model,
            "messages": [
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": text}
            ],
            "stream": false,
            "temperature": 0.3,
            // Generous budget for reasoning models (see ask for details).
            "max_tokens": 500
        });

        // Reasoning models intermittently return empty content — retry the same
        // endpoint a couple of times before moving on (see ask for details).
        let mut empty_retries: u32 = 0;
        const MAX_EMPTY_RETRIES: u32 = 2;

        let attempt = loop {
            log::info!(
                "llm::translate: trying endpoint {}/{} (model: {}, url: {})",
                i + 1,
                endpoints.len(),
                endpoint.model,
                url
            );

            let resp = client
                .post(&url)
                .header("Content-Type", "application/json")
                .header("Authorization", format!("Bearer {}", endpoint.api_key))
                .json(&body)
                .send()
                .await;

            match resp {
                Ok(r) => {
                    let status = r.status();

                    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                        log::warn!("llm::translate: endpoint {} returned 429, trying next", endpoint.base_url);
                        break Err(format!("Rate limited at {}", endpoint.base_url));
                    }

                    if !status.is_success() {
                        let body_text = r.text().await.unwrap_or_default();
                        log::warn!("llm::translate: endpoint {} returned {}: {}", endpoint.base_url, status, body_text);
                        break Err(format!("HTTP {} at {}", status, endpoint.base_url));
                    }

                    let json: serde_json::Value = match r.json().await {
                        Ok(j) => j,
                        Err(e) => {
                            break Err(format!("JSON parse error at {}: {}", endpoint.base_url, e));
                        }
                    };

                    let content = json
                        .get("choices")
                        .and_then(|c| c.get(0))
                        .and_then(|c| c.get("message"))
                        .and_then(|m| m.get("content"))
                        .and_then(|c| c.as_str())
                        .unwrap_or("");

                    if content.is_empty() {
                        // Log the raw response body — for reasoning models the
                        // "reasoning" field often explains why content is empty.
                        log::warn!(
                            "llm::translate: endpoint {} returned empty content, raw body: {}",
                            endpoint.base_url,
                            json
                        );
                        empty_retries += 1;
                        if empty_retries <= MAX_EMPTY_RETRIES {
                            log::warn!(
                                "llm::translate: endpoint {} returned empty content, retrying (attempt {}/{})",
                                endpoint.base_url, empty_retries, MAX_EMPTY_RETRIES
                            );
                            continue;
                        }
                        break Err(format!("Empty response at {}", endpoint.base_url));
                    }

                    let cleaned = content.trim().trim_matches('"').trim_matches('\'').trim().to_string();

                    if cleaned.is_empty() {
                        break Err(format!("Empty response after cleanup at {}", endpoint.base_url));
                    }

                    log::info!("llm::translate: success from {} (length: {})", endpoint.base_url, cleaned.len());

                    let truncated = if cleaned.chars().count() > 200 {
                        let s: String = cleaned.chars().take(200).collect();
                        format!("{}...", s)
                    } else {
                        cleaned
                    };

                    break Ok(truncated);
                }
                Err(e) => {
                    log::warn!(
                        "llm::translate: endpoint {} connection failed (URL: {}), error: {}, trying next",
                        endpoint.base_url, url, e
                    );
                    break Err(format!("Connection error at {}: {}", endpoint.base_url, e));
                }
            }
        };

        match attempt {
            Ok(response) => return Ok(response),
            Err(e) => {
                last_error = e;
                continue;
            }
        }
    }

    Err(format!("All LLM endpoints failed. Last error: {}", last_error))
}

// ─── Web search (MCP gateway + fallbacks) ─────────────────────────

/// Search the web for recent information relevant to `question`.
///
/// Primary path: the local Docker MCP gateway (`MCP_GATEWAY_URL` +
/// `MCP_GATEWAY_TOKEN`), calling the `searxng_web_search` tool over the
/// MCP streamable-HTTP transport. Fallbacks, in order: the SearXNG
/// instance directly (HTML endpoint, `SEARXNG_URL`), then DuckDuckGo's
/// HTML endpoint. Returns a compact multi-line digest of title+snippet
/// pairs, or None when every path fails (the caller then just answers
/// without web context).
pub async fn web_search(question: &str) -> Option<String> {
    if let Some(r) = mcp_web_search(question).await {
        log::info!("llm::web_search: mcp gateway ok ({} chars)", r.len());
        return Some(r);
    }
    log::warn!("llm::web_search: mcp gateway unavailable, trying fallbacks");
    if let Some(r) = searxng_direct_search(question).await {
        log::info!("llm::web_search: searxng direct ok");
        return Some(r);
    }
    if let Some(r) = duckduckgo_search(question).await {
        log::info!("llm::web_search: duckduckgo fallback ok");
        return Some(r);
    }
    log::warn!("llm::web_search: all search paths failed");
    None
}

/// Call the MCP gateway's searxng_web_search tool. The gateway speaks the
/// MCP streamable-HTTP transport: initialize -> initialized -> tools/call,
/// keeping the Mcp-Session-Id header across calls.
async fn mcp_web_search(question: &str) -> Option<String> {
    let url = std::env::var("MCP_GATEWAY_URL").ok()?;
    let token = std::env::var("MCP_GATEWAY_TOKEN").ok()?;
    if url.is_empty() || token.is_empty() {
        return None;
    }
    let client = llm_client();

    // 1) initialize — capture the session id.
    let init_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "discord-llm-bot", "version": "1.0"}
        }
    });
    let resp = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("Authorization", format!("Bearer {token}"))
        .json(&init_body)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .ok()?;
    let session = resp
        .headers()
        .get("Mcp-Session-Id")
        .and_then(|v| v.to_str().ok())?
        .to_string();
    if !resp.status().is_success() {
        log::warn!("llm::mcp_web_search: initialize failed ({})", resp.status());
        return None;
    }
    let _ = resp.text().await;

    // 2) initialized notification (no response body expected).
    let _ = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("Authorization", format!("Bearer {token}"))
        .header("Mcp-Session-Id", &session)
        .json(&serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await;

    // 3) tools/call searxng_web_search.
    let call_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "searxng_web_search",
            "arguments": {"query": question, "num_results": 5}
        }
    });
    let resp = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("Authorization", format!("Bearer {token}"))
        .header("Mcp-Session-Id", &session)
        .json(&call_body)
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        log::warn!("llm::mcp_web_search: tools/call failed ({})", resp.status());
        return None;
    }
    let body = resp.text().await.ok()?;
    // The gateway answers with SSE frames: "event: message\ndata: {...}".
    let json_line = body
        .lines()
        .find(|l| l.starts_with("data: "))?
        .strip_prefix("data: ")?;
    let v: serde_json::Value = serde_json::from_str(json_line_trim(json_line)).ok()?;
    let text = v
        .get("result")?
        .get("content")?
        .get(0)?
        .get("text")?
        .as_str()?
        .to_string();
    if text.is_empty() {
        return None;
    }
    // Compact the digest: keep only Title/Description lines, cap length.
    let mut digest = String::new();
    for line in text.lines() {
        if line.starts_with("Title:") || line.starts_with("Description:") {
            digest.push_str(line.trim());
            digest.push('\n');
        }
        if digest.len() > 1500 {
            break;
        }
    }
    if digest.is_empty() {
        Some(text.chars().take(1500).collect())
    } else {
        Some(digest)
    }
}

fn json_line_trim(s: &str) -> &str {
    s.trim()
}

/// Fallback 1: query the SearXNG instance directly (JSON format endpoint).
async fn searxng_direct_search(question: &str) -> Option<String> {
    let base = std::env::var("SEARXNG_URL").unwrap_or_else(|_| "http://searxng:8080".to_string());
    let client = llm_client();
    let resp = client
        .get(format!("{base}/search"))
        .query(&[("q", question), ("format", "json"), ("language", "it")])
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let v: serde_json::Value = resp.json().await.ok()?;
    let results = v.get("results")?.as_array()?;
    let mut digest = String::new();
    for r in results.iter().take(5) {
        let title = r.get("title").and_then(|t| t.as_str()).unwrap_or("");
        let content = r.get("content").and_then(|c| c.as_str()).unwrap_or("");
        if !title.is_empty() {
            digest.push_str(&format!("Title: {title}\nDescription: {content}\n"));
        }
        if digest.len() > 1500 {
            break;
        }
    }
    if digest.is_empty() { None } else { Some(digest) }
}

/// Fallback 2: DuckDuckGo HTML endpoint, results scraped from the lite page.
async fn duckduckgo_search(question: &str) -> Option<String> {
    let client = llm_client();
    let resp = client
        .post("https://html.duckduckgo.com/html/")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!("q={}", urlencode(question)))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let html = resp.text().await.ok()?;
    // Extract result titles+snippets: <a class="result__a" ...>Title</a>
    // and <a class="result__snippet" ...>snippet</a>.
    let mut digest = String::new();
    let mut chars = html.char_indices().peekable();
    let _ = &mut chars;
    for part in html.split("result__a") {
        if digest.len() > 1500 {
            break;
        }
        if let Some(t0) = part.find('>') {
            if let Some(t1) = part[t0 + 1..].find('<') {
                let title = &part[t0 + 1..t0 + 1 + t1];
                if !title.is_empty() && digest.len() + title.len() < 1500 {
                    digest.push_str(&format!("Title: {}\n", strip_tags(title)));
                }
            }
        }
        if let Some(s0) = part.find("result__snippet") {
            if let Some(gt) = part[s0..].find('>') {
                if let Some(lt) = part[s0 + gt + 1..].find('<') {
                    let snip = &part[s0 + gt + 1..s0 + gt + 1 + lt];
                    digest.push_str(&format!("Description: {}\n", strip_tags(snip)));
                }
            }
        }
    }
    if digest.is_empty() { None } else { Some(digest) }
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}
