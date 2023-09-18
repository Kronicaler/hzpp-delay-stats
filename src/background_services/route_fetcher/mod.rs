use anyhow::{anyhow, Result};
use opentelemetry::global::{shutdown_tracer_provider, shutdown_logger_provider};
use tracing::info;

use crate::model::route::Route;

#[tracing::instrument(level = "info")]
pub async fn get_routes() -> Result<()> {
    info!("fetching routes");

    let request = format!(
        "https://josipsalkovic.com/hzpp/planer/v3/getRoutes.php?date={}",
        chrono::Local::now().format("%Y%m%d")
    );
    let response = reqwest::get(&request).await?;

    if !response.status().is_success() {
        return Err(anyhow!(
            "Error when fetching routes\nrequest:{}\nresponse:{:?}",
            request,
            response
        ));
    }

    let routes: Vec<Route> = serde_json::from_str(&response.text().await?)?;

    info!("got {} routes", routes.len());

    Ok(())
}
