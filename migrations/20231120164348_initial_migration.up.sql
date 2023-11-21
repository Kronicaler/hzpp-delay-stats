CREATE TABLE stations(
    id varchar(255) primary key NOT NULL,
    code int NOT NULL,
    name varchar(255) NOT NULL,
    latitude double precision NOT NULL,
    longitude double precision NOT NULL
);

CREATE TABLE routes(
    id varchar(255) NOT NULL,
    route_number int NOT NULL,
    usual_source varchar(255) NOT NULL,
    destination varchar(255) NOT NULL,
    bikes_allowed char NOT NULL,
    wheelchair_accessible char NOT NULL,
    route_type char NOT NULL,
    real_start_time timestamp NULL,
    expected_start_time timestamp NOT NULL,
    real_end_time timestamp NULL,
    expected_end_time timestamp NOT NULL,
    PRIMARY KEY(id,route_number)
);

CREATE TABLE stops(
    id varchar(255) primary key NOT NULL,
    station_id varchar(255) NOT NULL,
    route_id varchar(255) NOT NULL,
    sequence char NOT NULL,
    code int NOT NULL,
    real_arrival timestamp NULL,
    expected_arrival timestamp NOT NULL,
    real_departure timestamp NULL,
    expected_departure timestamp NOT NULL,
    FOREIGN KEY (station_id) REFERENCES stations(id),
    FOREIGN KEY (route_id) REFERENCES routes(id)
);