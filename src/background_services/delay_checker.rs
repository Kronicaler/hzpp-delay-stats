use std::time::Duration;

use anyhow::bail;
use chrono::Utc;
use itertools::Itertools;
use tokio::{spawn, sync::mpsc::Receiver, time::sleep};
use tracing::{info, info_span, Instrument};

use crate::model::db_model::RouteDb;

pub async fn check_delays(
    delay_checker_receiver: &mut Receiver<Vec<RouteDb>>,
) -> anyhow::Result<()> {
    let mut buffer: Vec<Vec<RouteDb>> = vec![];

    while delay_checker_receiver.recv_many(&mut buffer, 32).await != 0 {
        let routes = buffer.drain(..).flatten().collect_vec();

        for mut route in routes {
            spawn(
                async move {
                    let secs_until_start =
                        route.expected_start_time.timestamp() - Utc::now().timestamp();

                    info!(
                        "Checking delays for route {:#?} starting in {}",
                        route, secs_until_start
                    );

                    if secs_until_start < 0 {
                        bail!("Got route in the past, this shouldn't happen");
                    }

                    sleep(Duration::from_secs(secs_until_start.try_into()?))
                        .instrument(info_span!("Waiting for route to start"))
                        .await;

                    loop {
                        let delay: Delay = get_route_delay(&route)?;

                        match delay {
                            Delay::WaitingForDeparture => {}
                            Delay::OnTime => {
                                if route.real_start_time.is_none() {
                                    route.real_start_time = Some(Utc::now());
                                    update_route(&route)?;
                                }
                            }
                            Delay::Late { minutes_late: _ } => {
                                if route.real_start_time.is_none() {
                                    route.real_start_time = Some(Utc::now());
                                    update_route(&route)?;
                                }
                            }
                            Delay::Finished { minutes_late: _ } => {
                                route.real_end_time = Some(Utc::now());
                                update_route(&route)?;
                                return Ok(());
                            }
                        };

                        sleep(Duration::from_secs(60))
                            .instrument(info_span!("Waiting 60 seconds"))
                            .await;
                    }
                }
                .instrument(info_span!("Checking delay")),
            );
        }
    }

    info!("Channel closed");

    Ok(())
}

#[tracing::instrument(err)]
fn update_route(route: &RouteDb) -> anyhow::Result<()> {
    todo!()
}

#[tracing::instrument(err)]
fn get_route_delay(route: &RouteDb) -> anyhow::Result<Delay> {
    todo!()
}

#[derive(Copy, Clone, Debug)]
enum Delay {
    WaitingForDeparture,
    OnTime,
    Late { minutes_late: i32 },
    Finished { minutes_late: i32 },
}
