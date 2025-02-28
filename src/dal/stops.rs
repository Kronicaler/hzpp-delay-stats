

use anyhow::Error;
use chrono::DateTime;
use chrono::Utc;
use itertools::Itertools;
use sqlx::query;
use sqlx::Pool;
use sqlx::QueryBuilder;

use sqlx::Postgres;
use sqlx::Transaction;
use tracing::Instrument;

use crate::model::db_model::StopDb;

use tracing::info_span;

pub async fn insert_stops( stops: Vec<&StopDb>, tx: &mut Transaction<'_, Postgres>) -> Result<(), Error> {
    let stops_chunks = stops.chunks(1024).collect_vec();
    Ok(for stops in stops_chunks {
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

        query_builder.push_values(stops, |mut b, stop| {
            b.push_bind(&stop.station_id)
                .push_bind(&stop.route_id)
                .push_bind(stop.route_expected_start_time)
                .push_bind(stop.sequence)
                .push_bind(stop.real_arrival)
                .push_bind(stop.expected_arrival)
                .push_bind(stop.real_departure)
                .push_bind(stop.expected_departure);
        });

        query_builder
            .push(" ON CONFLICT ( route_id, route_expected_start_time, sequence ) DO NOTHING");

        query_builder
            .build()
            .execute(&mut **tx)
            .instrument(info_span!("Inserting stops"))
            .await?;
    })
}


#[tracing::instrument(err)]
pub async fn update_stop_real_departure(
    stop: &StopDb,
    route_expected_start_time: DateTime<Utc>,
    route_id: &str,
    pool: Pool<Postgres>,
) -> Result<(), Error> {
    query!(
        "
UPDATE stops
SET real_departure = $1
where route_expected_start_time = $2 and route_id = $3 and sequence = $4
",
        stop.real_departure,
        route_expected_start_time,
        route_id,
        stop.sequence,
    )
    .execute(&pool)
    .await?;

    Ok(())
}


#[tracing::instrument(err)]
pub async fn update_stop_real_arrival(
    stop: &StopDb,
    route_expected_start_time: DateTime<Utc>,
    route_id: &str,
    pool: Pool<Postgres>,
) -> Result<(), Error> {
    query!(
        "
    UPDATE stops
    SET real_arrival = $1
    where route_expected_start_time = $2 and route_id = $3 and sequence = $4
    ",
        stop.real_arrival,
        route_expected_start_time,
        route_id,
        stop.sequence,
    )
    .execute(&pool)
    .await?;

    Ok(())
}