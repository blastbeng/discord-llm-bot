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
