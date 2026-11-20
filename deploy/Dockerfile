# syntax=docker/dockerfile:1
# One image for the server nodes and the load generator.

FROM rust:1.100-slim-trixie AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p server --bin web-server -p bench --bin loadgen \
    && cp target/release/web-server target/release/loadgen /usr/local/bin/

FROM debian:trixie-slim
COPY --from=build /usr/local/bin/web-server /usr/local/bin/loadgen /usr/local/bin/
COPY web /app/web
WORKDIR /app
ENV DRL_WEB_DIR=/app/web \
    DRL_BIND=0.0.0.0:3001
EXPOSE 3001
USER nobody
CMD ["web-server"]
