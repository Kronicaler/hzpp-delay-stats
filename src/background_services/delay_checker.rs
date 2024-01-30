//! Responsible for checking the delays of routes gotten from the route_fetcher

use std::time::Duration;

use anyhow::{anyhow, bail};
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use chrono_tz::Europe::Zagreb;
use itertools::Itertools;
use reqwest::{header::HeaderValue, Client, Method, Url};
use sqlx::{postgres::PgRow, query, Pool, Postgres, Row};
use tokio::{spawn, sync::mpsc::Receiver, time::sleep};
use tracing::{error, info, info_span, Instrument};

use crate::{model::db_model::RouteDb, utils::str_between_str};

/// Checks the delays of the routes from the given channel and saves them to the DB
pub async fn check_delays(
    delay_checker_receiver: &mut Receiver<Vec<RouteDb>>,
    pool: &Pool<Postgres>,
) -> Result<(), anyhow::Error> {
    let mut buffer: Vec<Vec<RouteDb>> = vec![];

    spawn_route_delay_tasks(get_unfinished_routes(pool).await?, pool).await;

    while delay_checker_receiver.recv_many(&mut buffer, 32).await != 0 {
        let routes = buffer.drain(..).flatten().collect_vec();

        spawn_route_delay_tasks(routes, pool).await;
    }

    info!("Channel closed");

    Ok(())
}

async fn spawn_route_delay_tasks(routes: Vec<RouteDb>, pool: &Pool<Postgres>) {
    for mut route in routes {
        let secs_until_end = route.expected_end_time.timestamp() - Utc::now().timestamp();
        if secs_until_end < 0 {
            info!("Got route in the past, discarding it");

            route.real_start_time = route.real_start_time.or(Some(route.expected_start_time));
            route.real_end_time = route.real_end_time.or(Some(route.expected_end_time));
            if let Err(e) = update_route_real_times(&route, pool).await {
                error!("error when saving old route {}", e);
            }

            continue;
        }

        let delay_pool = pool.clone();
        spawn(monitor_route(route, delay_pool));
    }
}

async fn get_unfinished_routes(pool: &Pool<Postgres>) -> Result<Vec<RouteDb>, anyhow::Error> {
    let x = query(
        "SELECT 
        id,
        route_number,
        source,
        destination,
        bikes_allowed,
        wheelchair_accessible,
        route_type,
        expected_start_time,
        expected_end_time,
        real_start_time,
        real_end_time
        from routes where real_end_time IS NULL",
    )
    .map(|row: PgRow| RouteDb {
        id: row.try_get(0).unwrap(),
        route_number: row.try_get(1).unwrap(),
        source: row.try_get(2).unwrap(),
        destination: row.try_get(3).unwrap(),
        bikes_allowed: row.try_get::<i16, usize>(4).unwrap().try_into().unwrap(),
        wheelchair_accessible: row.try_get::<i16, usize>(5).unwrap().try_into().unwrap(),
        route_type: row.try_get::<i16, usize>(6).unwrap().try_into().unwrap(),
        expected_start_time: row.try_get::<NaiveDateTime, usize>(7).unwrap().and_utc(),
        expected_end_time: row.try_get::<NaiveDateTime, usize>(8).unwrap().and_utc(),
        real_start_time: row
            .try_get::<Option<NaiveDateTime>, usize>(9)
            .unwrap()
            .map(|dt| dt.and_utc()),
        real_end_time: row
            .try_get::<Option<NaiveDateTime>, usize>(10)
            .unwrap()
            .map(|dt| dt.and_utc()),
    })
    .fetch_all(pool)
    .await?;

    return Ok(x);
}

async fn monitor_route(route: RouteDb, pool: Pool<Postgres>) -> Result<(), anyhow::Error> {
    let secs_until_start = route.expected_start_time.timestamp() - Utc::now().timestamp();
    let secs_until_end = route.expected_end_time.timestamp() - Utc::now().timestamp();

    info!(
        "Monitoring route {:#?} starting in {}",
        route, secs_until_start
    );

    if secs_until_end < 0 {
        info!("Got route in the past, discarding it");
        return Ok(());
    }

    info!("Waiting {} seconds for route to start", secs_until_start);
    sleep(Duration::from_secs(
        secs_until_start.try_into().unwrap_or(0),
    ))
    .await;

    check_delay_until_route_completion(route, pool).await?;

    Ok(())
}

