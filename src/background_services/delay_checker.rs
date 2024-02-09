//! Responsible for checking the delays of routes gotten from the route_fetcher

use std::{collections::HashMap, hash::RandomState, time::Duration, vec};

use anyhow::{anyhow, bail, Context};
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use chrono_tz::Europe::Zagreb;
use itertools::Itertools;
use reqwest::{header::HeaderValue, Client, Method, Url};
use sqlx::{query, query_as, Pool, Postgres};
use tokio::{spawn, sync::mpsc::Receiver, task::JoinSet, time::sleep};
use tracing::{error, info, info_span, Instrument};

use crate::{
    model::db_model::{RouteDb, StationDb, StopDb},
    utils::str_between_str,
};

/// Checks the delays of the routes from the given channel and saves them to the DB
pub async fn check_delays(
    delay_checker_receiver: &mut Receiver<Vec<RouteDb>>,
    pool: &Pool<Postgres>,
) -> Result<(), anyhow::Error> {
    let mut buffer: Vec<Vec<RouteDb>> = vec![];

    let pool1 = pool.clone();
    spawn(
        async move {
            let unfinished_routes = match get_unfinished_routes(&pool1).await {
                Ok(r) => r,
                Err(e) => {
                    error!("{:?}", e);
                    return;
                }
            };
            spawn_route_delay_tasks(unfinished_routes, &pool1).await;
        }
        .instrument(info_span!("spawn_unfinished_route_tasks")),
    );

    while delay_checker_receiver.recv_many(&mut buffer, 32).await != 0 {
        let routes = buffer.drain(..).flatten().collect_vec();

        spawn_route_delay_tasks(routes, pool).await;
    }

    info!("Channel closed");

    Ok(())
}

async fn spawn_route_delay_tasks(routes: Vec<RouteDb>, pool: &Pool<Postgres>) {
    for route in routes {
        let secs_until_end = route.expected_end_time.timestamp() - Utc::now().timestamp();
        if secs_until_end < 0 {
            info!("Got route in the past, discarding it");

            continue;
        }

        let delay_pool = pool.clone();
        spawn(monitor_route(route, delay_pool));
    }
}

#[tracing::instrument(err, skip(pool))]
async fn get_unfinished_routes(pool: &Pool<Postgres>) -> Result<Vec<RouteDb>, anyhow::Error> {
    let routes: Vec<RouteDb> = query_as(
        "SELECT 
        id,
        route_number,
        source,
        destination,
        bikes_allowed,
        wheelchair_accessible,
        route_type,
        expected_start_time,
        expected_end_time,
        real_start_time,
        real_end_time
        from routes where real_end_time IS NULL or real_start_time IS NULL",
    )
    .fetch_all(pool)
    .await?;

    let mut set = JoinSet::new();

    for mut route in routes.into_iter() {
        let pool = pool.clone();
        set.spawn(async move {
            let stops: Vec<StopDb> = query_as!(
                StopDb,
                "SELECT * from stops where route_id = $1 and route_expected_start_time = $2",
                route.id.clone(),
                route.expected_start_time
            )
            .fetch_all(&pool)
            .await?;

            route.stops = stops;

            Ok::<RouteDb, anyhow::Error>(route)
        });
    }

    let mut routes = vec![];

    while let Some(res) = set.join_next().await {
        let route = res??;
        routes.push(route);
    }

    Ok(routes)
}

async fn monitor_route(route: RouteDb, pool: Pool<Postgres>) -> Result<(), anyhow::Error> {
    let secs_until_start = route.expected_start_time.timestamp() - Utc::now().timestamp();
    let secs_until_end = route.expected_end_time.timestamp() - Utc::now().timestamp();

    info!(
        "Monitoring route {:#?} starting in {}",
        route, secs_until_start
    );

    if secs_until_end < 0 {
        info!("Got route in the past, discarding it");
        return Ok(());
    }

    info!("Waiting {} seconds for route to start", secs_until_start);
    sleep(Duration::from_secs(
        secs_until_start.try_into().unwrap_or(0),
    ))
    .await;

    check_delay_until_route_completion(route, pool).await?;

    Ok(())
}

