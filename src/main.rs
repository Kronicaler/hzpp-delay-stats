use anyhow::Result;
use dotenvy::dotenv;
use itertools::Itertools;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry::KeyValue;
use opentelemetry_otlp::{ExportConfig, TonicConfig};
use opentelemetry_sdk::trace::{Config, TracerProvider};
use opentelemetry_sdk::Resource;
use regex::Regex;
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

    get_routes().await.unwrap();

    tokio::time::sleep(Duration::from_millis(250)).await;

    return Ok(());
}

#[instrument]
fn get_delay() -> Result<()> {
    {
        let span = span!(Level::TRACE, "getting routes");
        let _enter = span.enter();

        let routes: Vec<Route> =
            serde_json::from_str(&fs::read_to_string("example_responses/routes.json")?)?;

        info!("Got {} routes", routes.len());

        thread::sleep(Duration::from_millis(25));
    }
    {
        let span = span!(Level::TRACE, "extracting delay from html");
        let _enter = span.enter();

        let delay_html = fs::read_to_string("example_responses/delay.html")?;

        if delay_html.contains("Vlak je redovit") {
            info!("The train is running on time");
        } else {
            let late_regex = Regex::new("Kasni . min")?;

            let minutes_late: i32 = late_regex
                .captures(&delay_html)
                .ok_or(HzppError::CouldntFindLateness)?[0]
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
    #[error("Couldn't find how late the train was in the returned html file")]
    CouldntFindLateness,
}
