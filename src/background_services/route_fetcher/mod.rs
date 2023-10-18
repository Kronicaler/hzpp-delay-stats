use std::backtrace::Backtrace;

use thiserror::Error;
use tracing::{error, info, info_span, Instrument};

use crate::model::route::Route;

#[tracing::instrument(err(Debug))]
pub async fn get_routes() -> Result<Vec<Route>, GetRoutesError> {
    let request = format!(
        "https://josipsalkovic.com/hzpp/planer/v3/getRoutes.php?date={}",
        chrono::Local::now().format("%Y%m%d")
    );
    let response = reqwest::get(&request)
        .instrument(info_span!("Fetching routes"))
        .await?;

    if !response.status().is_success() {
        let err = Err(GetRoutesError::InvalidResponseError {
            body: response
                .text()
                .await
                .unwrap_or_else(|e| format!("Couldn't read body | {}", e)),
            backtrace: Backtrace::capture(),
        });
        error!("{:?}", err);
        return err;
    }

    let routes: Vec<Route> = serde_json::from_str(
        &response
            .text()
            .instrument(info_span!("Reading body of response"))
            .await?,
    )?;

    info!("got {} routes", routes.len());

    Ok(routes)
}

#[derive(Error, Debug)]
pub enum GetRoutesError {
    #[error("Error fetching data")]
    HttpRequestError(#[from] reqwest::Error),
    #[error("Invalid response")]
    InvalidResponseError {
        body: String,
        #[backtrace]
        backtrace: Backtrace,
    },
    #[error("Error parsing a file")]
    FileParsingError(#[from] serde_json::Error),
}
