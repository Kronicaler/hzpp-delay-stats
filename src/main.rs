#![feature(error_generic_member_access)]
#![feature(try_blocks, yeet_expr, try_trait_v2)]
#![feature(async_closure)]

use anyhow::Result;
use background_services::data_fetcher::get_todays_data;
use background_services::delay_checker::check_delays;
use clap::{command, Parser, Subcommand};
use dotenvy::dotenv;
use model::db_model::RouteDb;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry::KeyValue;
use opentelemetry_otlp::{SpanExporterBuilder, TonicExporterBuilder, WithExportConfig};
use opentelemetry_sdk::runtime::Tokio;
use opentelemetry_sdk::trace::{Config, TracerProvider};
use opentelemetry_sdk::Resource;
use std::env;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
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

#[derive(Parser, Debug)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    #[command(about = "Start up the frontend in dev mode for development purposes")]
    Front {},
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    _ = dotenv();

    let cli = Cli::parse();
    println!("{:?}", cli);
    if cli.command.is_some() {
        let _ = Command::new("pwsh")
            .args(["-c ", "cd client; npm run dev -- --open"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .expect("Failed to execute command");
    }

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
            Ok(_) => unreachable!(),
            Err(err) => error!("{:?}",err),
        }},
    res = delay_checker =>{
        match res{
            Ok(_) => unreachable!(),
            Err(err) => error!("{:?}",err),
        }},
    _ = wait_for_signal() =>{info!("Received shutdown signal")}
    }

    Ok(())
}

#[cfg(unix)]
async fn wait_for_signal_impl() {
    use tokio::signal::unix::{signal, SignalKind};

    let mut signal_terminate = signal(SignalKind::terminate()).unwrap();
    let mut signal_interrupt = signal(SignalKind::interrupt()).unwrap();

    tokio::select! {
        _ = signal_terminate.recv() => tracing::debug!("Received SIGTERM."),
        _ = signal_interrupt.recv() => tracing::debug!("Received SIGINT."),
    };
}

#[cfg(windows)]
async fn wait_for_signal_impl() {
    use tokio::signal::windows;

    let mut signal_c = windows::ctrl_c().unwrap();
    let mut signal_break = windows::ctrl_break().unwrap();
    let mut signal_close = windows::ctrl_close().unwrap();
    let mut signal_shutdown = windows::ctrl_shutdown().unwrap();

    tokio::select! {
        _ = signal_c.recv() => tracing::debug!("Received CTRL_C."),
        _ = signal_break.recv() => tracing::debug!("Received CTRL_BREAK."),
        _ = signal_close.recv() => tracing::debug!("Received CTRL_CLOSE."),
        _ = signal_shutdown.recv() => tracing::debug!("Received CTRL_SHUTDOWN."),
    };
}

/// Registers signal handlers and waits for a signal that
/// indicates a shutdown request.
async fn wait_for_signal() {
    wait_for_signal_impl().await
}
