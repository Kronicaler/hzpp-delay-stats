//! Responsible for fetching and saving routes
use crate::model::{db_model::RouteDb, hzpp_api_model::HzppRoute};
use chrono::DateTime;
use chrono_tz::{Europe::Zagreb, Tz};
use itertools::Itertools;
use sqlx::{postgres::PgRow, Postgres, QueryBuilder, Row};
use std::{backtrace::Backtrace, collections::HashSet};
use tokio::sync::mpsc::Sender;
use tracing::{debug, error, info, info_span, Instrument};

/// Gets todays routes and saves them to the DB.
/// If a duplicate route is already in the DB then it's discarded.
#[tracing::instrument(err)]
pub async fn get_todays_data(
    pool: &sqlx::Pool<Postgres>,
    delay_checker_sender: Sender<Vec<RouteDb>>,
) -> anyhow::Result<()> {
    let today = chrono::Local::now().with_timezone(&Zagreb);

    let routes = fetch_routes(today)
        .await?
        .into_iter()
        .map(|r| RouteDb::try_from_hzpp_route(r, today))
        .filter_map(|r| match r {
            Err(e) => {
                error!("Error turning HzppRoute to RouteDb {e}");
                None
            }
            Ok(r) => Some(r),
        })
        .collect_vec();

    let saved_routes = save_data(&routes, pool.clone()).await?;

    delay_checker_sender.send(saved_routes).await?;

    Ok(())
}

/// Returns the saved routes. If a route is already present in the DB it isn't saved.
/// Does not save real times.
#[tracing::instrument(err, skip(routes))]
async fn save_data(
    routes: &Vec<RouteDb>,
    pool: sqlx::Pool<Postgres>,
) -> Result<Vec<RouteDb>, anyhow::Error> {
    let transaction = pool.begin().await?;

    let mut query_builder = QueryBuilder::new(
        "INSERT INTO routes (
            id,
            route_number,
            source,
            destination,
            bikes_allowed,
            wheelchair_accessible,
            route_type,
            expected_start_time,
            expected_end_time
        )",
    );

    query_builder.push_values(routes, |mut b, route| {
        b.push_bind(&route.id)
            .push_bind(route.route_number)
            .push_bind(&route.source)
            .push_bind(&route.destination)
            .push_bind(route.bikes_allowed as i16)
            .push_bind(route.wheelchair_accessible as i16)
            .push_bind(route.route_type as i16)
            .push_bind(route.expected_start_time)
            .push_bind(route.expected_end_time);
    });

    query_builder
        .push(" ON CONFLICT ( expected_start_time, id ) DO NOTHING RETURNING route_number");

    let query = query_builder.build();

    let saved_route_nums = query
        .map(|row: PgRow| {
            let route_number: i32 = row.try_get(0).unwrap();

            route_number
        })
        .fetch_all(&pool)
        .instrument(info_span!("Inserting routes"))
        .await?;

    let mut query_builder = QueryBuilder::new(
        "INSERT into stops (
            station_id,
            route_id,
            route_expected_start_time,
            sequence,
            real_arrival,
            expected_arrival,
            real_departure,
            expected_departure
        )",
    );

    let stops = routes.iter().flat_map(|r| &r.stops).collect_vec();

    query_builder.push_values(&stops, |mut b, stop| {
        b.push_bind(&stop.station_id)
            .push_bind(&stop.route_id)
            .push_bind(stop.route_expected_start_time)
            .push_bind(stop.sequence)
            .push_bind(stop.real_arrival)
            .push_bind(stop.expected_arrival)
            .push_bind(stop.real_departure)
            .push_bind(stop.expected_departure);
    });

    query_builder.push(" ON CONFLICT ( route_id, route_expected_start_time, sequence ) DO NOTHING");

    debug!("Routes: {:#?}\nStops: {:#?}", routes, stops);

    query_builder
        .build()
        .execute(&pool)
        .instrument(info_span!("Inserting stops"))
        .await?;

    transaction.commit().await?;

    let saved_route_nums: HashSet<i32> = HashSet::from_iter(saved_route_nums);

    let saved_routes = routes
        .iter()
        .filter(|r| saved_route_nums.contains(&r.route_number))
        .cloned()
        .collect_vec();

    Ok(saved_routes)
}

#[tracing::instrument(err)]
async fn fetch_routes(date: DateTime<Tz>) -> Result<Vec<HzppRoute>, GetRoutesError> {
    let request = format!(
        "https://josipsalkovic.com/hzpp/planer/v3/getRoutes.php?date={}",
        date.format("%Y%m%d")
    );

    let response = reqwest::get(&request)
        .instrument(info_span!("Fetching routes"))
        .await?
        .error_for_status()?;

    let routes_string = response
        .text()
        .instrument(info_span!("Reading body of response"))
        .await?;

    let routes: Vec<HzppRoute> =
        serde_json::from_str(&routes_string).map_err(|e| GetRoutesError::ParsingError {
            source: e,
            backtrace: Backtrace::capture(),
            routes: routes_string,
        })?;

    info!("got {} routes", routes.len());

    Ok(routes)
}

#[derive(thiserror::Error, Debug)]
enum GetRoutesError {
    #[error("error fetching the routes \n{} \n{}", source, backtrace)]
    HttpRequestError {
        #[from]
        source: reqwest::Error,
        backtrace: Backtrace,
    },

    #[error("error parsing the routes \n{} \n{} \n {}", source, routes, backtrace)]
    ParsingError {
        source: serde_json::Error,
        backtrace: Backtrace,
        routes: String,
    },
}
