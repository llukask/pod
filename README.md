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

### TUI Client

- **Terminal UI**: Ratatui-based client for managing podcasts and playback from the terminal
- **Local-first**: SQLite database for offline access, syncs with the server
- **Audio Playback**: mpv-based audio player with MPRIS media key support

## Tech Stack

- **Backend**: Rust with Axum web framework
- **Database**: PostgreSQL with SQLx (server), SQLite with rusqlite (TUI)
- **Authentication**: Argon2 password hashing, Bearer token sessions
- **Feed Parsing**: RSS/Atom feed support via feed-rs
- **TUI**: Ratatui + Crossterm, MPRIS D-Bus integration

## Quickstart

### Prerequisites

- Rust (1.70+)
- PostgreSQL (13+)

### 1. Clone and Setup

```bash
git clone <your-repo-url>
cd pod
```

### 2. Build

```bash
# Build everything
cargo build

# Build only the server
cargo build -p pod-server

# Build only the TUI client
cargo build -p pod-tui
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
# Run the server
cargo run --bin pod-server

# Or with logging
RUST_LOG=debug cargo run --bin pod-server
```

The API will be available at `http://localhost:3000/api/v1`.

To run the TUI client (requires a running server):

```bash
cargo run --bin pod
```

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

The project is organized as a Cargo workspace with three crates:

```
pod/
├── crates/
│   ├── pod-model/         # Shared data types (API contract)
│   │   └── src/lib.rs     # Podcast, Episode, sync types
│   ├── pod-server/        # HTTP API server
│   │   ├── .sqlx/         # SQLx offline query cache
│   │   ├── migrations/    # PostgreSQL migrations
│   │   └── src/
│   │       ├── main.rs    # Server entry point
│   │       ├── app.rs     # Application logic
│   │       ├── config.rs  # Configuration loading
│   │       ├── model.rs   # Server-only types (User, Session)
│   │       ├── feed.rs    # RSS feed processing
│   │       ├── db/        # Database layer
│   │       └── http/      # Routes, auth, error handling
│   │           └── api/   # JSON API handlers
│   └── pod-tui/           # Terminal UI client
│       └── src/
│           ├── main.rs    # TUI entry point
│           ├── app.rs     # TUI state and actions
│           ├── api_client.rs  # Server HTTP client
│           ├── local_db.rs    # Local SQLite storage
│           ├── player.rs      # mpv audio playback
│           ├── mpris.rs       # Media key integration
│           ├── sync.rs        # Server sync logic
│           └── ui/            # Screen rendering
├── frontend/              # Static web frontend
└── openapi.yaml           # API specification
```

### Database Migrations

Run from `crates/pod-server/`:

```bash
sqlx migrate add <migration_name>
sqlx migrate run
```

### Environment Variables

- `DATABASE_URL` (required): PostgreSQL connection string
- `BASE_URL` (required): Application's public URL
- `PORT` (optional, default `3000`): Listen port
- `REFRESH_INTERVAL_SECS` (optional, default `600`): Seconds between podcast refresh cycles
- `ALLOW_REGISTRATION` (optional, default `true`): Whether self-serve signup is permitted
