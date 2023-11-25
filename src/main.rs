#![feature(error_generic_member_access)]
#![feature(try_blocks)]

use anyhow::Result;
use dotenvy::dotenv;
use itertools::Itertools;
use model::db_model::RouteDb;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry::KeyValue;
use opentelemetry_otlp::new_exporter;
use opentelemetry_sdk::trace::{Config, TracerProvider};
use opentelemetry_sdk::Resource;
use regex::Regex;
use sqlx::Postgres;
use std::env;
use std::num::ParseIntError;
use std::{fs, thread, time::Duration};
use thiserror::Error;
use tracing::{error, info, instrument, span, warn, Level};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Registry};

use crate::background_services::route_fetcher::get_routes;
use crate::model::hzpp_api_model::HzppRoute;

mod background_services;
mod model;

#[tokio::main]
async fn main() -> Result<()> {
    _ = dotenv();

    let span_exporter = new_exporter().tonic().build_span_exporter()?;

    let provider = TracerProvider::builder()
        .with_simple_exporter(span_exporter)
        .with_config(
            Config::default().with_resource(Resource::new(vec![KeyValue::new(
                "service.name",
                "HZPP_delays",
            )])),
        )
        .build();

    let tracer = provider.tracer("HZPP_delays");

    let telemetry_layer = tracing_opentelemetry::layer().with_tracer(tracer);

    let env_filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();

    let stdout_log = tracing_subscriber::fmt::layer().pretty();

    let appender = tracing_appender::rolling::daily("./logs", "hzpp_delays.log");
    let (non_blocking_appender, _guard) = tracing_appender::non_blocking(appender);

    // A layer that logs events to rolling files.
    let file_log = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking_appender)
        .with_ansi(false)
        .pretty();

    let _subscriber = Registry::default()
        .with(telemetry_layer)
        .with(stdout_log)
        .with(file_log)
        .with(env_filter)
        .init();

    let db_url = env::var("DATABASE_URL").unwrap();

    let pool = sqlx::PgPool::connect(&db_url).await.unwrap();

    _ = fetch_routes_job(pool.clone()).await;

    Ok(())
    // let sched = JobScheduler::new().await?;

    // sched
    //     .add(Job::new_async("1/10 * * * * * *", |_uuid, _lock| {
    //         Box::pin(async move {
    //             _ = fetch_routes_job().await;
    //         })
    //     })?)
    //     .await?;

    // sched.start().await?;

    // loop {
    //     tokio::time::sleep(Duration::from_secs(u64::MAX)).await;
    // }
}

#[tracing::instrument(err)]
async fn fetch_routes_job(pool: sqlx::Pool<Postgres>) -> anyhow::Result<()> {
    let routes = get_routes()
        .await?
        .into_iter()
        .map(|r| RouteDb::try_from(r))
        .filter_map(|r| {
            if let Err(e) = r {
                error!("Error turning HzppRoute to RouteDb {e}");
                return None;
            }

            return r.ok();
        })
        .collect_vec();

    save_routes(&routes, pool.clone()).await?;
    send_routes_to_delay_checker(routes)?;

    Ok(())
}

#[tracing::instrument(err)]
pub fn send_routes_to_delay_checker(routes: Vec<RouteDb>) -> Result<()> {
    Ok(())
}

#[tracing::instrument(err)]
pub async fn save_routes(routes: &Vec<RouteDb>, pool: sqlx::Pool<Postgres>) -> Result<()> {
    for route in routes {
        sqlx::query!(
            r#"
    INSERT INTO routes (
        id,
        route_number,
        usual_source,
        destination,
        bikes_allowed,
        wheelchair_accessible,
        route_type,
        expected_start_time,
        expected_end_time
    ) 
    VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
    ON CONFLICT ( expected_start_time, id )
    DO NOTHING
        "#,
            route.id,
            route.route_number,
            route.usual_source,
            route.destination,
            route.bikes_allowed,
            route.wheelchair_accessible,
            route.route_type,
            route.expected_start_time,
            route.expected_end_time
        )
        .execute(&pool)
        .await
        .unwrap();
    }

    Ok(())
}

#[instrument(err)]
fn get_delay() -> Result<(), HzppError> {
    {
        let span = span!(Level::TRACE, "getting routes");
        let _enter = span.enter();

        let routes_file = "documentation/example_responses/routes.json";
        let routes: Vec<HzppRoute> =
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
        #[source]
        source: std::io::Error,
        file: String,
    },
    #[error("Error parsing a file")]
    FileParsingError(#[from] serde_json::Error),
    #[error("Error parsing the delay duration")]
    DelayParsingError(#[from] ParseIntError),
}
