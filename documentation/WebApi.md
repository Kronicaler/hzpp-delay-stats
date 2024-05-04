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

/api/route/status
```
route_number: i32
```

/api/route/delay
```
route_number: i32?
station_id: string?
from: DateTime?
to: DateTime?
```

/api/routes
```
start_station: string
end_station: string
depart_time: DateTime // depart time of the start_station
```


/api/user/register
```
guid: string
```

/api/route/favorite
```
user_guid: string
route_number: i32
alert_on_railway_works: bool
alert_on_delay: i32?
```
