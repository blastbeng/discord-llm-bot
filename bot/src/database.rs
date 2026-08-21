use sqlx::SqlitePool;
use std::path::Path;

pub async fn init_db(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS sentences (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            sentence TEXT NOT NULL UNIQUE
        )"
    )
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn insert_sentence(pool: &SqlitePool, sentence: &str) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT OR IGNORE INTO sentences (sentence) VALUES (?)")
        .bind(sentence)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn select_all_sentence(pool: &SqlitePool) -> Result<Vec<String>, sqlx::Error> {
    let rows = sqlx::query_scalar::<_, String>("SELECT sentence FROM sentences ORDER BY RANDOM()")
        .fetch_all(pool)
        .await?;
    Ok(rows)
}

pub async fn select_like_sentence(pool: &SqlitePool, text: &str) -> Result<Vec<String>, sqlx::Error> {
    let pattern = format!("%{}%", text);
    let rows = sqlx::query_scalar::<_, String>("SELECT sentence FROM sentences WHERE sentence LIKE ? ORDER BY RANDOM()")
        .bind(pattern)
        .fetch_all(pool)
        .await?;
    Ok(rows)
}

pub async fn populate_db_if_empty(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sentences")
        .fetch_one(pool)
        .await?;

    if count == 0 {
        if Path::new("config/sentences.txt").exists() {
            log::info!("Populating database from config/sentences.txt...");
            let contents = std::fs::read_to_string("config/sentences.txt").unwrap_or_default();
            for line in contents.lines() {
                let sentence = line.trim();
                if !sentence.is_empty() {
                    insert_sentence(pool, sentence).await?;
                }
            }
            log::info!("Database populated.");
        }
    }
    Ok(())
}
