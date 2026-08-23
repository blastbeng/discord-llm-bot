use crate::database;
use crate::tts;
use sqlx::SqlitePool;
use std::path::Path;

pub async fn run_background_generator(pool: SqlitePool) {
    let voices = [
        "Google",
        "Goku (FakeYou.com)",
        "Gerry Scotti (FakeYou.com)",
        "Homer Simpson (FakeYou.com)",
        "Peter Griffin (FakeYou.com)",
        "Papa Francesco (FakeYou.com)",
        "Silvio Berlusconi (FakeYou.com)",
    ];

    log::info!("Background generator started");
    
    // Initialize database statistics tracking
    if let Ok(stats) = database::get_db_statistics(&pool).await {
        log::info!("Initial database state: {}", stats);
    }
    
    loop {
        log::info!("Background generator: starting new cycle");
        let mut generated_count = 0;
        let mut failed_count = 0;
        let mut fakeyou_count = 0;
        let max_fakeyou_per_cycle = 3; // Increased limit for better coverage
        
        if let Ok(sentences) = database::select_all_sentence(&pool).await {
            log::debug!("Background generator: processing {} sentences", sentences.len());
            
            'outer: for sentence in sentences {
                for voice in voices.iter() {
                    if generated_count >= 6 {
                        break 'outer;
                    }
                    
                    // Skip FakeYou if we've hit the limit for this cycle
                    if *voice != "Google" && fakeyou_count >= max_fakeyou_per_cycle {
                        log::debug!("Background generator: skipping FakeYou voice {} due to rate limit", voice);
                        continue;
                    }
                    
                    let file_path = tts::get_file_path(voice, &sentence);
                    if !Path::new(&file_path).exists() {
                        log::info!(
                            "Generating TTS for: '{}' with voice: '{}'",
                            truncate_string(&sentence, 50),
                            voice
                        );
                        
                        match tts::get_or_generate_tts(&sentence, voice).await {
                            Ok(result) => {
                                if result.fallback {
                                    log::info!(
                                        "✓ Fallback to Google for '{}': {}",
                                        truncate_string(&sentence, 30),
                                        voice
                                    );
                                } else {
                                    generated_count += 1;
                                }
                                
                                if *voice != "Google" && !result.fallback {
                                    fakeyou_count += 1;
                                }
                            }
                            Err(e) => {
                                log::warn!(
                                    "Background generator: Failed to generate TTS for voice '{}': {} - Sentence: {}",
                                    voice,
                                    e,
                                    truncate_string(&sentence, 40)
                                );
                                failed_count += 1;
                            }
                        }
                        
                        generated_count += 1;
                        // Sleep a bit to avoid rate limiting and reduce API load
                        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                    } else {
                        log::trace!("Background generator: Cache hit for {} with {}", voice, truncate_string(&sentence, 20));
                    }
                }
            }
        } else {
            log::error!("Background generator: Failed to fetch sentences from database");
        }
        
        // Log cycle statistics
        let status = if failed_count > 0 {
            format!(
                "Generated {} files ({} FakeYou), {} failures",
                generated_count, fakeyou_count, failed_count
            )
        } else {
            format!(
                "Generated {} files successfully ({} FakeYou)",
                generated_count, fakeyou_count
            )
        };
        
        log::info!("Background generator: cycle complete - {}", status);
        
        // Wait 5 minutes before checking the database again
        tokio::time::sleep(std::time::Duration::from_secs(300)).await;
    }
}

/// Truncate long strings for better logging readability
fn truncate_string(s: &str, max_length: usize) -> String {
    if s.len() <= max_length {
        s.to_string()
    } else {
        format!("{}...", &s[..max_length])
    }
}
