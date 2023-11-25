use std::backtrace::Backtrace;

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

    let routes_string = response
        .text()
        .instrument(info_span!("Reading body of response"))
        .await?;

    let routes: Vec<HzppRoute> =
        serde_json::from_str(&routes_string).map_err(|e| GetRoutesError::ParsingError {
            source: e,
            backtrace: Backtrace::capture(),
            routes: routes_string,
        })?;

    info!("got {} routes", routes.len());

    Ok(routes)
}

#[derive(thiserror::Error, Debug)]
pub enum GetRoutesError {
    #[error("error fetching the routes \n{} \n{}", source, backtrace)]
    HttpRequestError {
        #[from]
        source: reqwest::Error,
        backtrace: Backtrace,
    },

    #[error("error parsing the routes \n{} \n{} \n {}", source, routes, backtrace)]
    ParsingError {
        source: serde_json::Error,
        backtrace: Backtrace,
        routes: String,
    },
}
