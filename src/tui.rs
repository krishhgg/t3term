use std::sync::Arc;

use anyhow::Result;

use crate::client::Client;

pub async fn run(_client: Arc<Client>) -> Result<()> {
    anyhow::bail!("TUI not built yet")
}
