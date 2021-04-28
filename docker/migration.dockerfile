FROM rust:1.48.0-slim-buster

RUN apt update && apt install -y --no-install-recommends \
  pkg-config \
  libssl-dev \
  libcurl4-openssl-dev \
  libpq-dev

RUN cargo install sqlx-cli --version 0.5.2 --no-default-features --features postgres
WORKDIR /app
CMD ["cargo", "sqlx", "migrate", "run"]
COPY ./migrations /app/migrations
COPY Cargo.toml /app/Cargo.toml
COPY Cargo.lock /app/Cargo.lock
