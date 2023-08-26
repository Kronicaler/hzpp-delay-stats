use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
pub struct Stop {
    stop_id: String,
    stop_name: String,
    arrival_time: String,
    departure_time: String,
    latitude: f64,
    longitude: f64,
    sequence: i32,
}
