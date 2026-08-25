use sqlx::SqlitePool;

pub async fn init_db(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    log::debug!("init_db: creating tables if not exists");
    
    // Create sentences table with additional metadata columns.
    // If the table already exists from the old Python bot (which only had
    // id + sentence), CREATE TABLE IF NOT EXISTS is a no-op and we rely on
    // the ALTER TABLE migration below to add the missing columns.
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

    // Migrate legacy Python bot tables that only had id + sentence columns.
    // SQLite doesn't support ADD COLUMN IF NOT EXISTS, so we check
    // PRAGMA table_info and add columns conditionally.
    let columns: Vec<(String, String)> = sqlx::query_as(
        "SELECT name, type FROM pragma_table_info('sentences')"
    )
    .fetch_all(pool)
    .await?;
    
    let existing: std::collections::HashSet<String> = columns.iter().map(|(n, _)| n.to_lowercase()).collect();
    
    if !existing.contains("created_at") {
        log::info!("init_db: migrating legacy table — adding created_at column");
        // SQLite does not allow non-constant defaults like CURRENT_TIMESTAMP
        // in ALTER TABLE ADD COLUMN. Add the column without a default, then
        // backfill existing rows so they have a valid timestamp.
        sqlx::query("ALTER TABLE sentences ADD COLUMN created_at TIMESTAMP")
            .execute(pool).await?;
        sqlx::query("UPDATE sentences SET created_at = CURRENT_TIMESTAMP WHERE created_at IS NULL")
            .execute(pool).await?;
    }
    if !existing.contains("usage_count") {
        log::info!("init_db: migrating legacy table — adding usage_count column");
        sqlx::query("ALTER TABLE sentences ADD COLUMN usage_count INTEGER DEFAULT 0")
            .execute(pool).await?;
    }
    if !existing.contains("last_used_at") {
        log::info!("init_db: migrating legacy table — adding last_used_at column");
        sqlx::query("ALTER TABLE sentences ADD COLUMN last_used_at TIMESTAMP")
            .execute(pool).await?;
    }

    // The legacy Python DB has an index on sentence but no UNIQUE constraint.
    // If a previous run of this migration was interrupted (e.g. the process
    // was killed between "RENAME TO sentences_old" and "DROP TABLE
    // sentences_old"), a leftover sentences_old table can remain with rows
    // stranded in it. Recover by merging any such rows back into sentences
    // (INSERT OR IGNORE dedupes against rows already present) and dropping
    // the leftover table. This also makes the migration below safe to run
    // again without tripping "table sentences_old already exists".
    let old_exists: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='sentences_old'"
    )
    .fetch_one(pool)
    .await?;
    if old_exists > 0 {
        log::info!("init_db: found leftover sentences_old from an interrupted migration — recovering");
        sqlx::query(
            "INSERT OR IGNORE INTO sentences (id, sentence, created_at, usage_count, last_used_at)
             SELECT id, sentence, created_at, usage_count, last_used_at FROM sentences_old"
        )
        .execute(pool)
        .await?;
        sqlx::query("DROP TABLE sentences_old")
            .execute(pool)
            .await?;
        log::info!("init_db: recovered leftover sentences_old and dropped it");
    }

    // Our insert_sentence uses ON CONFLICT(sentence), which requires a UNIQUE
    // constraint or PRIMARY KEY on that column. SQLite doesn't support
    // ALTER TABLE ADD CONSTRAINT, so we must recreate the table.
    //
    // Steps:
    //   1. Check if a UNIQUE constraint exists on the sentence column.
    //   2. If not, deduplicate existing rows (keep lowest id per sentence).
    //   3. Rename the old table, create a new one with the UNIQUE constraint,
    //      copy data over, and drop the old table.
    let has_unique_on_sentence: bool = {
        // Detect a UNIQUE constraint on the table via PRAGMA index_list.
        // A UNIQUE column/table constraint produces an auto-index with
        // origin='u' and "unique"=1 (e.g. sqlite_autoindex_sentences_1).
        // Parsing the CREATE TABLE sql text is fragile (e.g. the modern
        // schema "sentence TEXT NOT NULL UNIQUE" does not contain the exact
        // substring "sentence unique"), so we rely on the index metadata
        // instead. The legacy Python bot's schema has no such auto-index
        // (only a plain, non-unique index on sentence).
        let unique_auto_index: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_index_list('sentences')
             WHERE \"unique\" = 1 AND origin = 'u'"
        )
        .fetch_one(pool)
        .await?;
        unique_auto_index > 0
    };

    if !has_unique_on_sentence {
        log::info!("init_db: migrating legacy table — adding UNIQUE constraint on sentence column");

        // Step 1: Remove duplicate sentences, keeping the one with the lowest id
        let result = sqlx::query(
            "DELETE FROM sentences WHERE id NOT IN (
                SELECT MIN(id) FROM sentences GROUP BY sentence
            )"
        )
        .execute(pool)
        .await?;
        let dupes_deleted: i64 = result.rows_affected() as i64;
        log::info!("init_db: removed {} duplicate sentences", dupes_deleted);

        // Step 2: Recreate the table with the UNIQUE constraint.
        // Rename old table, create new one, copy data, drop old.
        sqlx::query("ALTER TABLE sentences RENAME TO sentences_old")
            .execute(pool)
            .await?;

        sqlx::query(
            "CREATE TABLE sentences (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                sentence TEXT NOT NULL UNIQUE,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                usage_count INTEGER DEFAULT 0,
                last_used_at TIMESTAMP
            )"
        )
        .execute(pool)
        .await?;

        sqlx::query(
            "INSERT INTO sentences (id, sentence, created_at, usage_count, last_used_at)
             SELECT id, sentence, created_at, usage_count, last_used_at
             FROM sentences_old"
        )
        .execute(pool)
        .await?;

        sqlx::query("DROP TABLE sentences_old")
            .execute(pool)
            .await?;

        log::info!("init_db: UNIQUE constraint on sentence column added successfully");
    }

    // Always deduplicate on startup, regardless of whether the UNIQUE
    // constraint migration above ran. This catches duplicates that may have
    // been introduced by external tools, manual edits, or edge cases in the
    // migration logic. Keeps the row with the lowest id (oldest entry) and
    // removes all others with the same sentence text.
    let dupe_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sentences WHERE id NOT IN (
            SELECT MIN(id) FROM sentences GROUP BY sentence
        )"
    )
    .fetch_one(pool)
    .await?;

    if dupe_count > 0 {
        log::info!("init_db: found {} duplicate sentences, removing them", dupe_count);
        let result = sqlx::query(
            "DELETE FROM sentences WHERE id NOT IN (
                SELECT MIN(id) FROM sentences GROUP BY sentence
            )"
        )
        .execute(pool)
        .await?;
        log::info!("init_db: removed {} duplicate sentences", result.rows_affected());
    } else {
        log::debug!("init_db: no duplicate sentences found");
    }

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
    
    // Return all sentences in a randomized order. Callers pick a random entry
    // (e.g. via `slice::choose`), so the order here only needs to avoid any
    // bias — a plain random sort is what the old comment's "weighted random"
    // actually produced in practice. SQLite's RANDOM() returns a large signed
    // integer, not a float in [0,1), so weighting it against usage_count was
    // never meaningful; a uniform random order is simpler and honest.
    let rows = sqlx::query_scalar::<_, String>(
        "SELECT sentence FROM sentences ORDER BY RANDOM()"
    )
    .fetch_all(pool)
    .await?;

    log::debug!("select_all_sentence: retrieved {} sentences", rows.len());
    Ok(rows)
}

