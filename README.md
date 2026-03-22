# Pod - Self-Hosted Podcast Manager

A personal podcast aggregation service built with Rust and PostgreSQL. Pod allows you to subscribe to your favorite podcasts, automatically fetches new episodes, and exposes a JSON REST API for managing subscriptions and tracking listening progress.

## Features

### Core Functionality

- **Podcast Subscription**: Add podcasts via RSS feed URLs
- **Automatic Updates**: Background refresh of all subscribed podcasts every 10 minutes
- **Progress Tracking**: Remembers where you left off in each episode
- **Multi-user Support**: Each user has their own subscriptions and progress

### Technical Features

- **Username/Password Authentication**: Argon2 password hashing with Bearer token sessions
- **PostgreSQL Backend**: Reliable data storage for users, podcasts, and progress
- **Background Processing**: Automatic podcast refresh on a configurable interval
- **JSON REST API**: Full-featured API with cursor-based pagination

## Tech Stack

- **Backend**: Rust with Axum web framework
- **Database**: PostgreSQL with SQLx
- **Authentication**: Argon2 password hashing, Bearer token sessions
- **Feed Parsing**: RSS/Atom feed support via feed-rs

## Quickstart

### Prerequisites

- Rust (1.70+)
- PostgreSQL (13+)

### 1. Clone and Setup

```bash
git clone <your-repo-url>
cd pod
```

### 2. Install Dependencies

```bash
cargo build
```

### 3. Database Setup

Create a PostgreSQL database and run migrations:

```bash
# Create database
createdb pod

# Set database URL
export DATABASE_URL="postgresql://username:password@localhost/pod"

# Install sqlx-cli if not already installed
cargo install sqlx-cli --no-default-features --features postgres

# Run migrations
sqlx migrate run
```

### 4. Environment Configuration

Create a `.env` file in the project root:

```env
# Database
DATABASE_URL=postgresql://username:password@localhost/pod

# Application URLs
BASE_URL=http://localhost:3000

# Optional: disable self-serve signups (defaults to true)
# ALLOW_REGISTRATION=false
```

Alternatively, create a `pod.toml` file with the same fields in snake_case. Environment variables take priority over the TOML file.

### 5. Run the Application

```bash
# Development mode
cargo run --bin pod-server

# Or with logging
RUST_LOG=debug cargo run --bin pod-server
```

The API will be available at `http://localhost:3000/api/v1`.

### 6. Create a User

With registration enabled (the default), POST to the register endpoint:

```bash
curl -X POST http://localhost:3000/api/v1/auth/register \
  -H 'Content-Type: application/json' \
  -d '{"username": "alice", "password": "your-password"}'
```

This returns a Bearer token you can use for subsequent requests.

## API

Base path: `/api/v1`. All responses are JSON. Authenticate with `Authorization: Bearer <token>`.

See `openapi.yaml` for the full specification.

### Authentication

- `POST /api/v1/auth/register` — Create a new account. Body: `{ "username", "password" }`. Returns `{ "token", "expires_at" }`.
- `POST /api/v1/auth/login` — Log in. Same request/response as register.
- `GET /api/v1/auth/me` — Fetch the current authenticated user. Returns `{ "username" }`.
- `POST /api/v1/auth/logout` — Invalidate the current session token.

### Podcasts

- `GET /api/v1/podcasts` — List subscribed podcasts with episode stats.
- `POST /api/v1/podcasts` — Subscribe to a podcast. Body: `{ "feed_url": "<rss_url>" }`. Creates the podcast if it doesn't exist.
- `GET /api/v1/podcasts/:id` — Fetch a subscribed podcast by ID.
- `GET /api/v1/podcasts/:id/episodes?per_page=20&page_token=<token>` — List episodes with user progress and `done` state, newest first. Cursor-based pagination; use the returned `next_page_token` to fetch the next page.

### Episodes

- `POST /api/v1/episodes/:id/progress` — Record listening progress. Body: `{ "progress": <seconds>, "done": <bool> }`. Returns `{ "progress", "done" }`.

### CORS

API responses mirror the caller's `Origin` header and allow credentials, so browser clients from any domain can call the API with Bearer tokens.

## Development

### Project Structure

```
pod/
├── src/
│   ├── bin/           # Binary executables
│   │   ├── pod-server.rs      # Main API server
│   │   └── pod-import-google.rs # Google Podcasts import utility
│   ├── db/            # Database layer
│   ├── http/          # HTTP server, routes, and middleware
│   │   ├── api/       # JSON API handlers
│   │   ├── auth.rs    # Authentication extractors and session management
│   │   └── errors.rs  # Error types
│   ├── app.rs         # Application logic
│   ├── feed.rs        # RSS feed processing
│   └── model.rs       # Data models
├── migrations/        # Database migrations
└── openapi.yaml       # API specification
```

### Database Migrations

Create a new migration:

```bash
sqlx migrate add <migration_name>
```

Run migrations:

```bash
sqlx migrate run
```

### Environment Variables

- `DATABASE_URL` (required): PostgreSQL connection string
- `BASE_URL` (required): Application's public URL
- `PORT` (optional, default `3000`): Listen port
- `REFRESH_INTERVAL_SECS` (optional, default `600`): Seconds between podcast refresh cycles
- `ALLOW_REGISTRATION` (optional, default `true`): Whether self-serve signup is permitted
