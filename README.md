# dockertest-rs

Run docker containers in your Rust integration tests.

## Example

 ```rust
 let mut test = DockerTest::new();
 test.add_instance(ImageInstace::new("postgres"));

 test.run(|ops| {
     // Create connection
     let container = ops.handle("postgres").expect("retrieve postgres container reference");
     let host_port = container.host_port(5432).expect("host port for postgres");
     let conn_string = format!("postgres://postgres::postgres@localhost:{}", host_port);
     let connection = PgConnection::establish().expect("connection to postgres");

     // Issue database operation

 })
```

## Development

This library is in its initial inception. Breaking changes are to be expected.