/// Fetch sentences ordered by least-used first, for the background generator.
/// Unlike select_all_sentence which randomizes the full set for user-facing
/// commands, this deterministic ordering ensures the generator processes the
/// least-cached sentences first and eventually covers all entries.
#[allow(dead_code)] // Used by the Discord bot's background generator, not by the wapp-bot
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
    // usage_count ASC (least-used first) prefers sentences that haven't been
    // spoken as often, improving variety. (This is a real ordering, unlike
    // select_all_sentence which just randomizes the full set.)
    let pattern = format!("%{}%", text);
    let rows = sqlx::query_scalar::<_, String>(
        "SELECT sentence FROM sentences 
         WHERE sentence LIKE ? COLLATE NOCASE
         ORDER BY 
            CASE 
                WHEN sentence = ? COLLATE NOCASE THEN 0 
                ELSE 1 
            END,
            usage_count ASC, created_at ASC"
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
    if !tokio::fs::try_exists("config/sentences.txt").await.unwrap_or(false) {
        log::warn!("populate_db_if_empty: no sentences.txt file found for population");
        return Ok(());
    }

    log::info!("Populating database from config/sentences.txt...");
    let contents = tokio::fs::read_to_string("config/sentences.txt").await.unwrap_or_default();
    
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
    let contents = tokio::fs::read_to_string("config/sentences.txt").await.unwrap_or_default();
    
    if contents.is_empty() {
        log::debug!("update_existing_database: no sentences file content available");
        return Ok(());
    }

    // Get current database sentences for comparison, deterministically sorted.
    // No randomization needed here — we just need a stable set to diff against.
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