#[tracing::instrument(err, fields(route_number=route.route_number))]
async fn check_delay_until_route_completion(
    mut route: RouteDb,
    pool: Pool<Postgres>,
) -> Result<(), anyhow::Error> {
    let mut train_has_started = route.real_start_time.is_some();
    let stations: HashMap<String, StationDb, RandomState> = HashMap::from_iter(
        get_stations(pool.clone())
            .await?
            .into_iter()
            .map(|s| (s.id.clone(), s)),
    );

    loop {
        sleep(Duration::from_secs(60))
            .instrument(info_span!("Waiting 60 seconds"))
            .await;

        if Utc::now() > route.expected_end_time + chrono::Duration::hours(12) {
            info!(train_timeout = true);

            if route.real_start_time.is_none() {
                info!(train_never_started = true);
            }

            if route.real_end_time.is_none() {
                info!(train_never_finished = true);
            }

            return Ok(());
        }

        let status: TrainStatus = match get_route_status(&route).await {
            Ok(dr) => match dr {
                StatusResponse::TrainStatus(ts) => ts,
                StatusResponse::TrainNotEvidented => {
                    info!(train_not_evidented = true);
                    return Ok(());
                }
                StatusResponse::UnisysError => {
                    info!(unisys_error = true);
                    continue;
                }
            },
            Err(err) => {
                error!("{:?}", err.context("error fetching delay"));
                continue;
            }
        };

        let minutes_late = status.get_minutes_late();

        if minutes_late.is_none() {
            continue;
        }
        let minutes_late = minutes_late.unwrap();

        match status.status {
            Status::Formed(_) => {
                continue;
            }
            Status::DepartingFromStation(_) => {
                if route.real_start_time.is_none() {
                    train_has_started = true;
                    route.real_start_time = Some(
                        route.expected_start_time + chrono::Duration::minutes(minutes_late.into()),
                    );
                    update_route_real_times(&route, &pool).await?;
                }

                let current_stop = get_current_stop(&mut route.stops, &stations, &status);
                info!(current_stop= ?current_stop);
                if let Some(current_stop) = current_stop {
                    if current_stop.real_departure.is_none() {
                        current_stop.real_departure = Some(
                            current_stop.expected_departure
                                + chrono::Duration::minutes(minutes_late.into()),
                        );
                        update_stop_departure(
                            current_stop,
                            route.expected_start_time,
                            &route.id,
                            pool.clone(),
                        )
                        .await?;
                    }
                }
            }
            Status::Arriving(_) => {
                if route.real_start_time.is_none() {
                    train_has_started = true;
                    route.real_start_time = Some(
                        route.expected_start_time + chrono::Duration::minutes(minutes_late.into()),
                    );
                    update_route_real_times(&route, &pool).await?;
                }

                let current_stop = get_current_stop(&mut route.stops, &stations, &status);
                if let Some(current_stop) = current_stop {
                    if current_stop.real_arrival.is_none() {
                        current_stop.real_arrival = Some(
                            current_stop.expected_arrival
                                + chrono::Duration::minutes(minutes_late.into()),
                        );
                        update_stop_arrival(
                            current_stop,
                            route.expected_start_time,
                            &route.id,
                            pool.clone(),
                        )
                        .await?;
                    }
                }
            }
            Status::FinishedDriving(datetime) => {
                if datetime < Utc::now() - chrono::Duration::hours(12) {
                    continue;
                }

                let current_stop = get_current_stop(&mut route.stops, &stations, &status);
                if let Some(current_stop) = current_stop {
                    if current_stop.real_arrival.is_none() {
                        current_stop.real_arrival = Some(
                            current_stop.expected_arrival
                                + chrono::Duration::minutes(minutes_late.into()),
                        );
                        update_stop_arrival(
                            current_stop,
                            route.expected_start_time,
                            &route.id,
                            pool.clone(),
                        )
                        .await?;
                    }
                }

                if train_has_started {
                    route.real_end_time = Some(
                        route.expected_end_time + chrono::Duration::minutes(minutes_late.into()),
                    );
                    update_route_real_times(&route, &pool).await?;
                    return Ok(());
                }
            }
        }
    }
}

#[tracing::instrument(skip(stops, stations))]
fn get_current_stop<'a>(
    stops: &'a mut Vec<StopDb>,
    stations: &HashMap<String, StationDb>,
    status: &TrainStatus,
) -> Option<&'a mut StopDb> {
    let mut current_stops = stops
        .iter_mut()
        .filter_map(|s| {
            let station = match stations.get(&s.station_id) {
                Some(s) => s,
                None => {
                    error!("Got unknown stop id");
                    return None;
                }
            };

            match is_delay_station_similar_to_stop_name(&status.station, &station.name) {
                Ok(r) => match r {
                    true => return Some(s),
                    false => None,
                },
                Err(e) => {
                    error!("{:?}", e);
                    return None;
                }
            }
        })
        .collect_vec();

    let current_stop = current_stops.pop();
    info!(current_stop= ?current_stop);
    current_stop
}

