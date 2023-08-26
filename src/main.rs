use std::fs;

use anyhow::Result;
use regex::Regex;
use thiserror::Error;

use crate::model::route::Route;

mod model;

fn main() -> Result<()> {
    let routes: Vec<Route> =
        serde_json::from_str(&fs::read_to_string("example_responses/routes.json")?)?;
    let delay_html = fs::read_to_string("example_responses/delay.html")?;

    if delay_html.contains("Vlak je redovit") {
        println!("The train is running on time");
    } else {
        let late_regex = Regex::new("Kasni . min.")?;

        let capture: i32 = late_regex
            .captures(&delay_html)
            .ok_or(HzppError::CouldntFindLateness)?[0]
            .to_owned()
            .trim()
            .split(" ")
            .collect::<Vec<_>>()[1]
            .to_owned()
            .parse()?;

        println!("The train is {capture} minutes late");
    }

    return Ok(());
}

#[derive(Error, Debug)]
pub enum HzppError {
    #[error("Couldn't find how late the train was in the returned html file")]
    CouldntFindLateness,
}
