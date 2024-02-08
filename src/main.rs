#![feature(error_generic_member_access)]
#![feature(try_blocks)]

use anyhow::Result;
use background_services::data_fetcher::get_todays_data;
use background_services::delay_checker::check_delays;
use dotenvy::dotenv;
use model::db_model::RouteDb;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry::KeyValue;
use opentelemetry_otlp::{SpanExporterBuilder, TonicExporterBuilder, WithExportConfig};
use opentelemetry_sdk::runtime::Tokio;
use opentelemetry_sdk::trace::{Config, TracerProvider};
use opentelemetry_sdk::Resource;
use std::env;
use std::time::Duration;
use tokio::sync::mpsc::channel;
use tokio::time::sleep;
use tokio::{select, spawn};
use tracing::{error, info};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Registry};

mod background_services;
mod model;
mod utils;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    _ = dotenv();
    info!("OTLP_ENDPOINT: {}", dotenvy::var("OTLP_ENDPOINT").unwrap());
    let provider = TracerProvider::builder()
        .with_batch_exporter(
            SpanExporterBuilder::Tonic(
                TonicExporterBuilder::default()
                    .with_timeout(Duration::from_millis(1000))
                    .with_endpoint(
                        dotenvy::var("OTLP_ENDPOINT")
                            .unwrap_or("http://localhost:4317".to_string()),
                    )
                    .with_protocol(opentelemetry_otlp::Protocol::Grpc),
            )
            .build_span_exporter()
            .unwrap(),
            Tokio,
        )
        .with_config(
            Config::default().with_resource(Resource::new(vec![KeyValue::new(
                "service.name",
                "HZPP_delay_stats",
            )])),
        )
        .build();

    let tracer = provider.tracer("HZPP_delay_stats");

    let telemetry_layer = tracing_opentelemetry::layer().with_tracer(tracer);

    let env_filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();

    let appender = tracing_appender::rolling::daily("./logs", "hzpp_delay_stats.log");
    let (non_blocking_appender, _guard) = tracing_appender::non_blocking(appender);

    // A layer that logs events to rolling files.
    let file_log = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking_appender)
        .with_ansi(false)
        .pretty();

    Registry::default()
        .with(telemetry_layer)
        .with(file_log)
        .with(env_filter)
        .init();

    let db_url = env::var("DATABASE_URL").unwrap();

    let pool = sqlx::PgPool::connect(&db_url).await.unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();

    let (delay_checker_sender, mut delay_checker_receiver) = channel::<Vec<RouteDb>>(32);

    let route_fetcher_pool = pool.clone();
    let route_fetcher = spawn(async move {
        loop {
            if let Err(e) = get_todays_data(&route_fetcher_pool, delay_checker_sender.clone()).await
            {
                error!("{e}");
                sleep(Duration::from_secs(60)).await;
            } else {
                sleep(Duration::from_secs(60 * 60)).await;
            }
        }
    });

    let delay_checker = spawn(async move {
        loop {
            if let Err(e) = check_delays(&mut delay_checker_receiver, &pool).await {
                error!("{e}");
            }
        }
    });

    select! {
    res = route_fetcher =>{
        match res{
            Ok(_) => todo!(),
            Err(err) => error!("{:?}",err),
        }},
    res = delay_checker =>{
        match res{
            Ok(_) => todo!(),
            Err(err) => error!("{:?}",err),
        }},
    }

    Ok(())
}
