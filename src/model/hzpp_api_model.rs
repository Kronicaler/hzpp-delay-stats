use std::str::FromStr;

use anyhow::anyhow;
use chrono::{DateTime, TimeZone};
use chrono_tz::{Europe::Zagreb, Tz};
use serde::{de, Deserialize, Deserializer, Serialize};

#[derive(Debug, Deserialize, Serialize)]
pub struct HzppRoute {
    pub route_id: String,
    pub route_number: i32,
    /// Usual starting station. Can be incorrect due to exceptions like construction.
    pub route_src: String,
    pub route_desc: String,
    /// Is completely incorrect
    #[serde(deserialize_with = "datetime_from_naive_time_str")]
    pub arrival_time: DateTime<Tz>,
    /// Is completely incorrect
    #[serde(deserialize_with = "datetime_from_naive_time_str")]
    pub departure_time: DateTime<Tz>,
    /// 1 is true. 0 and 2 are false
    pub bikes_allowed: i32,
    /// 1 is true. 0 and 2 are false
    pub wheelchair_accessible: i32,
    /// 2 is train. 3 is bus.
    pub route_type: i32,
    pub stops: Vec<HzzpStop>,
    pub calendar: Vec<Calendar>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Calendar {
    pub monday: i32,
    pub tuesday: i32,
    pub wednesday: i32,
    pub thursday: i32,
    pub friday: i32,
    pub saturday: i32,
    pub sunday: i32,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct HzzpStop {
    pub stop_id: String,
    pub stop_name: String,
    #[serde(deserialize_with = "datetime_from_naive_time_str")]
    pub arrival_time: DateTime<Tz>,
    #[serde(deserialize_with = "datetime_from_naive_time_str")]
    pub departure_time: DateTime<Tz>,
    pub latitude: f64,
    pub longitude: f64,
    pub sequence: i32,
}

fn datetime_from_naive_time_str<'de, D>(deserializer: D) -> Result<DateTime<Tz>, D::Error>
where
    D: Deserializer<'de>,
{
    // These shenanigangs are being done cause the API can return a very dumb time like "25:49:00"
    let s: String = Deserialize::deserialize(deserializer)?;

    let res: anyhow::Result<DateTime<Tz>> = try {
        let hour: u32 = String::from_str(&s[0..=1])?.parse()?;
        let minute: u32 = String::from_str(&s[3..=4])?.parse()?;
        let second: u32 = String::from_str(&s[6..=7])?.parse()?;
        let day: u32 = hour.saturating_sub(24) + 1;

        Zagreb
            .with_ymd_and_hms(0, 1, day, hour % 24, minute, second)
            .earliest()
            .ok_or_else(|| anyhow!("Error when constructing date"))?
    };

    return res.map_err(de::Error::custom);
}
