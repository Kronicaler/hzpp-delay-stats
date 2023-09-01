use anyhow::Result;
use itertools::Itertools;
use opentelemetry::KeyValue;
use opentelemetry_sdk::metrics::{MeterProvider, PeriodicReader};
use opentelemetry_sdk::Resource;
use opentelemetry_stdout::MetricsExporter;
use regex::Regex;
use std::{fs, thread, time::Duration};
use thiserror::Error;
use tracing::{error, info, instrument, warn};
use tracing_opentelemetry::MetricsLayer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::model::route::Route;

mod model;

#[instrument]
fn expensive_work() -> &'static str {
    info!(
        task = "tracing_setup",
        result = "success",
        "tracing successfully set up",
    );

    thread::sleep(Duration::from_millis(25));

    more_expensive_work();

    even_more_expensive_work();

    "success"
}

#[instrument]
fn more_expensive_work() {
    thread::sleep(Duration::from_millis(25));
}

#[instrument]
fn even_more_expensive_work() {
    info!("starting even more work");
    thread::sleep(Duration::from_millis(25));
    info!("ending even more work");
}

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
    // Install an otel pipeline with a simple span processor that exports data one at a time when
    // spans end. See the `install_batch` option on each exporter's pipeline builder to see how to
    // export in batches.
    let tracer = opentelemetry_jaeger::new_agent_pipeline()
        .with_service_name("HZPP_delays")
        .install_simple()?;
    let opentelemetry = tracing_opentelemetry::layer().with_tracer(tracer);

    let opentelemetry_metrics = MetricsLayer::new(init_meter_provider());

    tracing_subscriber::registry()
        .with(opentelemetry)
        .with(opentelemetry_metrics)
        .try_init()?;

    expensive_work();

    let routes: Vec<Route> =
        serde_json::from_str(&fs::read_to_string("example_responses/routes.json")?)?;

    let delay_html = fs::read_to_string("example_responses/delay.html")?;

    if delay_html.contains("Vlak je redovit") {
        info!("The train is running on time");
    } else {
        let late_regex = Regex::new("Kasni . min")?;

        let capture: i32 = late_regex
            .captures(&delay_html)
            .ok_or(HzppError::CouldntFindLateness)?[0]
            .to_owned()
            .trim()
            .split(" ")
            .collect_vec()[1]
            .to_owned()
            .parse()?;

        info!("The train is {capture} minutes late");
    }

    return Ok(());
}

#[derive(Error, Debug)]
pub enum HzppError {
    #[error("Couldn't find how late the train was in the returned html file")]
    CouldntFindLateness,
}