#[tracing::instrument(err, fields(route_number=route.route_number))]
async fn check_delay_until_route_completion(
    mut route: RouteDb,
    pool: Pool<Postgres>,
) -> Result<(), anyhow::Error> {
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

        match (delay.delay, delay.status) {
            (Delay::WaitingToDepart, Status::DepartingFromStation(_)) => {} // wait for train to start
            (Delay::WaitingToDepart, Status::FinishedDriving(_)) => {
                bail!("Got WaitingToDepart and FinishedDriving")
            }
            (Delay::OnTime, Status::DepartingFromStation(_)) => {
                if route.real_start_time.is_none() {
                    route.real_start_time = Some(route.expected_start_time);
                    update_route_real_times(&route, &pool).await?;
                }
            }
            (Delay::Late { minutes_late }, Status::DepartingFromStation(_)) => {
                if route.real_start_time.is_none() {
                    route.real_start_time = Some(
                        route.expected_start_time + chrono::Duration::minutes(minutes_late.into()),
                    );
                    update_route_real_times(&route, &pool).await?;
                }
            }
            (Delay::OnTime, Status::FinishedDriving(_)) => {
                route.real_end_time = Some(route.expected_end_time);
                update_route_real_times(&route, &pool).await?;
                return Ok(());
            }
            (Delay::Late { minutes_late }, Status::FinishedDriving(_)) => {
                route.real_end_time =
                    Some(route.expected_end_time + chrono::Duration::minutes(minutes_late.into()));
                update_route_real_times(&route, &pool).await?;
                return Ok(());
            }
        }

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

    let status = parse_delay_html2(content)?;

    Ok(status)
}

#[tracing::instrument(ret, err)]
fn parse_delay_html2(html: String) -> Result<TrainStatus, anyhow::Error> {
    let lines = html.lines().collect_vec();

    let station_line = *lines
        .iter()
        .filter(|l| l.contains("Kolodvor:"))
        .collect_vec()
        .first()
        .ok_or_else(|| anyhow!("Couldn't locate station line"))?;

    let station = str_between_str(station_line, "</I><strong>", "<br>")
        .ok_or_else(|| anyhow!("Couldn't locate station"))?
        .to_string();

    let status_line = *lines
        .iter()
        .enumerate()
        .filter(|l| l.1.contains("Završio") || l.1.contains("Odlazak"))
        .collect_vec()
        .first()
        .ok_or_else(|| anyhow!("Couldn't locate status line"))?;
    let status_time_line = lines
        .get(status_line.0 + 1)
        .ok_or_else(|| anyhow!("couldn't locate status time line"))?;

    let status_date = NaiveDate::parse_from_str(&status_time_line[..8], "%d.%m.%y.")?;
    let status_time = NaiveTime::parse_from_str(&status_time_line[12..16], "%H:%M")?;
    let status_datetime = status_date
        .and_time(status_time)
        .and_local_timezone(Zagreb)
        .earliest()
        .ok_or_else(|| anyhow!("invalid date"))?
        .with_timezone(&Utc);

    let status = match status_line {
        ref sl if sl.1.contains("Završio") => Status::FinishedDriving(status_datetime),
        ref sl if sl.1.contains("Odlazak") => Status::DepartingFromStation(status_datetime),
        _ => bail!("Couldn't construct status"),
    };

    let delay = if html.contains("Kasni") {
        let minutes_late: i32 = str_between_str(&html, "Kasni", "min.")
            .ok_or_else(|| anyhow!("Couldn't find delay number"))?
            .trim()
            .parse()?;
        Delay::Late { minutes_late }
    } else if html.contains("Vlak ceka polazak") {
        Delay::WaitingToDepart
    } else if html.contains("Vlak je redovit") {
        Delay::OnTime
    } else if html.contains("Vlak nije u evidenciji") {
        bail!("The train is not registered");
    } else {
        bail!("Unknown delay response");
    };

    Ok(TrainStatus {
        delay,
        station,
        status,
    })
}

#[tracing::instrument(ret)]
fn parse_delay_html(html: &String) -> Result<i32, anyhow::Error> {
    let x = html
        .find("Kasni ")
        .ok_or_else(|| anyhow!("couldn't find Kasni"))?
        + "Kasni ".len();

    let y = html
        .find(" min.")
        .ok_or_else(|| anyhow!("couldn't find min."))?;

    let result: i32 = html[x..y].trim().parse()?;

    Ok(result)
}

#[derive(Clone, Debug)]
struct TrainStatus {
    pub station: String,
    pub status: Status,
    pub delay: Delay,
}

#[derive(Copy, Clone, Debug)]
enum Delay {
    WaitingToDepart,
    OnTime,
    Late { minutes_late: i32 },
}

#[derive(Copy, Clone, Debug)]
enum Status {
    DepartingFromStation(DateTime<Utc>),
    FinishedDriving(DateTime<Utc>),
}

#[tracing::instrument(err)]
async fn update_route_real_times(
    route: &RouteDb,
    pool: &Pool<Postgres>,
) -> Result<(), anyhow::Error> {
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
