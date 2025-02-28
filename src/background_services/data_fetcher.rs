//! Responsible for fetching and saving routes
use crate::{
    dal::{insert_routes, insert_stations, insert_stops},
    model::{
        db_model::{RouteDb, StationDb},
        hzpp_api_model::{HzppRoute, HzppStation},
    },
};
use anyhow::{Context, Error};
use chrono::{DateTime, Days};
use chrono_tz::{Europe::Zagreb, Tz};
use itertools::Itertools;
use sqlx::Postgres;
use std::collections::HashSet;
use tokio::sync::mpsc::Sender;
use tracing::{Instrument, error, info, info_span};

/// Gets todays routes and saves them to the DB.
/// If a duplicate route is already in the DB then it's discarded.
/// After saving to DB sends them to the delay checker.
#[tracing::instrument(err)]
pub async fn get_todays_data(
    pool: &sqlx::Pool<Postgres>,
    delay_checker_sender: Sender<Vec<RouteDb>>,
) -> Result<(), Error> {
    let today = chrono::Local::now().with_timezone(&Zagreb);

    let stations = fetch_stations()
        .await?
        .into_iter()
        .map(|s| StationDb::from(s))
        .collect_vec();

    let mut routes = fetch_routes(today).await?;

    if routes.len() == 0 {
        info!("Due to getting no routes for today falling back to last date routes were available");
        let mut date = today
            .checked_sub_days(Days::new(7))
            .context("invalid day subtraction")?;

        while routes.len() == 0 {
            info!("fetching routes for {}", date.date_naive());
            routes = fetch_routes(date).await?;
            date = date
                .checked_sub_days(Days::new(7))
                .context("invalid day subtraction")?;
        }
    }

    let db_routes = routes
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

    let saved_routes = save_data(db_routes, stations, pool.clone()).await?;

    delay_checker_sender.send(saved_routes).await?;

    Ok(())
}

/// Returns the saved routes. If a route is already present in the DB it isn't saved.
/// Does not save real times.
#[tracing::instrument(err, skip(routes))]
async fn save_data(
    routes: Vec<RouteDb>,
    stations: Vec<StationDb>,
    pool: sqlx::Pool<Postgres>,
) -> Result<Vec<RouteDb>, Error> {
    let mut tx = pool.begin().await?;

    insert_stations(&stations, &mut tx).await?;

    let saved_route_nums = insert_routes(&routes, &mut tx).await?;

    let all_stops = routes.iter().flat_map(|r| &r.stops).collect_vec();
    insert_stops(all_stops, &mut tx).await?;

    tx.commit().await?;

    let saved_route_nums: HashSet<i32> = HashSet::from_iter(saved_route_nums);

    let saved_routes = routes
        .into_iter()
        .filter(|r| saved_route_nums.contains(&r.route_number))
        .collect_vec();

    Ok(saved_routes)
}

#[tracing::instrument(err)]
async fn fetch_routes(date: DateTime<Tz>) -> Result<Vec<HzppRoute>, Error> {
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

    let routes: Vec<HzppRoute> = serde_json::from_str(&routes_string)?;

    info!("got {} routes", routes.len());

    Ok(routes)
}

#[tracing::instrument(err)]
async fn fetch_stations() -> Result<Vec<HzppStation>, Error> {
    let request = format!("https://josipsalkovic.com/hzpp/planer/v3/getStops.php");

    let response = reqwest::get(&request)
        .instrument(info_span!("Fetching stations"))
        .await?
        .error_for_status()?;

    let stations_string = response
        .text()
        .instrument(info_span!("Reading body of response"))
        .await?;

    let stations: Vec<HzppStation> =
        serde_json::from_str(&stations_string).context("Error parsing stations")?;

    info!("got {} stations", stations.len());

    Ok(stations)
}

#[cfg(test)]
mod tests {
    use scraper::Selector;

    #[test]
    fn test_stations_html() {
        let html = include_str!("../../documentation/example_responses/prodaja.hzpp.hr.html");
        let html = scraper::Html::parse_document(&html);

        let row_station = html
            .select(&Selector::parse("div .row.station").unwrap())
            .next()
            .unwrap();

        for option_element in row_station
            .select(&Selector::parse("select").unwrap())
            .next()
            .unwrap()
            .select(&Selector::parse("option").unwrap())
        {
            println!("{:?},{:?}", option_element.attr("value").unwrap(), option_element.inner_html());
            return;
        }
    }
}
