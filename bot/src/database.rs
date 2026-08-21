use sqlx::SqlitePool;

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
