use std::str::FromStr;
use std::time::Duration;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

#[derive(Debug, thiserror::Error)]
pub enum StartupError {
    #[error("cannot open the database: {0}")]
    Connect(#[from] sqlx::Error),
    #[error("cannot apply migrations: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
}

/// Opens the pool, creating the database file if it is not there yet, and
/// applies every migration embedded at compile time.
pub async fn connect(database_url: &str, max_connections: u32) -> Result<SqlitePool, StartupError> {
    let options = SqliteConnectOptions::from_str(database_url)?
        .create_if_missing(true)
        // WAL keeps readers from blocking the single writer; on an in-memory
        // database SQLite ignores it.
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_secs(5));

    let pool = SqlitePoolOptions::new()
        .max_connections(max_connections)
        .connect_with(options)
        .await?;

    sqlx::migrate!().run(&pool).await?;

    Ok(pool)
}
