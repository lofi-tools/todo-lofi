use std::{path::PathBuf, process::Command, sync::LazyLock};
use storage::task::Task;
use toasty_cli::{Config, ToastyCli};

pub static REPO: LazyLock<PathBuf> = LazyLock::new(|| {
    let path_bytes = Command::new("git")
        .arg("rev-parse")
        .arg("--show-toplevel")
        .output()
        .unwrap()
        .stdout;
    let path_str = str::from_utf8(&path_bytes).unwrap().trim();
    PathBuf::from(path_str)
});

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let toasty_config = Config::load_from(&REPO.join("libs/storage/Toasty.toml"))?;
    let db_url = std::env::var("TURSO_DB_URL")
        .unwrap_or_else(|_| format!("turso:{}/.cache/todo.db", REPO.display()));
    let db = toasty::Db::builder()
        .models(toasty::models!(Task))
        .connect(&db_url)
        .await?;
    let cli = ToastyCli::with_config(db, toasty_config);
    cli.parse_and_run().await?;
    Ok(())
}
