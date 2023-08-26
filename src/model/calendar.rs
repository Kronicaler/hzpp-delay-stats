use serde::{Deserialize, Serialize};

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
