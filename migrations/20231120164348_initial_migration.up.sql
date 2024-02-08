CREATE TABLE stations(
    id varchar(255) PRIMARY KEY NOT NULL,
    code int NOT NULL,
    name varchar(255) NOT NULL,
    latitude double precision NOT NULL,
    longitude double precision NOT NULL
);

CREATE TABLE routes(
    id varchar(255) NOT NULL,
    route_number int NOT NULL,
    source varchar(255) NOT NULL,
    destination varchar(255) NOT NULL,
    bikes_allowed smallint NOT NULL,
    wheelchair_accessible smallint NOT NULL,
    route_type smallint NOT NULL,
    real_start_time timestamptz NULL,
    expected_start_time timestamptz NOT NULL,
    real_end_time timestamptz NULL,
    expected_end_time timestamptz NOT NULL,
    PRIMARY KEY(expected_start_time, id)
);

CREATE TABLE stops(
    station_id varchar(255) NOT NULL,
    route_id varchar(255) NOT NULL,
    route_expected_start_time timestamptz NOT NULL,
    sequence smallint NOT NULL,
    real_arrival timestamptz NULL,
    expected_arrival timestamptz NOT NULL,
    real_departure timestamptz NULL,
    expected_departure timestamptz NOT NULL,
    PRIMARY KEY(route_id, route_expected_start_time, sequence),
    FOREIGN KEY (station_id) REFERENCES stations(id),
    FOREIGN KEY (route_id, route_expected_start_time) REFERENCES routes(id, expected_start_time)
);