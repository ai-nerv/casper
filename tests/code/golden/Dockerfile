# build stage
FROM rust:1.94 AS build
WORKDIR /app
COPY . .
RUN cargo build --release

FROM scratch
COPY --from=build /app/target/release/app /app
ENTRYPOINT ["/app"]
