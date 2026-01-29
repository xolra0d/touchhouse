use std::sync::Arc;
use touchhouse::{
    accept_conn, build_logger, init_conn_semaphore, init_listener, load_existing_parts,
    log_startup_info, spawn_background_merges,
};

#[tokio::main]
async fn main() -> Result<(), String> {
    build_logger();
    load_existing_parts()?;
    spawn_background_merges();
    let conn_semaphore = init_conn_semaphore();
    let listener = init_listener().await?;
    log_startup_info();
    loop {
        accept_conn(Arc::clone(&conn_semaphore), &listener).await?;
    }
}
