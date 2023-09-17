use anyhow::Result;
use background_services::route_fetcher::get_routes;
use itertools::Itertools;
use opentelemetry::KeyValue;
use opentelemetry_sdk::metrics::{MeterProvider, PeriodicReader};
use opentelemetry_sdk::Resource;
use opentelemetry_stdout::MetricsExporter;
use regex::Regex;
use std::{fs, thread, time::Duration};
use thiserror::Error;
use tracing::{error, info, instrument, span, warn, Level};
use tracing_opentelemetry::MetricsLayer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::model::route::Route;

mod model;
mod background_services;

fn init_meter_provider() -> MeterProvider {
    let exporter = MetricsExporter::default();
    let reader = PeriodicReader::builder(exporter, opentelemetry_sdk::runtime::Tokio).build();
    MeterProvider::builder()
        .with_reader(reader)
        .with_resource(Resource::new(vec![KeyValue::new(
            "service.name",
            "metrics-basic-example",
        )]))
        .build()
}

#[tokio::main]
async fn main() -> Result<()> {
    let tracer = opentelemetry_jaeger::new_agent_pipeline()
        .with_service_name("HZPP_delays")
        .install_simple()?;
    let opentelemetry = tracing_opentelemetry::layer().with_tracer(tracer);

    let opentelemetry_metrics = MetricsLayer::new(init_meter_provider());

    tracing_subscriber::registry()
        .with(opentelemetry)
        .with(opentelemetry_metrics)
        .try_init()?;

    {
        let span = span!(Level::TRACE, "Root");
        let _enter = span.enter();

        get_routes().await?;
    }
    thread::sleep(Duration::from_millis(25));

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
