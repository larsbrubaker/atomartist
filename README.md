# AtomArtist

A Rust/Axum web application for AtomArtist.

## Development

### Prerequisites

- Rust 1.75+
- PostgreSQL
- Docker (optional)

### Running Locally

```bash
# Set up environment
export DATABASE_URL="postgresql://user:password@localhost:5432/atomartist"

# Run migrations and start server
cargo run
```

### Building Docker Image

```bash
docker build -t atomartist .
```

## API Endpoints

- `GET /` - Welcome message
- `GET /health` - Health check
- `GET /api/` - API index

## License

Copyright 2025 AtomArtist. All rights reserved.

