use crate::database;
use crate::tts;
use sqlx::SqlitePool;
use std::path::Path;

pub async fn run_background_generator(pool: SqlitePool) {
    loop {
        if let Ok(sentences) = database::select_all_sentence(&pool).await {
            for sentence in sentences {
                let file_path = tts::get_file_path("Google", &sentence);
                if !Path::new(&file_path).exists() {
                    log::info!("Background generator: Generating TTS for: {}", sentence);
                    match tts::get_tts_google(&sentence).await {
                        Ok(bytes) => {
                            if let Err(e) = tts::compress_and_save_mp3(bytes, &file_path).await {
                                log::error!("Background generator: Failed to save mp3: {}", e);
                            }
                        }
                        Err(e) => log::error!("Background generator: Failed to get TTS: {}", e),
                    }
                    // Sleep a bit to avoid rate limiting
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
        }
        // Wait before checking the database again
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    }
}
