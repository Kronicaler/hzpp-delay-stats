//! Responsible for checking the delays of routes gotten from the route_fetcher

use std::time::Duration;

use anyhow::anyhow;
use chrono::{DurationRound, Utc};
use itertools::Itertools;
use reqwest::{header::HeaderValue, Client, Method, Url};
use sqlx::{query, Pool, Postgres};
use tokio::{spawn, sync::mpsc::Receiver, time::sleep};
use tracing::{error, info, info_span, Instrument};

use crate::model::db_model::RouteDb;

/// Checks the delays of the routes from the given channel and saves them to the DB
pub async fn check_delays(
    delay_checker_receiver: &mut Receiver<Vec<RouteDb>>,
    pool: &Pool<Postgres>,
) -> Result<(), anyhow::Error> {
    let mut buffer: Vec<Vec<RouteDb>> = vec![];

    while delay_checker_receiver.recv_many(&mut buffer, 32).await != 0 {
        let routes = buffer.drain(..).flatten().collect_vec();

        for route in routes {
            let secs_until_start = route.expected_start_time.timestamp() - Utc::now().timestamp();
            if secs_until_start < 0 {
                info!("Got route in the past, discarding it");
                continue;
            }

            let delay_pool = pool.clone();
            spawn(check_delay(route, delay_pool));
        }
    }

    info!("Channel closed");

    Ok(())
}

#[tracing::instrument(err)]
async fn check_delay(mut route: RouteDb, pool: Pool<Postgres>) -> Result<(), anyhow::Error> {
    let secs_until_start = route.expected_start_time.timestamp() - Utc::now().timestamp();

    info!(
        "Checking delays for route {:#?} starting in {}",
        route, secs_until_start
    );

    if secs_until_start < 0 {
        info!("Got route in the past, discarding it");
        return Ok(());
    }

    sleep(Duration::from_secs(secs_until_start.try_into()?))
        .instrument(info_span!("Waiting for route to start"))
        .await;

    loop {
        let delay: TrainStatus = match get_route_delay(&route).await {
            Ok(it) => it,
            Err(err) => {
                error!("{:?}", err.context("error fetching delay"));
                sleep(Duration::from_secs(60))
                    .instrument(info_span!("Waiting 60 seconds"))
                    .await;
                continue;
            }
        };

        match delay {
            TrainStatus::WaitingForDeparture => {}
            TrainStatus::OnTime => {
                if route.real_start_time.is_none() {
                    route.real_start_time = Some(route.expected_end_time);
                    update_route(&route, &pool).await?;
                }
            }
            TrainStatus::Late { minutes_late: _ } => {
                if route.real_start_time.is_none() {
                    route.real_start_time =
                        Some(Utc::now().duration_round(chrono::Duration::minutes(1))?);
                    update_route(&route, &pool).await?;
                }
            }
            TrainStatus::Finished { minutes_late: _ } => {
                route.real_end_time =
                    Some(Utc::now().duration_round(chrono::Duration::minutes(1))?);
                update_route(&route, &pool).await?;
                return Ok(());
            }
        };

        sleep(Duration::from_secs(60))
            .instrument(info_span!("Waiting 60 seconds"))
            .await;
    }
}

#[tracing::instrument(ret, err)]
async fn get_route_delay(route: &RouteDb) -> Result<TrainStatus, anyhow::Error> {
    let url = format!(
        "https://traindelay.hzpp.hr/train/delay?trainId={}",
        route.route_number
    );

    let mut request = reqwest::Request::new(Method::GET, Url::parse(&url)?);
    request.headers_mut().append("Authorization", 
    HeaderValue::from_static("Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJoenBwLXBsYW5lciIsImlhdCI6MTY3NDEzNzM3NH0.a6FzxGKyUfHzLVuGP242MFWF6EspvJl1LTHwEVeMIsY"));

    let response = Client::new()
        .execute(request)
        .instrument(info_span!("Fetching delay"))
        .await?
        .error_for_status()?;

    let content = response
        .text()
        .instrument(info_span!("Reading body of response"))
        .await?;

    let delay = match content {
        ref x if x.contains("Vlak ceka polazak") => TrainStatus::WaitingForDeparture,
        ref x if x.contains("Vlak je redovit") && !x.contains("Završio je vožnju") => TrainStatus::OnTime,
        ref x if x.contains("Kasni") && !x.contains("Završio je vožnju")  => TrainStatus::Late {
            minutes_late: get_delay_from_html(x)?,
        },
        ref x if x.contains("Završio je vožnju") => TrainStatus::Finished {
            minutes_late: get_delay_from_html(x).unwrap_or(0),
        },
        _ => TrainStatus::WaitingForDeparture,
    };

    Ok(delay)
}

fn get_delay_from_html(html: &String) -> Result<i32, anyhow::Error> {
    let x = html
        .find("Kasni ")
        .ok_or_else(|| anyhow!("couldn't find Kasni"))?
        + "Kasni ".len();

    let y = html
        .find(" min.")
        .ok_or_else(|| anyhow!("couldn't find min."))?;

    let result: i32 = html[x..y].parse()?;

    Ok(result)
}

#[tracing::instrument(err)]
async fn update_route(route: &RouteDb, pool: &Pool<Postgres>) -> Result<(), anyhow::Error> {
    query!(
        "
    UPDATE routes
    SET real_start_time = $1, real_end_time=$2
    where expected_start_time = $3 and id = $4
    ",
        route.real_start_time.map(|dt| dt.naive_utc()),
        route.real_end_time.map(|dt| dt.naive_utc()),
        route.expected_start_time.naive_utc(),
        route.id
    )
    .execute(pool)
    .await?;

    Ok(())
}

#[derive(Copy, Clone, Debug)]
enum TrainStatus {
    WaitingForDeparture,
    OnTime,
    Late { minutes_late: i32 },
    Finished { minutes_late: i32 },
}
