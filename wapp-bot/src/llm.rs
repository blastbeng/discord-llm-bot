use std::sync::OnceLock;

static LLM_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

fn llm_client() -> &'static reqwest::Client {
    LLM_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
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
        Just give the spoken answer text."
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
    // Add the current question
    messages.push(serde_json::json!({"role": "user", "content": question}));

    let client = llm_client();
    let mut last_error = String::new();

    for (i, endpoint) in endpoints.iter().enumerate() {
        let url = format!("{}/chat/completions", endpoint.base_url.trim_end_matches('/'));

        let body = serde_json::json!({
            "model": endpoint.model,
            "messages": messages,
            "stream": false,
            "temperature": 0.7,
            // Generous token budget: reasoning models (e.g. gpt-oss:20b-cloud)
            // spend a lot on their internal chain-of-thought before the final
            // answer. A too-low max_tokens makes them exhaust the budget on
            // reasoning and return an empty "content" field. The response is
            // truncated to 200 chars for TTS afterwards anyway.
            "max_tokens": 500
        });

        log::info!(
            "llm::ask: trying endpoint {}/{} (model: {})",
            i + 1,
            endpoints.len(),
            endpoint.model
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
                    last_error = format!("Rate limited at {}", endpoint.base_url);
                    continue;
                }

                if !status.is_success() {
                    let body_text = r.text().await.unwrap_or_default();
                    log::warn!(
                        "llm::ask: endpoint {} returned status {}: {}",
                        endpoint.base_url, status, body_text
                    );
                    last_error = format!("HTTP {} at {}: {}", status, endpoint.base_url, body_text);
                    continue;
                }

                let json: serde_json::Value = match r.json().await {
                    Ok(j) => j,
                    Err(e) => {
                        log::warn!(
                            "llm::ask: failed to parse JSON from {}: {}",
                            endpoint.base_url, e
                        );
                        last_error = format!("JSON parse error at {}: {}", endpoint.base_url, e);
                        continue;
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
                    log::warn!(
                        "llm::ask: endpoint {} returned empty content",
                        endpoint.base_url
                    );
                    last_error = format!("Empty response at {}", endpoint.base_url);
                    continue;
                }

                // Like the old Python bot, take only the first line and
                // strip surrounding quotes. This keeps TTS short and clean.
                let first_line = content.lines().next().unwrap_or(content);
                let cleaned = first_line
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'')
                    .to_string();

                if cleaned.is_empty() {
                    last_error = format!("Empty response after cleanup at {}", endpoint.base_url);
                    continue;
                }

                log::info!(
                    "llm::ask: success from endpoint {} (model: {}, response length: {})",
                    endpoint.base_url,
                    endpoint.model,
                    cleaned.len()
                );

                // Truncate to 200 characters at a UTF-8 char boundary
                // (Google TTS limit). This matches the /speak command limit.
                let truncated = if cleaned.chars().count() > 200 {
                    let s: String = cleaned.chars().take(200).collect();
                    format!("{}...", s)
                } else {
                    cleaned
                };

                return Ok(truncated);
            }
            Err(e) => {
                log::warn!(
                    "llm::ask: endpoint {} connection failed: {}, trying next",
                    endpoint.base_url, e
                );
                last_error = format!("Connection error at {}: {}", endpoint.base_url, e);
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
            "max_tokens": 400
        });

        log::info!(
            "llm::translate: trying endpoint {}/{} (model: {})",
            i + 1,
            endpoints.len(),
            endpoint.model
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
                    last_error = format!("Rate limited at {}", endpoint.base_url);
                    continue;
                }

                if !status.is_success() {
                    let body_text = r.text().await.unwrap_or_default();
                    log::warn!("llm::translate: endpoint {} returned {}: {}", endpoint.base_url, status, body_text);
                    last_error = format!("HTTP {} at {}", status, endpoint.base_url);
                    continue;
                }

                let json: serde_json::Value = match r.json().await {
                    Ok(j) => j,
                    Err(e) => {
                        last_error = format!("JSON parse error at {}: {}", endpoint.base_url, e);
                        continue;
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
                    last_error = format!("Empty response at {}", endpoint.base_url);
                    continue;
                }

                let cleaned = content.trim().trim_matches('"').trim_matches('\'').trim().to_string();

                if cleaned.is_empty() {
                    last_error = format!("Empty response after cleanup at {}", endpoint.base_url);
                    continue;
                }

                log::info!("llm::translate: success from {} (length: {})", endpoint.base_url, cleaned.len());

                let truncated = if cleaned.chars().count() > 200 {
                    let s: String = cleaned.chars().take(200).collect();
                    format!("{}...", s)
                } else {
                    cleaned
                };

                return Ok(truncated);
            }
            Err(e) => {
                log::warn!("llm::translate: endpoint {} failed: {}, trying next", endpoint.base_url, e);
                last_error = format!("Connection error at {}: {}", endpoint.base_url, e);
                continue;
            }
        }
    }

    Err(format!("All LLM endpoints failed. Last error: {}", last_error))
}