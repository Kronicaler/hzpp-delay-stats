//! Responsible for checking the delays of routes gotten from the route_fetcher

use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use chrono_tz::Europe::Zagreb;
use itertools::Itertools;
use reqwest::{header::HeaderValue, Client, Method, Url};
use sqlx::{postgres::PgRow, query, Pool, Postgres, Row};
use tokio::{spawn, sync::mpsc::Receiver, time::sleep};
use tracing::{error, info, info_span, Instrument};

use crate::{model::db_model::RouteDb, utils::str_between_str};

/// Checks the delays of the routes from the given channel and saves them to the DB
pub async fn check_delays(
    delay_checker_receiver: &mut Receiver<Vec<RouteDb>>,
    pool: &Pool<Postgres>,
) -> Result<(), anyhow::Error> {
    let mut buffer: Vec<Vec<RouteDb>> = vec![];

    spawn_route_delay_tasks(get_unfinished_routes(pool).await?, pool).await;

    while delay_checker_receiver.recv_many(&mut buffer, 32).await != 0 {
        let routes = buffer.drain(..).flatten().collect_vec();

        spawn_route_delay_tasks(routes, pool).await;
    }

    info!("Channel closed");

    Ok(())
}

async fn spawn_route_delay_tasks(routes: Vec<RouteDb>, pool: &Pool<Postgres>) {
    for mut route in routes {
        let secs_until_end = route.expected_end_time.timestamp() - Utc::now().timestamp();
        if secs_until_end < 0 {
            info!("Got route in the past, discarding it");

            route.real_start_time = route.real_start_time.or(Some(route.expected_start_time));
            route.real_end_time = route.real_end_time.or(Some(route.expected_end_time));
            if let Err(e) = update_route_real_times(&route, pool).await {
                error!("error when saving old route {}", e);
            }

            continue;
        }

        let delay_pool = pool.clone();
        spawn(monitor_route(route, delay_pool));
    }
}

async fn get_unfinished_routes(pool: &Pool<Postgres>) -> Result<Vec<RouteDb>, anyhow::Error> {
    let x = query(
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
        from routes where real_end_time IS NULL",
    )
    .map(|row: PgRow| RouteDb {
        id: row.try_get(0).unwrap(),
        route_number: row.try_get(1).unwrap(),
        source: row.try_get(2).unwrap(),
        destination: row.try_get(3).unwrap(),
        bikes_allowed: row.try_get::<i16, usize>(4).unwrap().try_into().unwrap(),
        wheelchair_accessible: row.try_get::<i16, usize>(5).unwrap().try_into().unwrap(),
        route_type: row.try_get::<i16, usize>(6).unwrap().try_into().unwrap(),
        expected_start_time: row.try_get::<NaiveDateTime, usize>(7).unwrap().and_utc(),
        expected_end_time: row.try_get::<NaiveDateTime, usize>(8).unwrap().and_utc(),
        real_start_time: row
            .try_get::<Option<NaiveDateTime>, usize>(9)
            .unwrap()
            .map(|dt| dt.and_utc()),
        real_end_time: row
            .try_get::<Option<NaiveDateTime>, usize>(10)
            .unwrap()
            .map(|dt| dt.and_utc()),
    })
    .fetch_all(pool)
    .await?;

    return Ok(x);
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
    loop {
        let delay: TrainStatus = match get_route_delay(&route).await {
            Ok(it) => it,
            Err(err) => match err {
                GetRouteError::TrainNotEvidented => {
                    info!("train isn't evidented"); // sometimes the API is just shit
                    info!(train_not_evidented = true);
                    break Ok(());
                }
                GetRouteError::Other(err) => {
                    error!("{:?}", err.context("error fetching delay"));
                    sleep(Duration::from_secs(60))
                        .instrument(info_span!("Waiting 60 seconds"))
                        .await;
                    continue;
                }
            },
        };

        match (delay.delay, delay.status) {
            (Delay::WaitingToDepart, Status::DepartingFromStation(_) | Status::Arriving(_)) => {} // wait for train to start
            (Delay::WaitingToDepart, Status::Formed(_)) => {} // wait for train to start
            (Delay::OnTime, Status::DepartingFromStation(_) | Status::Arriving(_)) => {
                if route.real_start_time.is_none() {
                    route.real_start_time = Some(route.expected_start_time);
                    update_route_real_times(&route, &pool).await?;
                }
            }
            (
                Delay::Late { minutes_late },
                Status::DepartingFromStation(_) | Status::Arriving(_),
            ) => {
                if route.real_start_time.is_none() {
                    route.real_start_time = Some(
                        route.expected_start_time + chrono::Duration::minutes(minutes_late.into()),
                    );
                    update_route_real_times(&route, &pool).await?;
                }
            }
            (Delay::OnTime, Status::FinishedDriving(_)) => {
                route.real_end_time = Some(route.expected_end_time);
                update_route_real_times(&route, &pool).await?;
                return Ok(());
            }
            (Delay::Late { minutes_late }, Status::FinishedDriving(_)) => {
                route.real_end_time =
                    Some(route.expected_end_time + chrono::Duration::minutes(minutes_late.into()));
                update_route_real_times(&route, &pool).await?;
                return Ok(());
            }
            _ => bail!("invalid variant"),
        }

        sleep(Duration::from_secs(60))
            .instrument(info_span!("Waiting 60 seconds"))
            .await;
    }
}

