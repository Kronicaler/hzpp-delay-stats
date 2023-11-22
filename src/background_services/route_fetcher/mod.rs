use snafu::Snafu;
use tracing::{info, info_span, Instrument};

use crate::model::hzpp_api_model::HzppRoute;

#[tracing::instrument(err)]
pub async fn get_routes() -> Result<Vec<HzppRoute>, GetRoutesError> {
    let request = format!(
        "https://josipsalkovic.com/hzpp/planer/v3/getRoutes.php?date={}",
        chrono::Local::now().format("%Y%m%d")
    );
    let response = reqwest::get(&request)
        .instrument(info_span!("Fetching routes"))
        .await?
        .error_for_status()?;

    let routes: Vec<HzppRoute> = serde_json::from_str(
        &response
            .text()
            .instrument(info_span!("Reading body of response"))
            .await?,
    )?;

    info!("got {} routes", routes.len());

    Ok(routes)
}

// TODO: explore having this replaced with snafu
#[derive(Snafu, Debug)]
pub enum GetRoutesError {
    #[snafu(display("error fetching the routes {source}"), context(false))]
    HttpRequestError { source: reqwest::Error },

    #[snafu(display("error parsing the routes {source}"), context(false))]
    ParsingError { source: serde_json::Error },
}
