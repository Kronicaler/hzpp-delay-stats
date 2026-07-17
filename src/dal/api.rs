use anyhow::{Context, Error, anyhow, bail};
use axum::http::HeaderValue;
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use chrono_tz::{Europe::Zagreb, Tz};
use itertools::Itertools;
use reqwest::{Client, Method, Url};
use tracing::{Instrument, info, info_span};

use crate::{
    model::{
        db_model::RouteDb,
        hzpp_api_model::{HzppRoute, HzppStation},
    },
    utils::str_between_str,
};

#[tracing::instrument(err)]
pub async fn fetch_stations() -> Result<Vec<HzppStation>, Error> {
    let request = format!("https://josipsalkovic.com/hzpp/planer/v3/getStops.php");

    let response = reqwest::get(&request)
        .instrument(info_span!("Fetching stations"))
        .await?
        .error_for_status()?;

    let stations_string = response
        .text()
        .instrument(info_span!("Reading body of response"))
        .await?;

    let stations: Vec<HzppStation> =
        serde_json::from_str(&stations_string).context("Error parsing stations")?;

    info!("got {} stations", stations.len());

    Ok(stations)
}

#[tracing::instrument(err)]
pub async fn fetch_routes(date: DateTime<Tz>) -> Result<Vec<HzppRoute>, Error> {
    let request = format!(
        "https://josipsalkovic.com/hzpp/planer/v3/getRoutes.php?date={}",
        date.format("%Y%m%d")
    );

    let response = reqwest::get(&request)
        .instrument(info_span!("Fetching routes"))
        .await?
        .error_for_status()?;

    let routes_string = response
        .text()
        .instrument(info_span!("Reading body of response"))
        .await?;

    let routes: Vec<HzppRoute> = serde_json::from_str(&routes_string)?;

    info!("got {} routes", routes.len());

    Ok(routes)
}

#[tracing::instrument(ret, err)]
pub async fn get_route_status(route: &RouteDb) -> Result<StatusResponse, Error> {
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
pub fn parse_delay_html(html: String) -> Result<StatusResponse, Error> {
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
pub enum StatusResponse {
    TrainStatus(TrainStatus),
    TrainNotEvidented,
    UnisysError,
}

#[derive(Clone, Debug)]
pub struct TrainStatus {
    pub station: String,
    pub status: Status,
    pub delay: Delay,
}

#[derive(Copy, Clone, Debug)]
pub enum Delay {
    NoData,
    WaitingToDepart,
    OnTime,
    Late { minutes_late: i32 },
}

#[derive(Copy, Clone, Debug)]
pub enum Status {
    Formed(DateTime<Utc>),
    DepartingFromStation(DateTime<Utc>),
    Arriving(DateTime<Utc>),
    FinishedDriving(DateTime<Utc>),
}

impl Status {
    pub fn get_time(self) -> DateTime<Utc> {
        match self {
            Status::Formed(dt) => dt,
            Status::DepartingFromStation(dt) => dt,
            Status::Arriving(dt) => dt,
            Status::FinishedDriving(dt) => dt,
        }
    }
}

impl TrainStatus {
    pub fn get_minutes_late(&self) -> Option<i32> {
        match self.delay {
            Delay::NoData => None,
            Delay::WaitingToDepart => None,
            Delay::OnTime => Some(0),
            Delay::Late { minutes_late } => Some(minutes_late),
        }
    }
}

#[cfg(test)]
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
