use serde::{Deserialize, Serialize};

use super::{calendar::Calendar, stop::Stop};

#[derive(Debug, Deserialize, Serialize)]
pub struct Route {
    route_i: String,
    route_number: i32,
    route_src: String,
    route_desc: String,
    arrival_time: String,
    departure_time: String,
    bikes_allowed: i32,
    wheelchair_accessible: i32,
    route_type: i32,
    stops: Vec<Stop>,
    calendar: Vec<Calendar>,
}