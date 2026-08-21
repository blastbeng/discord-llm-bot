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
        if let Ok(sentences) = database::select_all_sentence(&pool).await {
            for sentence in sentences {
                for voice in voices.iter() {
                    let file_path = tts::get_file_path(voice, &sentence);
                    if !Path::new(&file_path).exists() {
                        log::info!("Background generator: Generating TTS for: {} with voice: {}", sentence, voice);
                        
                        let tts_result = if *voice == "Google" {
                            tts::get_tts_google(&sentence).await
                        } else {
                            tts::get_tts_fakeyou(&sentence, voice).await
                        };

                        match tts_result {
                            Ok(bytes) => {
                                if let Err(e) = tts::compress_and_save_mp3(bytes, &file_path).await {
                                    log::error!("Background generator: Failed to save mp3: {}", e);
                                }
                            }
                            Err(e) => log::warn!("Background generator: Failed to get TTS for voice {}: {}", voice, e),
                        }
                        // Sleep a bit to avoid rate limiting
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    }
                }
            }
        }
        // Wait before checking the database again
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    }
}
