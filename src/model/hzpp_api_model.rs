use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
pub struct HzppRoute {
    route_id: String,
    route_number: i32,
    /// Usual starting station. Can be incorrect due to exceptions like construction.
    route_src: String,
    route_desc: String,
    /// Is completely incorrect
    arrival_time: String,
    /// Is completely incorrect
    departure_time: String,
    /// 1 is true. 0 and 2 are false
    bikes_allowed: i32,
    /// 1 is true. 0 and 2 are false
    wheelchair_accessible: i32,
    /// 2 is train. 3 is bus.
    route_type: i32,
    stops: Vec<HzzpStop>,
    calendar: Vec<Calendar>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Calendar {
    monday: i32,
    tuesday: i32,
    wednesday: i32,
    thursday: i32,
    friday: i32,
    saturday: i32,
    sunday: i32,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct HzzpStop {
    stop_id: String,
    stop_name: String,
    arrival_time: String,
    departure_time: String,
    latitude: f64,
    longitude: f64,
    sequence: i32,
}
