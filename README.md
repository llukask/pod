# Pod - Self-Hosted Podcast Manager

A personal podcast aggregation and playback application built with Rust and PostgreSQL. Pod allows you to subscribe to your favorite podcasts, automatically fetches new episodes, and provides a clean web interface for listening with progress tracking.

## Features

### Core Functionality

- **Podcast Subscription**: Add podcasts via RSS feed URLs
- **Automatic Updates**: Background refresh of all subscribed podcasts every 10 minutes
- **Progress Tracking**: Remembers where you left off in each episode
- **Web-based Player**: HTML5 audio player with 30-second skip controls
- **Multi-user Support**: Each user has their own subscriptions and progress

### Technical Features

- **Google OAuth Authentication**: Secure login with Google accounts
- **Responsive Design**: Modern UI with Tailwind CSS and dark mode support
- **PostgreSQL Backend**: Reliable data storage for users, podcasts, and progress
- **Background Processing**: Automatic podcast refresh without blocking the UI
- **Session Management**: Secure cookie-based authentication

## Tech Stack

- **Backend**: Rust with Axum web framework
- **Database**: PostgreSQL with SQLx
- **Frontend**: HTML templates with Askama, Tailwind CSS
- **Authentication**: Google OAuth 2.0
- **Feed Parsing**: RSS/Atom feed support

## Quickstart

### Prerequisites

- Rust (1.70+)
- PostgreSQL (13+)
- Node.js and pnpm (for frontend assets)
- Google OAuth application credentials

### 1. Clone and Setup

```bash
git clone <your-repo-url>
cd pod
```

### 2. Install Dependencies

```bash
# Install Rust dependencies
cargo build

# Install frontend dependencies
pnpm install
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

### 4. Google OAuth Setup

1. Go to the [Google Cloud Console](https://console.cloud.google.com/)
2. Create a new project or select an existing one
3. Enable the Google+ API
4. Create OAuth 2.0 credentials
5. Add your domain to authorized origins
6. Add `{your-domain}/auth/google_callback` to authorized redirect URIs

### 5. Environment Configuration

Create a `.env` file in the project root:

```env
# Database
DATABASE_URL=postgresql://username:password@localhost/pod

# Google OAuth
GOOGLE_OAUTH_CLIENT_ID=your-google-client-id
GOOGLE_OAUTH_CLIENT_SECRET=your-google-client-secret

# Application URLs
BASE_URL=http://localhost:3000
COOKIE_DOMAIN=localhost

# Optional: Cookie encryption key (will generate if not provided)
# COOKIE_KEY=base64-encoded-64-byte-key
```

### 6. Build Frontend Assets

```bash
# Generate CSS
npx tailwindcss -i styles/input.css -o assets/main.css --watch
```

### 7. Run the Application

```bash
# Development mode
cargo run --bin pod-server

# Or with logging
RUST_LOG=debug cargo run --bin pod-server
```

The application will be available at `http://localhost:3000`.

## Usage

### Adding Podcasts

1. Log in with your Google account
2. On the dashboard, paste a podcast RSS feed URL into the input field
3. Click "Add Feed" to subscribe
4. The app will automatically fetch episodes in the background

### Listening to Episodes

1. Click on a podcast from your dashboard
2. Browse available episodes
3. Click "Start playing" to begin an episode
4. Use the skip forward/backward buttons (30 seconds)
5. Your progress is automatically saved

### Managing Subscriptions

Currently, podcast management is done through the web interface. Episodes are automatically refreshed every 10 minutes.

## Development

### Project Structure

```
pod/
├── src/
│   ├── bin/           # Binary executables
│   │   ├── pod-server.rs      # Main web server
│   │   └── pod-import-google.rs # Google Podcasts import utility
│   ├── db/            # Database layer
│   ├── http/          # Web server and routes
│   │   ├── web/       # Web interface handlers
│   │   └── auth.rs    # Authentication
│   ├── app.rs         # Application logic
│   ├── feed.rs        # RSS feed processing
│   └── model.rs       # Data models
├── templates/         # HTML templates
├── migrations/        # Database migrations
├── assets/           # Static assets
└── styles/           # Tailwind CSS source
```

### Running Tests

```bash
cargo test
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

For production deployment, ensure these environment variables are set:

- `DATABASE_URL`: PostgreSQL connection string
- `GOOGLE_OAUTH_CLIENT_ID`: Google OAuth client ID
- `GOOGLE_OAUTH_CLIENT_SECRET`: Google OAuth client secret
- `BASE_URL`: Your application's public URL
- `COOKIE_DOMAIN`: Your domain name
- `COOKIE_KEY`: Base64-encoded 64-byte encryption key
