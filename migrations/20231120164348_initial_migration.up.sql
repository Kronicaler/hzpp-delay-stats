CREATE TABLE IF NOT EXISTS stations(
    id varchar(255) primary key NOT NULL,
    code int NOT NULL,
    name varchar(255) NOT NULL,
    latitude double precision NOT NULL,
    longitude double precision NOT NULL
);

CREATE TABLE IF NOT EXISTS routes(
    id varchar(255) primary key NOT NULL,
    train_number int NOT NULL,
    source varchar(255) NOT NULL,
    destination varchar(255) NOT NULL,
    bikes_allowed char NOT NULL,
    wheelchair_accessible char NOT NULL,
    route_type char NOT NULL,
    expected_start_time timestamp NOT NULL,
    real_start_time timestamp NULL,
    expected_end_time timestamp NOT NULL,
    real_end_time timestamp NULL,
    days_of_the_week char -- 0, 1, 2 = monday, tuesday, wednesday
);

CREATE TABLE IF NOT EXISTS stops(
    id varchar(255) primary key NOT NULL,
    station_id varchar(255) NOT NULL,
    route_id varchar(255) NOT NULL,
    sequence int NOT NULL,
    code int NOT NULL,
    expected_arrival timestamp NOT NULL,
    real_arrival timestamp NOT NULL,
    expected_departure timestamp NOT NULL,
    real_departure timestamp NOT NULL,
    FOREIGN KEY (station_id) REFERENCES stations(id),
    FOREIGN KEY (route_id) REFERENCES routes(id)
);