async fn get_stations(pool: Pool<Postgres>) -> Result<Vec<StationDb>, anyhow::Error> {
    let res = query_as!(StationDb, "Select * from stations")
        .fetch_all(&pool)
        .await?;

    return Ok(res);
}

fn is_delay_station_similar_to_stop_name(
    delay_station_name: &str,
    stop_name: &str,
) -> Result<bool, anyhow::Error> {
    let binding = delay_station_name.to_lowercase().replace(".", "");
    let delay_station_words = binding.trim().split(" ").collect_vec();

    let binding = stop_name.to_lowercase().replace(".", "");
    let stop_words = binding.trim().split(" ").collect_vec();

    if delay_station_words.len() != stop_words.len() {
        return Ok(false);
    }

    let are_all_words_similar = delay_station_words
        .iter()
        .zip(stop_words.iter())
        .map(|(w1, w2)| w1.contains(w2) || w2.contains(w1))
        .reduce(|acc, e| acc && e)
        .context("empty iterator???")?;

    return Ok(are_all_words_similar);
}

#[tracing::instrument(err)]
async fn update_stop_arrival(
    stop: &StopDb,
    route_expected_start_time: DateTime<Utc>,
    route_id: &str,
    pool: Pool<Postgres>,
) -> Result<(), anyhow::Error> {
    query!(
        "
    UPDATE stops
    SET real_arrival = $1
    where route_expected_start_time = $2 and route_id = $3 and sequence = $4
    ",
        stop.real_arrival,
        route_expected_start_time,
        route_id,
        stop.sequence,
    )
    .execute(&pool)
    .await?;

    Ok(())
}

#[tracing::instrument(err)]
async fn update_stop_departure(
    stop: &StopDb,
    route_expected_start_time: DateTime<Utc>,
    route_id: &str,
    pool: Pool<Postgres>,
) -> Result<(), anyhow::Error> {
    query!(
        "
UPDATE stops
SET real_departure = $1
where route_expected_start_time = $2 and route_id = $3 and sequence = $4
",
        stop.real_departure,
        route_expected_start_time,
        route_id,
        stop.sequence,
    )
    .execute(&pool)
    .await?;

    Ok(())
}

#[tracing::instrument(ret, err)]
async fn get_route_status(route: &RouteDb) -> Result<StatusResponse, anyhow::Error> {
    let url = format!(
        "https://traindelay.hzpp.hr/train/delay?trainId={}",
        route.route_number
    );

    let mut request =
        reqwest::Request::new(Method::GET, Url::parse(&url).context("Couldn't parse url")?);
    request.headers_mut().append("Authorization", 
    HeaderValue::from_static("Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJoenBwLXBsYW5lciIsImlhdCI6MTY3NDEzNzM3NH0.a6FzxGKyUfHzLVuGP242MFWF6EspvJl1LTHwEVeMIsY"));

    let response = Client::new()
        .execute(request)
        .instrument(info_span!("Fetching delay"))
        .await
        .context("error fetching html")?
        .error_for_status()
        .context("error status fetching html")?;

    let content = response
        .text()
        .instrument(info_span!("Reading body of response"))
        .await
        .context("Error getting response text")?;

    let delay_response = parse_delay_html(content)?;

    Ok(delay_response)
}

