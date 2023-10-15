#![feature(error_generic_member_access)]

use anyhow::Result;
use dotenvy::dotenv;
use itertools::Itertools;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry::KeyValue;
use opentelemetry_otlp::{ExportConfig, TonicConfig};
use opentelemetry_sdk::trace::{Config, TracerProvider};
use opentelemetry_sdk::Resource;
use regex::Regex;
use std::num::ParseIntError;
use std::{fs, thread, time::Duration};
use thiserror::Error;
use tracing::{error, info, instrument, span, warn, Level};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Registry};

use crate::background_services::route_fetcher::get_routes;
use crate::model::route::Route;

mod background_services;
mod model;

#[tokio::main]
async fn main() -> Result<()> {
    _ = dotenv();

    let provider = TracerProvider::builder()
        .with_simple_exporter(
            opentelemetry_otlp::SpanExporter::new_tonic(
                ExportConfig::default(),
                TonicConfig::default(),
            )
            .unwrap(),
        )
        .with_config(
            Config::default().with_resource(Resource::new(vec![KeyValue::new(
                "service.name",
                "HZPP_delays",
            )])),
        )
        .build();

    let tracer = provider.tracer("HZPP_delays");

    let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);

    let env_filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();

    let _subscriber = Registry::default()
        .with(telemetry)
        .with(env_filter)
        .set_default();

    _ = get_delay();
    get_routes().await.unwrap();

    tokio::time::sleep(Duration::from_millis(250)).await;

    return Ok(());
}

#[instrument(err)]
fn get_delay() -> Result<(), HzppError> {
    {
        let span = span!(Level::TRACE, "getting routes");
        let _enter = span.enter();

        let routes_file = "documentation/example_responses/routes.json";
        let routes: Vec<Route> =
            serde_json::from_str(&fs::read_to_string(routes_file).map_err(|e| {
                HzppError::FileReadingError {
                    source: e,
                    file: routes_file.to_string(),
                }
            })?)?;

        info!("Got {} routes", routes.len());

        thread::sleep(Duration::from_millis(25));
    }
    {
        let span = span!(Level::TRACE, "extracting delay from html");
        let _enter = span.enter();

        let delay_file = "example_responses/delay.html";
        let delay_html =
            fs::read_to_string(delay_file).map_err(|e| HzppError::FileReadingError {
                source: e,
                file: delay_file.to_string(),
            })?;

        if delay_html.contains("Vlak je redovit") {
            info!("The train is running on time");
        } else {
            let late_regex = Regex::new("Kasni . min").unwrap();

            let minutes_late: i32 = late_regex
                .captures(&delay_html)
                .ok_or_else(|| HzppError::CouldntFindLateness(delay_html.clone()))?[0]
                .to_owned()
                .trim()
                .split(" ")
                .collect_vec()[1]
                .to_owned()
                .parse()?;

            thread::sleep(Duration::from_millis(25));
            info!("The train is {} minutes late", minutes_late);
        }
    }

    sending_delays_to_db();

    Ok(())
}

#[instrument]
fn sending_delays_to_db() {
    thread::sleep(Duration::from_millis(25));
}

#[derive(Error, Debug)]
pub enum HzppError {
    #[error("Couldn't find how late the train was in the returned html file: {0}")]
    CouldntFindLateness(String),
    #[error("Error reading the file {file} | {source}")]
    FileReadingError {
        #[backtrace]
        source: std::io::Error,
        file: String,
    },
    #[error("Error parsing a file")]
    FileParsingError(#[from] serde_json::Error),
    #[error("Error parsing the delay duration")]
    DelayParsingError(#[from] ParseIntError),
}
