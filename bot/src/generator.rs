use crate::database;
use crate::tts;
use sqlx::SqlitePool;

pub async fn run_background_generator(pool: SqlitePool) {
    // The generator pre-generates TTS files to disk — it only makes sense
    // when SAVE_MP3_ON_DISK is enabled. When disabled, TTS is generated
    // on-demand and saved to temp files, so caching is pointless.
    let save_mp3 = std::env::var("SAVE_MP3_ON_DISK")
        .unwrap_or_else(|_| "false".to_string())
        .to_lowercase()
        == "true";

    if !save_mp3 {
        log::info!("Background generator disabled (SAVE_MP3_ON_DISK=false)");
        return;
    }

    let voices = tts::AVAILABLE_VOICES;

    log::info!("Background generator started");
    
    // Initialize database statistics tracking
    if let Ok(stats) = database::get_db_statistics(&pool).await {
        log::info!("Initial database state: {}", stats);
    }
    
    loop {
        log::info!("Background generator: starting new cycle");
        let mut generated_count = 0;
        let mut failed_count = 0;
        
        match database::select_sentences_for_generation(&pool).await {
            Ok(sentences) => {
                log::debug!("Background generator: processing {} sentences", sentences.len());
                
                'outer: for sentence in sentences {
                    for voice in voices.iter() {
                        if generated_count >= 6 {
                            break 'outer;
                        }
                        
                        let file_path = tts::get_file_path(voice, &sentence);
                        if !tokio::fs::try_exists(&file_path).await.unwrap_or(false) {
                            log::info!(
                                "Generating TTS for: '{}' with voice: '{}'",
                                truncate_string(&sentence, 50),
                                voice
                            );
                            
                            match tts::get_or_generate_tts_with_effect(&sentence, voice, "none").await {
                                Ok(_) => {
                                    generated_count += 1;
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

                            // Sleep a bit to avoid rate limiting and reduce API load
                            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                        } else {
                            log::trace!("Background generator: Cache hit for {} with {}", voice, truncate_string(&sentence, 20));
                        }
                    }
                }
            }
            Err(e) => {
                log::error!("Background generator: Failed to fetch sentences from database: {}", e);
            }
        }
        
        // Log cycle statistics
        let status = if failed_count > 0 {
            format!(
                "Generated {} files, {} failures",
                generated_count, failed_count
            )
        } else {
            format!(
                "Generated {} files successfully",
                generated_count
            )
        };
        
        log::info!("Background generator: cycle complete - {}", status);
        
        // Wait 5 minutes before checking the database again
        tokio::time::sleep(std::time::Duration::from_secs(300)).await;
    }
}

/// Truncate long strings for better logging readability.
/// Uses char-boundary-safe truncation to avoid panicking on multibyte
/// UTF-8 characters (e.g., Italian accented letters à, è, ì, ò, ù).
fn truncate_string(s: &str, max_length: usize) -> String {
    if s.len() <= max_length {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_length).collect();
        format!("{truncated}...")
    }
}
