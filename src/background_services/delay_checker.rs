//! Responsible for checking the delays of routes gotten from the route_fetcher

use std::{
    collections::{HashMap, hash_map::RandomState},
    time::Duration,
    vec,
};

use anyhow::{Context, Error, bail};
use chrono::Utc;
use itertools::Itertools;
use sqlx::{Pool, Postgres};
use tokio::{spawn, sync::mpsc::Receiver, time::sleep};
use tracing::{Instrument, error, info, info_span};

use crate::{
    dal::{
        api::{Status, StatusResponse, TrainStatus, get_route_status},
        db::{
            get_stations, get_unfinished_routes, update_route_real_times, update_stop_real_arrival,
            update_stop_real_departure,
        },
    },
    model::db_model::{RouteDb, StationDb, StopDb},
};

/// Checks the delays of the routes from the given channel and saves them to the DB
pub async fn check_delays(
    delay_checker_receiver: &mut Receiver<Vec<RouteDb>>,
    pool: &Pool<Postgres>,
) -> Result<(), Error> {
    let mut buffer: Vec<Vec<RouteDb>> = vec![];

    let pool1 = pool.clone();
    spawn(
        async move {
            let unfinished_routes = match get_unfinished_routes(&pool1).await {
                Ok(r) => r,
                Err(e) => {
                    error!("{:?}", e);
                    return;
                }
            };
            spawn_route_delay_tasks(unfinished_routes, &pool1).await;
        }
        .instrument(info_span!("spawn_unfinished_route_tasks")),
    );

    while delay_checker_receiver.recv_many(&mut buffer, 32).await != 0 {
        let routes = buffer.drain(..).flatten().collect_vec();

        spawn_route_delay_tasks(routes, pool).await;
    }

    info!("Channel closed");

    Ok(())
}

async fn spawn_route_delay_tasks(routes: Vec<RouteDb>, pool: &Pool<Postgres>) {
    for route in routes {
        let secs_until_end = route.expected_end_time.timestamp() - Utc::now().timestamp();
        if secs_until_end < 0 {
            info!("Got route in the past, discarding it");

            continue;
        }

        let delay_pool = pool.clone();
        spawn(monitor_route(route, delay_pool));
    }
}

