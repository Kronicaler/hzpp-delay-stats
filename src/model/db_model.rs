use anyhow::{anyhow, bail, Context, Error};
use chrono::{DateTime, Days, Timelike, Utc};
use chrono_tz::Tz;
use derivative::Derivative;
use itertools::Itertools;
use sqlx::prelude::FromRow;
use tracing::error;

use super::hzpp_api_model::{HzppRoute, HzppStation, HzppStop};

#[derive(Derivative)]
#[derivative(Debug)]
#[derive(FromRow)]
pub struct RouteDb {
    pub id: String,
    pub route_number: i32,
    pub source: String,
    pub destination: String,
    #[sqlx(try_from = "i16")]
    pub bikes_allowed: BikesAllowed,
    #[sqlx(try_from = "i16")]
    pub wheelchair_accessible: WheelchairAccessible,
    #[sqlx(try_from = "i16")]
    pub route_type: RouteType,
    pub real_start_time: Option<DateTime<Utc>>,
    /// The departure time of the first stop
    pub expected_start_time: DateTime<Utc>,
    pub real_end_time: Option<DateTime<Utc>>,
    /// The arrival time of the last stop
    pub expected_end_time: DateTime<Utc>,
    #[derivative(Debug = "ignore")]
    #[sqlx(skip)]
    pub stops: Vec<StopDb>,
}

impl RouteDb {
    pub fn try_from_hzpp_route(hzpp_route: HzppRoute, date: DateTime<Tz>) -> anyhow::Result<Self> {
        let first_stop = hzpp_route
            .stops
            .first()
            .ok_or_else(|| anyhow!("Unexpected route with 0 stops"))?;

        let last_stop = hzpp_route
            .stops
            .last()
            .ok_or_else(|| anyhow!("Unexpected route with 0 stops"))?;

        // These shenanigangs are being done cause the API can return a very dumb time like "25:49:00"
        let expected_start_time = first_stop.departure_time;
        let expected_end_time = last_stop.arrival_time;

        let (expected_start_time, expected_end_time) =
            convert_hzpp_time_to_utc(&date, expected_start_time, expected_end_time)?;

        let (stops, errors): (Vec<_>, Vec<_>) = hzpp_route
            .stops
            .iter()
            .map(|s| StopDb::try_from_hzpp_stop(s.clone(), &hzpp_route, &date))
            .partition_result();

        if errors.len() != 0 {
            errors
                .iter()
                .for_each(|e| error!("Error turning HzppStop to StopDb: {}", e));
            bail!("Error turning HzppStop to StopDb");
        }

        Ok(RouteDb {
            id: hzpp_route.route_id,
            route_number: hzpp_route.route_number,
            source: first_stop.stop_name.clone(),
            destination: last_stop.stop_name.clone(),
            bikes_allowed: hzpp_route.bikes_allowed.try_into()?,
            wheelchair_accessible: hzpp_route.wheelchair_accessible.try_into()?,
            route_type: hzpp_route.route_type.try_into()?,
            real_start_time: None,
            expected_start_time: expected_start_time.with_timezone(&Utc),
            real_end_time: None,
            expected_end_time: expected_end_time.with_timezone(&Utc),
            stops,
        })
    }
}

fn convert_hzpp_time_to_utc(
    date: &DateTime<Tz>,
    expected_start_time: (u8, u8),
    expected_end_time: (u8, u8),
) -> Result<(DateTime<Tz>, DateTime<Tz>), Error> {
    let expected_start_time = date
        .checked_add_days(Days::new(expected_start_time.0 as u64 / 24))
        .ok_or_else(|| anyhow!("invalid start time day"))?
        .with_hour(expected_start_time.0 as u32 % 24)
        .ok_or_else(|| anyhow!("invalid start time hour"))?
        .with_minute(expected_start_time.1.into())
        .ok_or_else(|| anyhow!("invalid start time minute"))?
        .with_second(0)
        .ok_or_else(|| anyhow!("invalid start time second"))?
        .with_nanosecond(0)
        .ok_or_else(|| anyhow!("invalid end time nanosecond"))?;
    let expected_end_time = date
        .checked_add_days(Days::new(expected_end_time.0 as u64 / 24))
        .ok_or_else(|| anyhow!("invalid end time day"))?
        .with_hour(expected_end_time.0 as u32 % 24)
        .ok_or_else(|| anyhow!("invalid end time hour"))?
        .with_minute(expected_end_time.1.into())
        .ok_or_else(|| anyhow!("invalid end time minute"))?
        .with_second(0)
        .ok_or_else(|| anyhow!("invalid end time second"))?
        .with_nanosecond(0)
        .ok_or_else(|| anyhow!("invalid end time nanosecond"))?;
    Ok((expected_start_time, expected_end_time))
}

