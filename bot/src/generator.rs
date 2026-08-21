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

    loop {
        let mut generated_count = 0;
        if let Ok(sentences) = database::select_all_sentence(&pool).await {
            'outer: for sentence in sentences {
                for voice in voices.iter() {
                    if generated_count >= 3 {
                        break 'outer;
                    }
                    let file_path = tts::get_file_path(voice, &sentence);
                    if !Path::new(&file_path).exists() {
                        log::info!("Background generator: Generating TTS for: {} with voice: {}", sentence, voice);
                        
                        match tts::get_or_generate_tts(&sentence, voice).await {
                            Ok(result) => {
                                log::info!("Background generator: Generated TTS for: {} with voice: {} (fallback: {})", sentence, voice, result.fallback);
                            }
                            Err(e) => log::warn!("Background generator: Failed to get TTS for voice {}: {}", voice, e),
                        }
                        generated_count += 1;
                        // Sleep a bit to avoid rate limiting
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    }
                }
            }
        }
        // Wait 5 minutes before checking the database again
        tokio::time::sleep(std::time::Duration::from_secs(300)).await;
    }
}