async fn monitor_route(route: RouteDb, pool: Pool<Postgres>) -> Result<(), Error> {
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

    if secs_until_start <= 0 {
        info!("Got ongoing route, starting monitoring");

        check_delay_until_route_completion(route, pool).await?;

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
) -> Result<(), Error> {
    let mut train_has_started = route.real_start_time.is_some();
    let stations: HashMap<String, StationDb, RandomState> = HashMap::from_iter(
        get_stations(pool.clone())
            .await?
            .into_iter()
            .map(|s| (s.id.clone(), s)),
    );

    loop {
        sleep(Duration::from_secs(60))
            .instrument(info_span!("Waiting 60 seconds"))
            .await;

        if Utc::now() > route.expected_end_time + chrono::Duration::try_hours(12).unwrap() {
            info!(train_timeout = true);

            if route.real_start_time.is_none() {
                info!(train_never_started = true);
            }

            if route.real_end_time.is_none() {
                info!(train_never_finished = true);
            }

            return Ok(());
        }

        let status: TrainStatus = match get_route_status(&route).await {
            Ok(dr) => match dr {
                StatusResponse::TrainStatus(ts) => ts,
                StatusResponse::TrainNotEvidented => {
                    info!(train_not_evidented = true);
                    return Ok(());
                }
                StatusResponse::UnisysError => {
                    info!(unisys_error = true);
                    continue;
                }
            },
            Err(err) => {
                error!("{:?}", err.context("error fetching delay"));
                continue;
            }
        };

        let minutes_late = status.get_minutes_late();

        if minutes_late.is_none() {
            continue;
        }
        let minutes_late = minutes_late.unwrap();

        match status.status {
            Status::Formed(_) => {
                continue;
            }
            Status::DepartingFromStation(_) => {
                if route.real_start_time.is_none() {
                    train_has_started = true;
                    route.real_start_time = Some(
                        route.expected_start_time
                            + chrono::Duration::try_minutes(minutes_late.into()).unwrap(),
                    );
                    update_route_real_times(&route, &pool).await?;
                }

                update_current_stop_departure(&mut route, &stations, &status, minutes_late, &pool)
                    .await?;
            }
            Status::Arriving(_) => {
                if route.real_start_time.is_none() {
                    train_has_started = true;
                    route.real_start_time = Some(
                        route.expected_start_time
                            + chrono::Duration::try_minutes(minutes_late.into()).unwrap(),
                    );
                    update_route_real_times(&route, &pool).await?;
                }

                update_current_stop_arrival(&mut route, &stations, &status, minutes_late, &pool)
                    .await?;
            }
            Status::FinishedDriving(datetime) => {
                if datetime < Utc::now() - chrono::Duration::try_hours(12).unwrap() {
                    continue;
                }

                update_current_stop_arrival(&mut route, &stations, &status, minutes_late, &pool)
                    .await?;

                if train_has_started {
                    route.real_end_time = Some(
                        route.expected_end_time
                            + chrono::Duration::try_minutes(minutes_late.into()).unwrap(),
                    );
                    update_route_real_times(&route, &pool).await?;
                    return Ok(());
                }
            }
        }
    }
}

/// How many minutes the `minutes late` and `status time` are allowed to differ from one another.
///
/// If the data is correct these two should always be the same. but we don't know if the data is correct...
const ALLOWED_TIME_DIFF: i64 = 10;

async fn update_current_stop_arrival(
    route: &mut RouteDb,
    stations: &HashMap<String, StationDb>,
    status: &TrainStatus,
    minutes_late: i32,
    pool: &Pool<Postgres>,
) -> Result<(), Error> {
    let current_stop = get_current_stop(&mut route.stops, &stations, &status);
    info!(current_stop = ?current_stop);
    let Some(current_stop) = current_stop else {
        return Ok(());
    };
    if current_stop.real_arrival.is_some() {
        return Ok(()); // Stop arrival is already written
    }

    current_stop.real_arrival = Some(
        current_stop.expected_arrival + chrono::Duration::try_minutes(minutes_late.into()).unwrap(),
    );

    if (current_stop.real_departure.unwrap() - status.status.get_time()).abs()
        > chrono::Duration::try_minutes(ALLOWED_TIME_DIFF).unwrap()
    {
        error!("times don't add up");
        error!(?status, minutes_late, ?current_stop);
        bail!("times don't add up");
    }

    update_stop_real_arrival(
        current_stop,
        route.expected_start_time,
        &route.id,
        pool.clone(),
    )
    .await?;

    Ok(())
}

async fn update_current_stop_departure(
    route: &mut RouteDb,
    stations: &HashMap<String, StationDb>,
    status: &TrainStatus,
    minutes_late: i32,
    pool: &Pool<Postgres>,
) -> Result<(), Error> {
    let current_stop = get_current_stop(&mut route.stops, &stations, &status);
    info!(current_stop = ?current_stop);
    let Some(current_stop) = current_stop else {
        return Ok(());
    };
    if current_stop.real_departure.is_some() {
        return Ok(()); // stop departure was already written
    }

    current_stop.real_departure = Some(
        current_stop.expected_departure
            + chrono::Duration::try_minutes(minutes_late.into()).unwrap(),
    );

    if (current_stop.real_departure.unwrap() - status.status.get_time()).abs()
        > chrono::Duration::try_minutes(ALLOWED_TIME_DIFF).unwrap()
    {
        error!("times don't add up");
        error!(?status, minutes_late, ?current_stop);
        bail!("times don't add up");
    }

    update_stop_real_departure(
        current_stop,
        route.expected_start_time,
        &route.id,
        pool.clone(),
    )
    .await?;

    Ok(())
}

#[tracing::instrument(skip(stops, stations))]
fn get_current_stop<'a>(
    stops: &'a mut Vec<StopDb>,
    stations: &HashMap<String, StationDb>,
    status: &TrainStatus,
) -> Option<&'a mut StopDb> {
    let mut current_stops = stops
        .iter_mut()
        .filter_map(|s| {
            let station = match stations.get(&s.station_id) {
                Some(s) => s,
                None => {
                    error!("Got unknown stop id");
                    return None;
                }
            };

            match is_delay_station_similar_to_stop_name(&status.station, &station.name) {
                Ok(r) => match r {
                    true => return Some(s),
                    false => None,
                },
                Err(e) => {
                    error!("{:?}", e);
                    return None;
                }
            }
        })
        .collect_vec();

    let current_stop = current_stops.pop();
    info!(current_stop= ?current_stop);
    current_stop
}

fn is_delay_station_similar_to_stop_name(
    delay_station_name: &str,
    stop_name: &str,
) -> Result<bool, Error> {
    let binding = delay_station_name.to_lowercase().replace(".", "");
    let delay_station_words = binding.trim().split(" ").collect_vec();

    let binding = stop_name.to_lowercase().replace(".", "");
    let stop_words = binding.trim().split(" ").collect_vec();

    if delay_station_words.len() != stop_words.len() {
        return Ok(false);
    }

    let are_all_words_similar = delay_station_words
        .iter()
        .zip(stop_words.iter())
        .map(|(w1, w2)| w1.contains(w2) || w2.contains(w1))
        .reduce(|acc, e| acc && e)
        .context("empty iterator???")?;

    Ok(are_all_words_similar)
}

#[cfg(test)]
mod tests {
    #[test]
    fn is_delay_station_similar_to_stop_name_test1() {
        let str1 = "SV. IVAN ŽABNO";
        let str2 = "SVeti IVAN ŽABNO";

        assert!(super::is_delay_station_similar_to_stop_name(str1, str2).unwrap());
    }

    #[test]
    fn is_delay_station_similar_to_stop_name_test2() {
        let str1 = "Zagreb GL. Kol.";
        let str2 = "Zagreb Glavni kolodvor";

        assert!(super::is_delay_station_similar_to_stop_name(str1, str2).unwrap());
    }

    #[test]
    fn is_delay_station_similar_to_stop_name_test3() {
        let str1 = "Zagreb GL. Kol.";
        let str2 = "Zagreb zapadni kolodvor";

        assert!(!super::is_delay_station_similar_to_stop_name(str1, str2).unwrap());
    }

    #[test]
    fn is_delay_station_similar_to_stop_name_test4() {
        let str1 = "Novska";
        let str2 = "NOVSKA";

        assert!(super::is_delay_station_similar_to_stop_name(str1, str2).unwrap());
    }
}