#[derive(Copy, Clone, Debug)]
pub enum BikesAllowed {
    Allowed = 1,
    NotAllowed = 2, // API shenanigans
}

impl TryFrom<i32> for BikesAllowed {
    type Error = Error;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 | 2 => Ok(BikesAllowed::NotAllowed),
            1 => Ok(BikesAllowed::Allowed),
            _ => bail!("Got wrong value when trying to convert u8 {value} to BikesAllowed"),
        }
    }
}

impl TryFrom<i16> for BikesAllowed {
    type Error = Error;

    fn try_from(value: i16) -> Result<Self, Self::Error> {
        match value {
            0 | 2 => Ok(BikesAllowed::NotAllowed),
            1 => Ok(BikesAllowed::Allowed),
            _ => bail!("Got wrong value when trying to convert u8 {value} to BikesAllowed"),
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub enum WheelchairAccessible {
    Accessible = 1,
    NotAccessible = 2,
}

#[derive(Copy, Clone, Debug)]
pub enum RouteType {
    Train = 2,
    Bus = 3,
}

impl TryFrom<i32> for RouteType {
    type Error = Error;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            2 => Ok(RouteType::Train),
            3 => Ok(RouteType::Bus),
            _ => bail!("Got wrong value when trying to convert u8 {value} to BikesAllowed"),
        }
    }
}

impl TryFrom<i16> for RouteType {
    type Error = Error;

    fn try_from(value: i16) -> Result<Self, Self::Error> {
        match value {
            2 => Ok(RouteType::Train),
            3 => Ok(RouteType::Bus),
            _ => bail!("Got wrong value when trying to convert u8 {value} to BikesAllowed"),
        }
    }
}

impl TryFrom<i32> for WheelchairAccessible {
    type Error = Error;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 | 2 => Ok(WheelchairAccessible::NotAccessible),
            1 => Ok(WheelchairAccessible::Accessible),
            _ => bail!("Got wrong value when trying to convert u8 {value} to WheelchairAccessible"),
        }
    }
}

impl TryFrom<i16> for WheelchairAccessible {
    type Error = Error;

    fn try_from(value: i16) -> Result<Self, Self::Error> {
        match value {
            0 | 2 => Ok(WheelchairAccessible::NotAccessible),
            1 => Ok(WheelchairAccessible::Accessible),
            _ => bail!("Got wrong value when trying to convert u8 {value} to WheelchairAccessible"),
        }
    }
}

#[derive(Debug, FromRow)]
pub struct StopDb {
    pub station_id: String,
    pub route_id: String,
    pub route_expected_start_time: DateTime<Utc>,
    pub sequence: i16,
    pub real_arrival: Option<DateTime<Utc>>,
    pub expected_arrival: DateTime<Utc>,
    pub real_departure: Option<DateTime<Utc>>,
    pub expected_departure: DateTime<Utc>,
}

impl StopDb {
    fn try_from_hzpp_stop(
        hzpp_stop: HzppStop,
        hzpp_route: &HzppRoute,
        date: &DateTime<Tz>,
    ) -> Result<Self, anyhow::Error> {
        let (expected_arrival, expected_departure) =
            convert_hzpp_time_to_utc(&date, hzpp_stop.arrival_time, hzpp_stop.departure_time)?;
        let (route_expected_start_time, _) = convert_hzpp_time_to_utc(
            &date,
            hzpp_route
                .stops
                .first()
                .context("route doesn't contain any stops")?
                .departure_time,
            hzpp_route
                .stops
                .last()
                .context("route doesn't contain any stops")?
                .arrival_time,
        )?;

        Ok(StopDb {
            station_id: hzpp_stop.stop_id,
            route_id: hzpp_route.route_id.clone(),
            route_expected_start_time: route_expected_start_time.to_utc(),
            sequence: hzpp_stop.sequence.try_into()?,
            real_arrival: None,
            expected_arrival: expected_arrival.to_utc(),
            real_departure: None,
            expected_departure: expected_departure.to_utc(),
        })
    }
}

#[derive(Debug, FromRow)]
pub struct StationDb {
    pub id: String,
    pub code: i32,
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
}

impl From<HzppStation> for StationDb {
    fn from(s: HzppStation) -> Self {
        StationDb {
            code: s.stop_code,
            id: s.stop_id,
            latitude: s.stop_lat,
            longitude: s.stop_lng,
            name: s.stop_name,
        }
    }
}

impl std::hash::Hash for StationDb {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
        self.code.hash(state);
        self.name.hash(state);
    }
}
