use sqlx::sqlite::SqlitePool;

pub struct Database {
    pub pool: SqlitePool,
}

impl Database {
    pub async fn new(db_url: &str) -> Result<Self, sqlx::Error> {
        let pool = SqlitePool::connect(db_url).await?;
        
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS sentences (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                sentence TEXT NOT NULL
            )"
        )
        .execute(&pool)
        .await?;

        Ok(Self { pool })
    }

    pub async fn insert_sentence(&self, sentence: &str) -> Result<(), sqlx::Error> {
        sqlx::query("INSERT OR IGNORE INTO sentences (sentence) VALUES (?)")
            .bind(sentence)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn select_like_sentence(&self, text: &str) -> Result<Vec<String>, sqlx::Error> {
        let like_text = format!("%{}%", text);
        let rows = sqlx::query_scalar::<_, String>(
            "SELECT sentence FROM sentences WHERE sentence LIKE ? ORDER BY RANDOM()"
        )
        .bind(like_text)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    pub async fn select_all_sentence(&self) -> Result<Vec<String>, sqlx::Error> {
        let rows = sqlx::query_scalar::<_, String>(
            "SELECT sentence FROM sentences ORDER BY RANDOM()"
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }
}
