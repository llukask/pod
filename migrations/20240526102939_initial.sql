create table podcast (
	id text primary key default gen_random_uuid(),

	title text not null,
	description text not null,
	image_link text not null,

	feed_url text not null,
	feed_type text not null,

	created_at timestamptz not null default current_timestamp,
	last_updated timestamptz not null default current_timestamp,

	unique (feed_url)
);

create table episode (
	id text primary key,
	podcast_id text not null references podcast(id),

	title text not null,
	summary text not null,
	summary_type text not null,

	publication_date timestamptz not null,

	audio_url text not null,
	audio_type text not null,
	audio_duration int not null,

	thumbnail_url text,

	created_at timestamptz not null default current_timestamp,
	last_updated timestamptz not null default current_timestamp,

	unique (podcast_id, audio_url)
);

create table users (
	id uuid primary key default gen_random_uuid(),
	email text not null unique,
	created_at timestamptz not null default current_timestamp,
	last_updated timestamptz not null default current_timestamp
);

create table sessions (
	id uuid primary key default gen_random_uuid(),
	user_id uuid not null unique references users(id),
	session_id text not null unique,
	expires_at timestamptz not null
);

create table user_subscription (
    id text primary key default gen_random_uuid(),
    user_id uuid not null references users(id),
    podcast_id text not null references podcast(id),
    created_at timestamptz not null default current_timestamp,
    last_updated timestamptz not null default current_timestamp
);

create table user_episode (
    id text primary key default gen_random_uuid(),
    user_id uuid not null references users(id),
    episode_id text not null references episode(id),
    created_at timestamptz not null default current_timestamp,
    last_updated timestamptz not null default current_timestamp,

    progress int not null default 0,
    done boolean not null default false
);
