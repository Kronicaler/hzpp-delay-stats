use serde::{de, Deserialize, Deserializer, Serialize};
use std::str::FromStr;

#[derive(Debug, Deserialize, Serialize)]
pub struct HzppRoute {
    pub route_id: String,
    pub route_number: i32,
    /// Usual starting station. Can be incorrect due to exceptions like construction.
    pub route_src: String,
    pub route_desc: String,
    /// Is completely incorrect
    #[serde(deserialize_with = "timestamp_from_hzpp_time")]
    pub arrival_time: (u8, u8),
    /// Is completely incorrect
    #[serde(deserialize_with = "timestamp_from_hzpp_time")]
    pub departure_time: (u8, u8),
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
    #[serde(deserialize_with = "timestamp_from_hzpp_time")]
    pub arrival_time: (u8, u8),
    #[serde(deserialize_with = "timestamp_from_hzpp_time")]
    pub departure_time: (u8, u8),
    pub latitude: f64,
    pub longitude: f64,
    pub sequence: i32,
}

fn timestamp_from_hzpp_time<'de, D>(deserializer: D) -> Result<(u8, u8), D::Error>
where
    D: Deserializer<'de>,
{
    // These shenanigangs are being done cause the API can return a very dumb time like "25:49:00"
    let s: String = Deserialize::deserialize(deserializer)?;

    let res: anyhow::Result<(u8, u8)> = try {
        let hour: u8 = String::from_str(&s[0..=1])?.parse()?;
        let minute: u8 = String::from_str(&s[3..=4])?.parse()?;

        (hour, minute)
    };

    res.map_err(de::Error::custom)
}