#[tracing::instrument(ret, err)]
fn parse_delay_html(html: String) -> Result<StatusResponse, anyhow::Error> {
    if html.contains("Unisys") {
        return Ok(StatusResponse::UnisysError);
    }

    if html.contains("Vlak nije u evidenciji") {
        return Ok(StatusResponse::TrainNotEvidented);
    }

    let lines = html.lines().collect_vec();

    let station_line = *lines
        .iter()
        .filter(|l| l.contains("Kolodvor:"))
        .collect_vec()
        .first()
        .ok_or_else(|| anyhow!("Couldn't locate station line"))?;

    let station = str_between_str(station_line, "</I><strong>", "<br>")
        .ok_or_else(|| anyhow!("Couldn't locate station"))?
        .to_string()
        .replace("+", " ");

    let status_line = *lines
        .iter()
        .enumerate()
        .filter(|l| {
            l.1.contains("Završio")
                || l.1.contains("Odlazak")
                || l.1.contains("Formiran")
                || l.1.contains("Dolazak")
        })
        .collect_vec()
        .first()
        .ok_or_else(|| anyhow!("Couldn't locate status line"))?;
    let status_time_line = lines
        .get(status_line.0 + 1)
        .ok_or_else(|| anyhow!("couldn't locate status time line"))?;

    let status_date = NaiveDate::parse_from_str(&status_time_line[..9], "%d.%m.%y.")
        .context("Couldn't parse status_date")?;
    let status_time = NaiveTime::parse_from_str(&status_time_line[12..17], "%H:%M")
        .context("Couldn't parse status_time")?;
    let status_datetime: DateTime<Utc> = status_date
        .and_time(status_time)
        .and_local_timezone(Zagreb)
        .earliest()
        .ok_or_else(|| anyhow!("invalid date"))?
        .with_timezone(&Utc);

    let status = match status_line {
        ref sl if sl.1.contains("Završio") => Status::FinishedDriving(status_datetime),
        ref sl if sl.1.contains("Odlazak") => Status::DepartingFromStation(status_datetime),
        ref sl if sl.1.contains("Formiran") => Status::Formed(status_datetime),
        ref sl if sl.1.contains("Dolazak") => Status::Arriving(status_datetime),
        _ => return Err(anyhow!("Couldn't construct status"))?,
    };

    let delay = if html.contains("Kasni") {
        let minutes_late: i32 = str_between_str(&html, "Kasni", "min.")
            .ok_or_else(|| anyhow!("Couldn't find delay number"))?
            .trim()
            .parse()
            .context("Couldn't parse delay number")?;
        Delay::Late { minutes_late }
    } else if html.contains("Vlak ceka polazak") {
        Delay::WaitingToDepart
    } else if html.contains("Vlak je redovit") {
        Delay::OnTime
    } else if lines
        .get(20)
        .ok_or_else(|| anyhow!("couldn't find delay line"))?
        .contains("<BLINK>                                                  </BLINK>")
    {
        Delay::NoData
    } else {
        bail!("Unknown delay response");
    };

    Ok(StatusResponse::TrainStatus(TrainStatus {
        delay,
        station,
        status,
    }))
}

#[derive(Clone, Debug)]
enum StatusResponse {
    TrainStatus(TrainStatus),
    TrainNotEvidented,
    UnisysError,
}

#[derive(Clone, Debug)]
struct TrainStatus {
    pub station: String,
    pub status: Status,
    pub delay: Delay,
}

#[derive(Copy, Clone, Debug)]
enum Delay {
    NoData,
    WaitingToDepart,
    OnTime,
    Late { minutes_late: i32 },
}

#[derive(Copy, Clone, Debug)]
enum Status {
    Formed(DateTime<Utc>),
    DepartingFromStation(DateTime<Utc>),
    Arriving(DateTime<Utc>),
    FinishedDriving(DateTime<Utc>),
}

impl TrainStatus{
    pub fn get_minutes_late(&self) -> Option<i32> {
        match self.delay {
            Delay::NoData => None,
            Delay::WaitingToDepart => None,
            Delay::OnTime => Some(0),
            Delay::Late { minutes_late } => Some(minutes_late),
        }
    }
}

#[tracing::instrument(err)]
async fn update_route_real_times(
    route: &RouteDb,
    pool: &Pool<Postgres>,
) -> Result<(), anyhow::Error> {
    query!(
        "
    UPDATE routes
    SET real_start_time = $1, real_end_time=$2
    where expected_start_time = $3 and id = $4
    ",
        route.real_start_time,
        route.real_end_time,
        route.expected_start_time,
        route.id
    )
    .execute(pool)
    .await?;

    Ok(())
}

mod tests {

    #[test]
    fn is_delay_station_similar_to_stop_name_test1() {
        let str1 = "SV. IVAN ŽABNO";
        let str2 = "SVeti IVAN ŽABNO";

        assert!(super::is_delay_station_similar_to_stop_name(str1, str2).unwrap());
    }

    #[test]
    fn is_delay_station_similar_to_stop_name_test2() {
        let str1 = "Zagreb GL. Kol.";
        let str2 = "Zagreb Glavni kolodvor";

        assert!(super::is_delay_station_similar_to_stop_name(str1, str2).unwrap());
    }

    #[test]
    fn is_delay_station_similar_to_stop_name_test3() {
        let str1 = "Zagreb GL. Kol.";
        let str2 = "Zagreb zapadni kolodvor";

        assert!(!super::is_delay_station_similar_to_stop_name(str1, str2).unwrap());
    }

