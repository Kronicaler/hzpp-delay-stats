use std::backtrace::Backtrace;

use tracing::{error, info, info_span, Instrument};

use crate::model::route::Route;

#[tracing::instrument(err)]
pub async fn get_routes() -> Result<Vec<Route>, GetRoutesError> {
    let request = format!(
        "https://josipsalkovic.com/hzpp/planer/v3/getRoutes.php?date={}",
        chrono::Local::now().format("%Y%m%d")
    );
    let response = reqwest::get(&request)
        .instrument(info_span!("Fetching routes"))
        .await?
        .error_for_status()?;

    let routes: Vec<Route> = serde_json::from_str(
        &response
            .text()
            .instrument(info_span!("Reading body of response"))
            .await?,
    )?;

    info!("got {} routes", routes.len());

    Ok(routes)
}

#[derive(thiserror::Error, Debug)]
pub enum GetRoutesError {
    #[error("Error fetching the routes \n {}", backtrace)]
    HttpRequestError {
        #[from]
        source: reqwest::Error,
        backtrace: Backtrace,
    },

    #[error("Error parsing the routes \n {}", backtrace)]
    FileParsingError {
        #[from]
        source: serde_json::Error,
        backtrace: Backtrace,
    },
}
