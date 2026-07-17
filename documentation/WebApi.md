# WEB API

## What does a user want to see?

- delay of train X at station Y at Z timespan (XYZ all optional)
- alert of train line they frequently search/favorited having recent delays or railway works announced
- get trains driving from station X to station Y at depart time Z (XYZ all required)
- current status and info of a train

### Interesting data to show

- total delay of all trains (at station/region/train line/country)
- map of train lines and stations showing relevant data (marking stations in red that have a lot of delays or railway works)

### Endpoints

#### /api/routes/status

Parameters:

```Rust
route_numbers: Vec<i32>
```

Returns:
The status of the requested routes

#### /api/routes/delay

Parameters:

```Rust
route_numbers: Vec<i32>
station_id: string?
from: DateTime?
to: DateTime?
```

Returns:
The delays of the requested routes

#### /api/routes

Parameters:

```Rust
start_station: string
end_station: string
depart_time: DateTime // depart time of the start_station
```

Returns:
The Routes which pass through the start station around the specified depart_time and pass through the end_station after the start_station.

#### /api/user/register

Parameters:

```Rust
guid: string
```

Returns:
HTTP status 200

#### /api/route/favorite

Parameters:

```Rust
user_guid: string
route_number: i32
alert_on_railway_works: bool
alert_on_delay: i32?
```

Returns:
HTTP status 200
