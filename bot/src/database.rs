use sqlx::SqlitePool;
use std::path::Path;

pub async fn init_db(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    log::debug!("init_db: creating tables if not exists");
    
    // Create sentences table with additional metadata columns
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS sentences (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            sentence TEXT NOT NULL UNIQUE,
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
            usage_count INTEGER DEFAULT 0,
            last_used_at TIMESTAMP
        )"
    )
    .execute(pool)
    .await?;

    // Create index for better query performance
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_sentence ON sentences(sentence)"
    )
    .execute(pool)
    .await?;

    log::debug!("init_db: database schema initialized successfully");
    Ok(())
}

pub async fn insert_sentence(pool: &SqlitePool, sentence: &str) -> Result<(), sqlx::Error> {
    log::debug!("insert_sentence: inserting '{}'", sentence);
    
    // Insert or update sentence with usage counter increment
    let result = sqlx::query(
        "INSERT INTO sentences (sentence, created_at, usage_count) 
         VALUES (?, CURRENT_TIMESTAMP, 1)
         ON CONFLICT(sentence) DO UPDATE SET 
            usage_count = usage_count + 1,
            last_used_at = CURRENT_TIMESTAMP"
    )
    .bind(sentence)
    .execute(pool)
    .await?;

    let rows_affected = result.rows_affected();
    log::debug!("insert_sentence: {} rows affected for sentence", rows_affected);
    Ok(())
}

pub async fn select_all_sentence(pool: &SqlitePool) -> Result<Vec<String>, sqlx::Error> {
    log::debug!("select_all_sentence: fetching all sentences");
    
    // Weighted random selection: bias toward less-used and older sentences.
    // RANDOM() returns a float in [0,1); multiplying by usage_count+1 gives
    // higher values to more-used sentences, so ascending sort picks the
    // least-used first. Adding created_at weight (older = smaller) further
    // breaks ties toward older entries. This ensures variety instead of
    // always repeating the same popular sentences.
    let rows = sqlx::query_scalar::<_, String>(
        "SELECT sentence FROM sentences 
         ORDER BY (RANDOM() * (usage_count + 1)) ASC, created_at ASC"
    )
    .fetch_all(pool)
    .await?;

    log::debug!("select_all_sentence: retrieved {} sentences", rows.len());
    Ok(rows)
}

/// Fetch sentences ordered by least-used first, for the background generator.
/// Unlike select_all_sentence which uses weighted-random ordering for variety
/// in user-facing commands, this deterministic ordering ensures the generator
/// processes the least-cached sentences first and eventually covers all entries.
pub async fn select_sentences_for_generation(pool: &SqlitePool) -> Result<Vec<String>, sqlx::Error> {
    log::debug!("select_sentences_for_generation: fetching sentences ordered by usage_count ASC");
    let rows = sqlx::query_scalar::<_, String>(
        "SELECT sentence FROM sentences 
         ORDER BY usage_count ASC, created_at ASC"
    )
    .fetch_all(pool)
    .await?;

    log::debug!("select_sentences_for_generation: retrieved {} sentences", rows.len());
    Ok(rows)
}

pub async fn select_like_sentence(pool: &SqlitePool, text: &str) -> Result<Vec<String>, sqlx::Error> {
    log::debug!("select_like_sentence: searching for pattern '%{}%'", text);
    
    // Search sentences with case-insensitive LIKE query and ordering by relevance.
    // The CASE differentiates exact matches (rank 0) from partial matches (rank 1),
    // so sentences equal to the search text appear first, followed by those that
    // merely contain it as a substring.
    let pattern = format!("%{}%", text);
    let rows = sqlx::query_scalar::<_, String>(
        "SELECT sentence FROM sentences 
         WHERE sentence LIKE ? COLLATE NOCASE
         ORDER BY 
            CASE 
                WHEN sentence = ? COLLATE NOCASE THEN 0 
                ELSE 1 
            END,
            usage_count DESC, created_at ASC"
    )
    .bind(&pattern)
    .bind(text)
    .fetch_all(pool)
    .await?;

    log::debug!("select_like_sentence: found {} matching sentences", rows.len());
    
    if rows.is_empty() {
        log::warn!("select_like_sentence: no sentences found matching pattern '{}'", text);
    }
    
    Ok(rows)
}

