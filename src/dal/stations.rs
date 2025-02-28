use anyhow::Error;
use sqlx::{query_as, Pool, Postgres, QueryBuilder, Transaction};
use tracing::{info_span, Instrument};

use crate::model::db_model::StationDb;

pub async fn insert_stations(
    stations: &Vec<StationDb>,
    tx: &mut Transaction<'_, Postgres>,
) -> Result<(), Error> {
    let mut query_builder = QueryBuilder::new(
        "INSERT into stations (
                id,
                code,
                name,
                latitude,
                longitude
            )",
    );
    query_builder.push_values(stations, |mut b, station| {
        b.push_bind(station.id.clone())
            .push_bind(station.code)
            .push_bind(station.name.clone())
            .push_bind(station.latitude)
            .push_bind(station.longitude);
    });
    query_builder.push(" ON CONFLICT ( id ) DO NOTHING");

    query_builder
        .build()
        .execute(&mut **tx)
        .instrument(info_span!("Inserting stations"))
        .await?;

    Ok(())
}

pub async fn get_stations(pool: Pool<Postgres>) -> Result<Vec<StationDb>, Error> {
    let res = query_as!(StationDb, "Select * from stations")
        .fetch_all(&pool)
        .await?;

    return Ok(res);
}