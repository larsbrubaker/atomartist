# Build stage
FROM rustlang/rust:nightly as builder

WORKDIR /app

# Copy the standalone atomartist project
COPY . .

# Build the application
RUN cargo build --release --locked

# Runtime stage
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/target/release/atomartist /app/atomartist
COPY --from=builder /app/migrations ./migrations

EXPOSE 8080

CMD ["/app/atomartist"]