pub async fn populate_db_if_empty(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sentences")
        .fetch_one(pool)
        .await?;

    log::debug!("populate_db_if_empty: current sentence count = {}", count);
    
    if count == 0 {
        // Database is empty, populate from file
        populate_from_file(pool).await?;
    } else {
        // Check if there are new entries in the file that need to be added
        log::info!("Database already has {} sentences. Checking for updates...", count);
        update_existing_database(pool).await?;
    }

    Ok(())
}

/// Populate database from config/sentences.txt file
async fn populate_from_file(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    if !Path::new("config/sentences.txt").exists() {
        log::warn!("populate_db_if_empty: no sentences.txt file found for population");
        return Ok(());
    }

    log::info!("Populating database from config/sentences.txt...");
    let contents = std::fs::read_to_string("config/sentences.txt").unwrap_or_default();
    
    if contents.is_empty() {
        log::warn!("populate_db_if_empty: sentences.txt file is empty");
        return Ok(());
    }

    let mut inserted_count = 0;
    for line in contents.lines() {
        let sentence = line.trim();
        if !sentence.is_empty() {
            insert_sentence(pool, sentence).await?;
            inserted_count += 1;
        }
    }

    log::info!("Database populated successfully: {} sentences added", inserted_count);
    Ok(())
}

/// Update existing database with new or modified sentences from file
async fn update_existing_database(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let contents = std::fs::read_to_string("config/sentences.txt").unwrap_or_default();
    
    if contents.is_empty() {
        log::debug!("update_existing_database: no sentences file content available");
        return Ok(());
    }

    // Get current database sentences for comparison (unsorted — no need for
    // the weighted-random ordering that select_all_sentence applies).
    let existing_sentences: Vec<String> = sqlx::query_scalar(
        "SELECT sentence FROM sentences ORDER BY sentence"
    )
    .fetch_all(pool)
    .await?;
    let existing_set: std::collections::HashSet<String> = existing_sentences.iter().cloned().collect();

    log::info!("update_existing_database: {} sentences in database", existing_sentences.len());

    // Insert any new sentences from file
    let mut updated_count = 0;
    for line in contents.lines() {
        let sentence = line.trim().to_string();
        if !sentence.is_empty() && !existing_set.contains(&sentence) {
            insert_sentence(pool, &sentence).await?;
            updated_count += 1;
        }
    }

    log::info!("update_existing_database: {} new sentences added", updated_count);
    
    // Log database statistics
    let stats = get_db_statistics(pool).await?;
    log::info!("Database statistics: {}", stats);

    Ok(())
}

/// Get comprehensive database statistics
pub async fn get_db_statistics(pool: &SqlitePool) -> Result<String, sqlx::Error> {
    // Fetch various metrics from the database
    let total_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sentences").fetch_one(pool).await?;
    
    let min_usage: i64 = sqlx::query_scalar(
        "SELECT MIN(usage_count) FROM sentences"
    ).fetch_one(pool).await?;

    let max_usage: i64 = sqlx::query_scalar(
        "SELECT MAX(usage_count) FROM sentences"
    ).fetch_one(pool).await?;

    let avg_usage: f64 = sqlx::query_scalar::<_, f64>(
        "SELECT AVG(usage_count) FROM sentences"
    ).fetch_one(pool).await?;

    Ok(format!(
        "Total: {} | Min Usage: {} | Max Usage: {} | Avg Usage: {:.2}",
        total_count, min_usage, max_usage, avg_usage
    ))
}
