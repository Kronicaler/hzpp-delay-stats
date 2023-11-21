use chrono::{DateTime, Utc};
use sqlx::prelude::FromRow;

#[derive(Debug, FromRow)]
pub struct Route {
    pub id: String,
    pub route_number: i32,
    /// Usual starting station. Can be incorrect due to exceptions like construction.
    pub usual_source: String,
    pub destination: String,
    /// 1 is true. 0 and 2 are false
    pub bikes_allowed: u8, // TODO turn into enum
    /// 1 is true. 0 and 2 are false
    pub wheelchair_accessible: u8, // TODO turn into enum
    /// 2 is train. 3 is bus.
    pub route_type: u8, // TODO turn into enum
    pub real_start_time: Option<DateTime<Utc>>,
    // Should this be the departure time of the first stop?
    pub expected_start_time: DateTime<Utc>,
    pub real_end_time: Option<DateTime<Utc>>,
    // Should this be the arrival time of the last stop?
    pub expected_end_time: DateTime<Utc>,
}

#[derive(Debug, FromRow)]
pub struct Stop {
    pub id: String,
    pub station_id: String,
    pub route_id: String,
    pub sequence: i8,
    pub real_arrival: Option<DateTime<Utc>>,
    pub expected_arrival: DateTime<Utc>,
    pub real_departure: Option<DateTime<Utc>>,
    pub expected_departure: DateTime<Utc>,
}

#[derive(Debug, FromRow)]
pub struct Station {
    pub id: String,
    pub code: i32,
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
}