    #[test]
    fn is_delay_station_similar_to_stop_name_test4() {
        let str1 = "Novska";
        let str2 = "NOVSKA";

        assert!(super::is_delay_station_similar_to_stop_name(str1, str2).unwrap());
    }

    #[test]
    fn test_parse_html() -> Result<(), anyhow::Error> {
        let html = r##"<HTML>
<HEAD>
<TITLE>Trenutna pozicija vlaka</TITLE>
<meta name="viewport" content="width=device-width, initial-scale=1.0" charset=windows-1250">
</HEAD>
<BODY BACKGROUND=Images/slika.jpg><TABLE align="CENTER"><TR>
<TD><FONT COLOR="#333399"><FONT FACE=Verdana,Arial,Helvetica COLOR="#333399">
<H3 ALIGN=center>HŽ Infrastruktura<BR>                                  </H3></FONT>
</TR></TABLE>
<HR>
<FORM METHOD="GET" ACTION="http://10.215.0.117/hzinfo/Default.asp?">
<P ALIGN=CENTER>
<FONT SIZE=6 FACE=Arial,Helvetica COLOR="#333399">
<TABLE ALIGN=CENETR WIDTH=110%>
<TD BGCOLOR=#bbddff><I>Trenutna pozicija<br>vlak: </I>  8067 <br>
Relacija:<br> SAVSKI-MAR>DUGO-SELO- </strong></TD><TR>
<TD BGCOLOR=#bbddff><I>Kolodvor: </I><strong>DUGO+SELO<br> </TD><TR>
<TD BGCOLOR=#bbddff><I>Završio vožnju      </I><cr>
26.01.24. u 18:58 sati</TD><TR>
<TD><FONT FACE=Arial,Helvetica COLOR=#ff00b0>
Vlak je redovit                                   <BR>
<FONT SIZE=4 FACE=Verdana,Arial,Helvetica COLOR="#333399">
 <BR>
</TD><TR><TD>
</TD></TABLE><HR><FONT SIZE=1 FACE=Arial,Helvetica COLOR=009FFF>
Stanje vlaka od 26/01/24   u 23:33   <HR>
<INPUT TYPE="HIDDEN" NAME="Category" VALUE="hzinfo">
<INPUT TYPE="HIDDEN" NAME="Service" VALUE="tpvl">
<INPUT TYPE="HIDDEN" NAME="SCREEN" VALUE="1">
<INPUT TYPE="SUBMIT" VALUE="Povrat">
</FORM>
</BODY>
</HTML>"##;

        super::parse_delay_html(html.to_string())?;

        Ok(())
    }

    #[test]
    fn test_parse_html2() -> Result<(), anyhow::Error> {
        let html2 = r##"<HTML>
<HEAD>
<TITLE>Trenutna pozicija vlaka</TITLE>
<meta name="viewport" content="width=device-width, initial-scale=1.0" charset=windows-1250">
</HEAD>
<BODY BACKGROUND=Images/slika.jpg><TABLE align="CENTER"><TR>
<TD><FONT COLOR="#333399"><FONT FACE=Verdana,Arial,Helvetica COLOR="#333399">
<H3 ALIGN=center>HŽ Infrastruktura<BR>                                  </H3></FONT>
</TR></TABLE>
<HR>
<FORM METHOD="GET" ACTION="http://10.215.0.117/hzinfo/Default.asp?">
<P ALIGN=CENTER>
<FONT SIZE=6 FACE=Arial,Helvetica COLOR="#333399">
<TABLE ALIGN=CENETR WIDTH=110%>
<TD BGCOLOR=#bbddff><I>Trenutna pozicija<br>vlak: </I>  2303 <br>
Relacija:<br>  >  </strong></TD><TR>
<TD BGCOLOR=#bbddff><I>Kolodvor: </I><strong>SV.+IVAN+ŽABNO<br> </TD><TR>
<TD BGCOLOR=#bbddff><I>Odlazak  </I><cr>
27.01.24. u 00:03 sati</TD><TR>
<TD><FONT FACE=Arial,Helvetica COLOR=#ff00b0>
Vlak je redovit                                   <BR>
<FONT SIZE=4 FACE=Verdana,Arial,Helvetica COLOR="#333399">
<predv><BR>
</TD><TR><TD>
</TD></TABLE><HR><FONT SIZE=1 FACE=Arial,Helvetica COLOR=009FFF>
Stanje vlaka od 27/01/24   u 00:09   <HR>
<INPUT TYPE="HIDDEN" NAME="Category" VALUE="hzinfo">
<INPUT TYPE="HIDDEN" NAME="Service" VALUE="tpvl">
<INPUT TYPE="HIDDEN" NAME="SCREEN" VALUE="1">
<INPUT TYPE="SUBMIT" VALUE="Povrat">
</FORM>
</BODY>
</HTML>
"##;

        super::parse_delay_html(html2.to_string())?;

        Ok(())
    }