#[tracing::instrument(ret, err)]
async fn get_route_delay(route: &RouteDb) -> Result<TrainStatus, GetRouteError> {
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

    let status = match parse_delay_html(content) {
        Ok(status) => status,
        Err(err) => match err {
            ParseHtmlError::TrainNotEvidented => return Err(GetRouteError::TrainNotEvidented),
            ParseHtmlError::Other(e) => return Err(e)?,
        },
    };

    Ok(status)
}

#[derive(thiserror::Error, Debug)]
enum GetRouteError {
    #[error("train isn't evidented")]
    TrainNotEvidented,

    #[error("error getting route delay")]
    Other(#[from] anyhow::Error),
}

#[tracing::instrument(ret, err)]
fn parse_delay_html(html: String) -> Result<TrainStatus, ParseHtmlError> {
    let lines = html.lines().collect_vec();

    if lines
        .get(14)
        .ok_or_else(|| anyhow!("couldn't get line 14"))?
        .contains("Vlak nije u evidenciji")
    {
        return Err(ParseHtmlError::TrainNotEvidented)?;
    }

    let station_line = *lines
        .iter()
        .filter(|l| l.contains("Kolodvor:"))
        .collect_vec()
        .first()
        .ok_or_else(|| anyhow!("Couldn't locate station line"))?;

    let station = str_between_str(station_line, "</I><strong>", "<br>")
        .ok_or_else(|| anyhow!("Couldn't locate station"))?
        .to_string();

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
    let status_datetime = status_date
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
    } else if html.contains("Vlak nije u evidenciji") {
        return Err(anyhow!("The train is not registered"))?;
    } else {
        return Err(anyhow!("Unknown delay response"))?;
    };

    Ok(TrainStatus {
        delay,
        station,
        status,
    })
}

#[derive(thiserror::Error, Debug)]
enum ParseHtmlError {
    #[error("train isn't evidented")]
    TrainNotEvidented,

    #[error("error parsing the delay html")]
    Other(#[from] anyhow::Error),
}

#[derive(Clone, Debug)]
struct TrainStatus {
    pub station: String,
    pub status: Status,
    pub delay: Delay,
}

#[derive(Copy, Clone, Debug)]
enum Delay {
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
        route.real_start_time.map(|dt| dt.naive_utc()),
        route.real_end_time.map(|dt| dt.naive_utc()),
        route.expected_start_time.naive_utc(),
        route.id
    )
    .execute(pool)
    .await?;

    Ok(())
}

mod tests {
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

        match super::parse_delay_html(html4.to_string()) {
            Ok(_) => Ok(()),
            Err(err) => match err {
                super::ParseHtmlError::TrainNotEvidented => Ok(()), // expected error
                super::ParseHtmlError::Other(err) => Err(err),      // unexpected error
            },
        }
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
}
