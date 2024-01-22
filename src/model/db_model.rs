use anyhow::{anyhow, bail, Error};
use chrono::{DateTime, Days, Timelike, Utc};
use chrono_tz::Tz;
use sqlx::prelude::FromRow;

use super::hzpp_api_model::HzppRoute;

#[derive(Debug, FromRow)]
pub struct RouteDb {
    pub id: String,
    pub route_number: i32,
    /// Usual starting station. Can be incorrect due to exceptions like construction.
    pub usual_source: String,
    /// Usual ending station. Can be incorrect due to exceptions like construction.
    pub usual_destination: String,
    pub bikes_allowed: BikesAllowed,
    pub wheelchair_accessible: WheelchairAccessible,
    pub route_type: RouteType,
    pub real_start_time: Option<DateTime<Utc>>,
    /// The departure time of the first stop
    pub expected_start_time: DateTime<Utc>,
    pub real_end_time: Option<DateTime<Utc>>,
    /// The arrival time of the last stop
    pub expected_end_time: DateTime<Utc>,
}

impl RouteDb {
    pub fn try_from_hzpp_route(value: HzppRoute, date: DateTime<Tz>) -> anyhow::Result<Self> {
        // These shenanigangs are being done cause the API can return a very dumb time like "25:49:00"
        let expected_start_time = value
            .stops
            .first()
            .ok_or_else(|| anyhow!("Unexpected route with 0 stops"))?
            .departure_time;

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

        let expected_end_time = value
            .stops
            .last()
            .ok_or_else(|| anyhow!("Unexpected route with 0 stops"))?
            .arrival_time;

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

        Ok(RouteDb {
            id: value.route_id,
            route_number: value.route_number,
            usual_source: value.route_src,
            usual_destination: value.route_desc,
            bikes_allowed: value.bikes_allowed.try_into()?,
            wheelchair_accessible: value.wheelchair_accessible.try_into()?,
            route_type: value.route_type.try_into()?,
            real_start_time: None,
            expected_start_time: expected_start_time.with_timezone(&Utc),
            real_end_time: None,
            expected_end_time: expected_end_time.with_timezone(&Utc),
        })
    }
}

#[derive(Copy, Clone, Debug, sqlx::Type)]
pub enum BikesAllowed {
    NotAllowed = 0 | 2, // API shenanigans
    Allowed = 1,
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

#[derive(Copy, Clone, Debug)]
pub enum WheelchairAccessible {
    NotAccessible = 0 | 2,
    Accessible = 1,
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

#[derive(Clone, Debug, FromRow)]
pub struct StopDb {
    pub id: String,
    pub station_id: String,
    pub route_id: String,
    pub sequence: i8,
    pub real_arrival: Option<DateTime<Utc>>,
    pub expected_arrival: DateTime<Utc>,
    pub real_departure: Option<DateTime<Utc>>,
    pub expected_departure: DateTime<Utc>,
}

#[derive(Clone, Debug, FromRow)]
pub struct StationDb {
    pub id: String,
    pub code: i32,
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
}