    #[test]
    fn test_parse_html3() -> Result<(), anyhow::Error> {
        let html3 = r##"<HTML>
<HEAD>
<TITLE>Trenutna pozicija vlaka</TITLE>
<meta name="viewport" content="width=device-width, initial-scale=1.0" charset=windows-1250">
</HEAD>
<BODY BACKGROUND=Images/slika.jpg><TABLE align="CENTER"><TR>
<TD><FONT COLOR="#333399"><FONT FACE=Verdana,Arial,Helvetica COLOR="#333399">
<H3 ALIGN=center>HŽ Infrastruktura<BR>                                  </H3></FONT>
</TR></TABLE>
<HR>
<FORM METHOD="GET" ACTION="http://10.215.0.117/hzinfo/Default.asp?">
<P ALIGN=CENTER>
<FONT SIZE=6 FACE=Arial,Helvetica COLOR="#333399">
<TABLE ALIGN=CENETR WIDTH=110%>
<TD BGCOLOR=#bbddff><I>Trenutna pozicija<br>vlak: </I>  2111 <br>
Relacija:<br> ZAGREB-GLA>NOVSKA---- </strong></TD><TR>
<TD BGCOLOR=#bbddff><I>Kolodvor: </I><strong>LIPOVLJANI<br> </TD><TR>
<TD BGCOLOR=#bbddff><I>Odlazak  </I><cr>
27.01.24. u 01:07 sati</TD><TR>
<TD><FONT FACE=Arial,Helvetica COLOR=#FF000A>
<BLINK>Kasni    6 min.                                   </BLINK><BR>
<FONT SIZE=4 FACE=Verdana,Arial,Helvetica COLOR="#333399">
 <BR>
</TD><TR><TD>
</TD></TABLE><HR><FONT SIZE=1 FACE=Arial,Helvetica COLOR=009FFF>
Stanje vlaka od 27/01/24   u 01:55   <HR>
<INPUT TYPE="HIDDEN" NAME="Category" VALUE="hzinfo">
<INPUT TYPE="HIDDEN" NAME="Service" VALUE="tpvl">
<INPUT TYPE="HIDDEN" NAME="SCREEN" VALUE="1">
<INPUT TYPE="SUBMIT" VALUE="Povrat">
</FORM>
</BODY>
</HTML>
"##;

        super::parse_delay_html(html3.to_string())?;

        Ok(())
    }

    #[test]
    fn test_parse_html4() -> Result<(), anyhow::Error> {
        let html4 = r##"<HTML>
<HEAD>
<meta name="viewport" content="width=device-width, initial-scale=1.0" charset=windows-1250">
<TITLE>Trenutna pozicija putničkog vlaka</TITLE>
</HEAD>
<BODY BACKGROUND=Images/slika.jpg><TABLE align="CENTER"><TR>
<TD><FONT COLOR="#333399"><FONT FACE=Verdana,Arial,Helvetica COLOR="#333399">
<H5 ALIGN=center>HŽ Infrastruktura<BR>Trenutna pozicija putničkog vlaka</H5></FONT>
</TR></TABLE>
<HR>
<FORM METHOD="GET" ACTION="http://10.215.0.117/hzinfo/Default.asp?"><P><FONT FACE=Arial,Helvetica COLOR="#333399" ALIGN=center  >
<STRONG>Broj vlaka: </STRONG>
<INPUT NAME="VL" TYPE="TEXT" SIZE="5" MAXLENGTH="5">
<P>
<P><STRONG>Vlak nije u evidenciji.                                     </STRONG></P>
<INPUT TYPE="HIDDEN" NAME="Category" VALUE="hzinfo">
<INPUT TYPE="HIDDEN" NAME="Service" VALUE="tpvl">
<INPUT TYPE="HIDDEN" NAME="SCREEN" VALUE="2">
<INPUT TYPE="SUBMIT" VALUE=" OK ">
</FORM>
<PRE><P>
<STRONG><P>
<STRONG><P>
<STRONG><P></PRE>
</BODY>
</HTML>
"##;

        super::parse_delay_html(html4.to_string())?;

        Ok(())
    }

