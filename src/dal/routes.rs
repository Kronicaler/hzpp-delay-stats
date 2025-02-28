use anyhow::Error;
use sqlx::{postgres::PgRow, query, query_as, Pool, Postgres, Row, Transaction};
use tokio::task::JoinSet;

use crate::{model::db_model::StopDb, RouteDb};

use tracing::{info_span, Instrument};

use sqlx::QueryBuilder;

/// Returns saved route_numbers
pub async fn insert_routes(
    routes: &Vec<RouteDb>,
    tx: &mut Transaction<'_, Postgres>,
) -> Result<Vec<i32>, Error> {
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
        .fetch_all(&mut **tx)
        .instrument(info_span!("Inserting routes"))
        .await?;
    Ok(saved_route_nums)
}

#[tracing::instrument(err, skip(pool))]
pub async fn get_unfinished_routes(pool: &Pool<Postgres>) -> Result<Vec<RouteDb>, Error> {
    let routes: Vec<RouteDb> = query_as(
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
        from routes where real_end_time IS NULL or real_start_time IS NULL",
    )
    .fetch_all(pool)
    .await?;

    let mut set = JoinSet::new();

    for mut route in routes.into_iter() {
        let pool = pool.clone();
        set.spawn(async move {
            let stops: Vec<StopDb> = query_as!(
                StopDb,
                "SELECT * from stops where route_id = $1 and route_expected_start_time = $2",
                route.id.clone(),
                route.expected_start_time
            )
            .fetch_all(&pool)
            .await?;

            route.stops = stops;

            Ok::<RouteDb, Error>(route)
        });
    }

    let mut routes = vec![];

    while let Some(res) = set.join_next().await {
        let route = res??;
        routes.push(route);
    }

    Ok(routes)
}

#[tracing::instrument(err)]
pub async fn update_route_real_times(route: &RouteDb, pool: &Pool<Postgres>) -> Result<(), Error> {
    query!(
        "
    UPDATE routes
    SET real_start_time = $1, real_end_time=$2
    where expected_start_time = $3 and id = $4
    ",
        route.real_start_time,
        route.real_end_time,
        route.expected_start_time,
        route.id
    )
    .execute(pool)
    .await?;

    Ok(())
}
