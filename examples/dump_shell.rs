use std::sync::Arc;
use t3term::{
    auth::Scope,
    client::{Client, WatchEvent},
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let client = Arc::new(Client::connect(&[Scope::Read], "5m").await?);
    let mut events = client.watch_shell(None);
    for _ in 0..3 {
        if let Some(WatchEvent::Item(item)) = events.recv().await {
            let summary: Vec<String> = item
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| match v {
                    serde_json::Value::Object(o) => format!(
                        "{k}: {{{}}}",
                        o.iter()
                            .map(|(k, v)| format!(
                                "{k}:{}",
                                v.as_array().map_or("-".into(), |a| a.len().to_string())
                            ))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    other => format!(
                        "{k}: {}",
                        other.to_string().chars().take(60).collect::<String>()
                    ),
                })
                .collect();
            println!("{}", summary.join(" | "));
        }
    }
    Ok(())
}