    #[test]
    fn test_parse_html5() -> Result<(), anyhow::Error> {
        let html5 = r##"<HTML>
<HEAD>
<TITLE>Trenutna pozicija vlaka</TITLE>
<meta name="viewport" content="width=device-width, initial-scale=1.0" charset=windows-1250">
</HEAD>
<BODY BACKGROUND=Images/slika.jpg><TABLE align="CENTER"><TR>
<TD><FONT COLOR="#333399"><FONT FACE=Verdana,Arial,Helvetica COLOR="#333399">
<H3 ALIGN=center>HŽ Infrastruktura<BR>                                  </H3></FONT>
</TR></TABLE>
<HR>
<FORM METHOD="GET" ACTION="http://10.215.0.117/hzinfo/Default.asp?">
<P ALIGN=CENTER>
<FONT SIZE=6 FACE=Arial,Helvetica COLOR="#333399">
<TABLE ALIGN=CENETR WIDTH=110%>
<TD BGCOLOR=#bbddff><I>Trenutna pozicija<br>vlak: </I>  2023 <br>
Relacija:<br> ZAGREB-GLA>VINKOVCI-- </strong></TD><TR>
<TD BGCOLOR=#bbddff><I>Kolodvor: </I><strong>ZAGREB+GL.+KOL.<br> </TD><TR>
<TD BGCOLOR=#bbddff><I>Formiran </I><cr>
27.01.24. u 17:34 sati</TD><TR>
<TD><FONT FACE=Arial,Helvetica COLOR=#FF000A>
<BLINK>                                                  </BLINK><BR>
<FONT SIZE=4 FACE=Verdana,Arial,Helvetica COLOR="#333399">
Vlak ceka polazak                                 <BR>
</TD><TR><TD>
</TD></TABLE><HR><FONT SIZE=1 FACE=Arial,Helvetica COLOR=009FFF>
Stanje vlaka od 27/01/24   u 18:54   <HR>
<INPUT TYPE="HIDDEN" NAME="Category" VALUE="hzinfo">
<INPUT TYPE="HIDDEN" NAME="Service" VALUE="tpvl">
<INPUT TYPE="HIDDEN" NAME="SCREEN" VALUE="1">
<INPUT TYPE="SUBMIT" VALUE="Povrat">
</FORM>
</BODY>
</HTML>
"##;

        super::parse_delay_html(html5.to_string())?;

        Ok(())
    }

    #[test]
    fn test_parse_html6() -> Result<(), anyhow::Error> {
        let html6 = r##"<!DOCTYPE HTML PUBLIC \"-//W3C//DTD HTML 4.01 Transitional//EN\"
\"http://www.w3.org/TR/html4/loose.dtd\">
<html><head><title>Unisys Internet Commerce Enabler Error Message</title></head>
<body>
<table width=\"100%\" border=0><tr><td rowspan=2>
<img src=\"/CISystem/Images/Globe.gif\" width=147 height=55 alt=\"\"/>
</td><td colspan=2 width=\"85%\">
<font face=\"georgia, times-new-roman\" size=4 color=\"#0033FF\">
<a href=\"http://www.unisys.com/sw/web/ice\">
<img src=\"/CISystem/Images/ICEPower-Img.gif\" width=160 height=43 align=\"right\" border=0
 alt=\"Click here for information about Unisys Internet Commerce Enabler\"/></a>
<b><i>Unisys Internet Commerce Enabler</i></b></font></td>
</tr><tr><td colspan=2 bgcolor=\"#0033FF\" height=16 width=\"85%\">
</td></tr></table>
<br><br><font size=5><b>Error Description:</b></font>
<hr>
<font size=4 color=\"#FF0000\"><b>The maximum number of available Cool ICE sessions has been exceeded.  Please try again later.</b></font>
<hr>
<br><font size=5><b>Error Code:</b></font>
<hr>
<font size=4 color=\"#FF0000\"><b>800417D9</b></font>
<hr><br><br><br>
Please report this error to the Webmaster, or System Administrator
<hr>
</body></html>"##;

        super::parse_delay_html(html6.to_string())?;

        Ok(())
    }

    #[test]
    fn test_parse_html7() -> Result<(), anyhow::Error> {
        let html7 = r##"<HTML>
<HEAD>
<TITLE>Trenutna pozicija vlaka</TITLE>
<meta name="viewport" content="width=device-width, initial-scale=1.0" charset=windows-1250">
</HEAD>
<BODY BACKGROUND=Images/slika.jpg><TABLE align="CENTER"><TR>
<TD><FONT COLOR="#333399"><FONT FACE=Verdana,Arial,Helvetica COLOR="#333399">
<H3 ALIGN=center>HŽ Infrastruktura<BR>                                  </H3></FONT>
</TR></TABLE>
<HR>
<FORM METHOD="GET" ACTION="http://10.215.0.117/hzinfo/Default.asp?">
<P ALIGN=CENTER>
<FONT SIZE=6 FACE=Arial,Helvetica COLOR="#333399">
<TABLE ALIGN=CENETR WIDTH=110%>
<TD BGCOLOR=#bbddff><I>Trenutna pozicija<br>vlak: </I>  5121 <br>
Relacija:<br> ZAGREB-GLA>SISAK-CAPR </strong></TD><TR>
<TD BGCOLOR=#bbddff><I>Kolodvor: </I><strong>ZAGREB+GL.+KOL.<br> </TD><TR>
<TD BGCOLOR=#bbddff><I>Formiran </I><cr>
31.01.24. u 20:11 sati</TD><TR>
<TD><FONT FACE=Arial,Helvetica COLOR=#FF000A>
<BLINK>                                                  </BLINK><BR>
<FONT SIZE=4 FACE=Verdana,Arial,Helvetica COLOR="#333399">
 <BR>
</TD><TR><TD>
</TD></TABLE><HR><FONT SIZE=1 FACE=Arial,Helvetica COLOR=009FFF>
Stanje vlaka od 31/01/24   u 20:19   <HR>
<INPUT TYPE="HIDDEN" NAME="Category" VALUE="hzinfo">
<INPUT TYPE="HIDDEN" NAME="Service" VALUE="tpvl">
<INPUT TYPE="HIDDEN" NAME="SCREEN" VALUE="1">
<INPUT TYPE="SUBMIT" VALUE="Povrat">
</FORM>
</BODY>
</HTML>
"##;

        super::parse_delay_html(html7.to_string())?;

        Ok(())
    }

    #[test]
    fn test_parse_html8() -> Result<(), anyhow::Error> {
        let html8 = r##"<HTML>
<HEAD>
<TITLE>Trenutna pozicija vlaka</TITLE>
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1.0\" charset=windows-1250\">
</HEAD>
<BODY BACKGROUND=Images/slika.jpg><TABLE align=\"CENTER\"><TR>
<TD><FONT COLOR=\"#333399\"><FONT FACE=Verdana,Arial,Helvetica COLOR=\"#333399\">
<H3 ALIGN=center>HŽ Infrastruktura<BR>                                  </H3></FONT>
</TR></TABLE>
<HR>
<FORM METHOD=\"GET\" ACTION=\"http://10.215.0.117/hzinfo/Default.asp?\">
<P ALIGN=CENTER>
<FONT SIZE=6 FACE=Arial,Helvetica COLOR=\"#333399\">
<TABLE ALIGN=CENETR WIDTH=110%>
<TD BGCOLOR=#bbddff><I>Trenutna pozicija<br>vlak: </I>  3136 <br>
Relacija:<br> ZABOK----->DJURMANEC- </strong></TD><TR>
<TD BGCOLOR=#bbddff><I>Kolodvor: </I><strong>KRAPINA<br> </TD><TR>
<TD BGCOLOR=#bbddff><I>Formiran </I><cr>
02.02.24. u 18:09 sati</TD><TR>
<TD><FONT FACE=Arial,Helvetica COLOR=#FF000A>
<BLINK>                                                  </BLINK><BR>
<FONT SIZE=4 FACE=Verdana,Arial,Helvetica COLOR=\"#333399\">
 <BR>
</TD><TR><TD>
</TD></TABLE><HR><FONT SIZE=1 FACE=Arial,Helvetica COLOR=009FFF>
Stanje vlaka od 02/02/24   u 18:29   <HR>
<INPUT TYPE=\"HIDDEN\" NAME=\"Category\" VALUE=\"hzinfo\">
<INPUT TYPE=\"HIDDEN\" NAME=\"Service\" VALUE=\"tpvl\">
<INPUT TYPE=\"HIDDEN\" NAME=\"SCREEN\" VALUE=\"1\">
<INPUT TYPE=\"SUBMIT\" VALUE=\"Povrat\">
</FORM>
</BODY>
</HTML>
"##;

        super::parse_delay_html(html8.to_string())?;

        Ok(())
    }
}
