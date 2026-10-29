# Pod

A self-hosted podcast server. It polls feeds, stores episodes in Postgres and
syncs subscriptions, playback positions and inbox state with desktop, Android
and web clients. This branch is the v2 rewrite; see `docs/spec.md` and `docs/design.md`.

## Running locally

```bash
nix develop                       # or install Rust and Postgres 16 yourself
scripts/dev-db.sh start
export DATABASE_URL="$(scripts/dev-db.sh url)"
sqlx migrate run --source crates/pod-server/migrations
POD_BASE_URL=http://localhost:3000 cargo run -p pod-server
curl -i http://localhost:3000/healthz
```

The server applies pending migrations on startup. The `sqlx migrate run` step
is for the build: with `DATABASE_URL` set, sqlx's `query!` macros check every
query against that database. Without it, or with `SQLX_OFFLINE=true`, they
read the committed `.sqlx/` cache instead. After changing a query or a
migration, regenerate and commit the cache (sqlx-cli 0.9, as in `nix
develop`):

```bash
cargo sqlx prepare --workspace -- --all-targets
```

## Configuration

The server reads a TOML file from `POD_CONFIG` (default `pod.toml`; a missing
default file is fine) and then applies environment overrides. See
`pod.example.toml`.

| Key | Env override | Default | Meaning |
| --- | --- | --- | --- |
| `database_url` | `POD_DATABASE_URL`, then `DATABASE_URL` | required | Postgres connection string |
| `base_url` | `POD_BASE_URL` | required | Public URL, sent in the feed poller's `User-Agent` |
| `listen_addr` | `POD_LISTEN_ADDR` | `0.0.0.0:3000` | HTTP listen address |
| `poll_interval_secs` | `POD_POLL_INTERVAL_SECS` | `900` | Seconds between polls of one feed |
| `poller_workers` | `POD_POLLER_WORKERS` | `4` | Concurrent feed poller workers |
| `fetch_allowlist` | `POD_FETCH_ALLOWLIST` (comma-separated) | empty | Host names, IPs or CIDRs the fetch guard allows on private networks |
| `allow_registration` | `POD_ALLOW_REGISTRATION` (`true`/`false`) | `true` | Whether new accounts can register |
| `client_ip_header` | `POD_CLIENT_IP_HEADER` | unset | Header holding the client IP (e.g. `X-Real-IP`), read only from `trusted_proxies` |
| `trusted_proxies` | `POD_TRUSTED_PROXIES` (comma-separated) | `127.0.0.0/8`, `::1/128` | IPs or CIDRs of reverse proxies allowed to set `client_ip_header` |
| `directory_base_url` | `POD_DIRECTORY_BASE_URL` | `https://itunes.apple.com` | iTunes-compatible search API for `/v1/directory/search` (`/search` is appended) |

Environment values win over the file. An empty env value counts as unset.
`RUST_LOG` sets the log filter (default `info`).

### LAN fetch allowlist

Feed fetches refuse loopback, private, link-local and other internal
addresses, so a feed URL can't be used to probe the server's network.
`fetch_allowlist` lifts that for feeds self-hosted on a LAN. Each entry is one
of:

- a host name (`nas.lan`): exact match, and every address it resolves to is
  allowed;
- an IP address (`192.168.1.20`): any host resolving to it is allowed;
- a CIDR (`10.0.0.0/8`): any host resolving inside it is allowed.

Other hosts may only use ports 80 and 443. A feed URL whose host is itself
allowlisted may use any port: `http://nas.lan:8080/feed.xml` with `nas.lan`
listed, or `http://192.168.1.20:8080/feed.xml` with `192.168.1.0/24` listed.
A host name that merely resolves inside a listed CIDR keeps the 80/443 rule;
list the name itself to open its ports. In the environment, separate
entries with commas: `POD_FETCH_ALLOWLIST=nas.lan,192.168.1.0/24`.

## Deploy

The server runs on a NixOS host with a local Postgres, reachable over the
tailnet behind nginx on the same machine. It ships as the flake's
`pod-server` package and NixOS module (see "NixOS module" below). Behind
nginx, set `client_ip_header = "X-Real-IP"` so the auth rate limit sees each
client's address instead of the proxy's; the module does that when it
configures nginx.

Run exactly one server process, since the feed poller and the auth rate
limits live in it. Every start applies pending migrations before the server
binds its port, and a failed migration makes the process exit.

## NixOS module

The flake exports `nixosModules.pod-server` (also `default`), which runs
`packages.<system>.pod-server` as the `pod-server` systemd unit under a static
`pod` user, with systemd sandboxing. The options:

| Option | Default | Meaning |
| --- | --- | --- |
| `services.pod-server.enable` | `false` | Run the server. |
| `package` | this flake's `pod-server` | The package to run. |
| `web.enable` | `true` | Serve the web client at every path the API doesn't claim (`web_dir`). |
| `web.package` | this flake's `pod-web` | The built web client, the contents of `web/dist`. |
| `settings` | | `pod.toml`, passed through `POD_CONFIG`. Any key from `pod.example.toml`; `base_url` is required. |
| `settings.listen_addr` | `127.0.0.1:3000` | Loopback, for nginx in front. |
| `settings.allow_registration` | `true` | Accounts only come from registration; turn it off once they exist. |
| `environmentFile` | `null` | A systemd `EnvironmentFile` with `POD_*` secrets, e.g. from agenix. |
| `database.createLocally` | `true` | A local Postgres with a `pod` database, reached over `/run/postgresql` with peer auth. |
| `backup.enable` | `false` | A daily `pg_dump` of the local database into `backup.location` (see "Backups and restore" below). Needs `database.createLocally`. |
| `backup.location` | `/var/backup/pod-server` | The dump directory, owned by `pod`, mode 0700. |
| `backup.startAt` | `*-*-* 03:15:00` | When to dump, in `systemd.time` format. The timer is persistent, so a run missed while the machine was off happens at boot. |
| `backup.keep` | `14` | How many dumps to keep; older ones are deleted after each dump. |
| `nginx.enable` | `false` | An nginx virtual host proxying to `listen_addr`. It sets `X-Real-IP`, and the module sets `client_ip_header = "X-Real-IP"`, so the auth rate limit sees real client addresses. |
| `nginx.virtualHost` | | The virtual host's name. |

The module leaves TLS and the firewall to the host. A host on the tailnet,
with a Tailscale certificate the host config manages:

```nix
{
  inputs.pod.url = "github:<owner>/pod/v2";

  outputs = { nixpkgs, pod, ... }: {
    nixosConfigurations.box = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        pod.nixosModules.default
        {
          services.pod-server = {
            enable = true;
            settings.base_url = "https://box.tailnet-name.ts.net";
            nginx.enable = true;
            nginx.virtualHost = "box.tailnet-name.ts.net";
          };
          # Merges with the virtual host the module defines.
          services.nginx.virtualHosts."box.tailnet-name.ts.net" = {
            forceSSL = true;
            sslCertificate = "/var/lib/tailscale-cert/box.crt";
            sslCertificateKey = "/var/lib/tailscale-cert/box.key";
          };
          networking.firewall.interfaces.tailscale0.allowedTCPPorts = [ 80 443 ];
        }
      ];
    };
  };
}
```

Admin commands run through `pod-server-admin`, which the module puts on the
system `PATH`. As root, it runs `pod-server admin` as the `pod` user with the
unit's config and environment file:

```bash
sudo pod-server-admin list-users
sudo pod-server-admin reset-password alice < new-password.txt
```

`nix build .#checks.x86_64-linux.pod-server` boots the module in a VM with
Postgres and nginx, registers an account through nginx and checks that it
survives a restart. It then dumps the database, registers a second account,
checks retention and runs the restore drill below: the first account is back
and the second is gone.

### Backups and restore

With `backup.enable`, the `pod-server-backup` timer runs `pg_dump
--format=custom` as the `pod` user and writes
`/var/backup/pod-server/pod-<UTC timestamp>.dump`, for example
`pod-20261026T031500Z.dump`. pg_dump compresses the custom format itself. A
dump in progress is named `….dump.partial` and renamed when complete, and
after each dump only the newest `backup.keep` dumps stay.

These dumps sit on the same disk as the database. Copying them off the
machine is the host's job:

- Add `/var/backup/pod-server` to the host's restic or borg paths, and
  exclude `*.partial`.
- Schedule that backup after `backup.startAt`, or order its unit
  `after = [ "pod-server-backup.service" ]`, so it picks up that night's dump.
- Run it as root: the directory and the dumps are readable only by `pod`.

The database is all the state the server has. `pod.toml` and the unit come
from the host's NixOS config, and secrets from `environmentFile`.

To restore, put the dump on the machine and run, as root:

```bash
sudo pod-server-restore /var/backup/pod-server/pod-20261026T031500Z.dump
```

It performs the restore drill:

1. Checks that the file is a custom-format dump, then stops `pod-server`.
2. Drops the `pod` database and creates it again, owned by the `pod` role.
3. Runs `pg_restore` as `pod`, in one transaction, so a failed restore
   leaves an empty database rather than a partial one.
4. Starts `pod-server`, which applies any migrations newer than the dump.

Then check it:

```bash
curl --fail http://127.0.0.1:3000/healthz
sudo pod-server-admin list-users
```

On a fresh machine, deploy the same config first, so Postgres and the `pod`
role exist, then restore.

Restoring an older dump rolls the server back to that day. Everything after
the dump is gone on the server: accounts registered since, sessions and
password resets (a session revoked since then is valid again, so repeat
any `reset-password` made after the dump), subscriptions, playback state and
settings. Clients don't notice the rollback on their own. Their sync cursors
are ahead of the server's version counters, and the server only sends rows
above a client's cursor, so a client skips new changes until the server's
counter passes its cursor again, and the changes made after the dump stay
only on the devices that made or pulled them. To recover, each user signs out
on every device (Settings, Sign out) and signs in again, which starts their
sync from scratch with the restored data. Signing in again after a "Signed
out" status is not enough, since it keeps the cursors. Changes that were
still unsent at sign-out are lost.

## Administration

`pod-server admin` runs one operator command against the configured database
and exits without serving. It reads the same config as the server and applies
pending migrations first.

```bash
pod-server admin list-users                    # id, username, created_at, live sessions
pod-server admin purge-episode <episode_id>    # tombstone an episode for every client
pod-server admin reset-password <username>     # reads the new password from stdin
```

`reset-password` reads one line from stdin, so the password stays out of shell
history and the process list, and it signs the user out on every device. A
purged episode stays deleted while its feed keeps carrying it unchanged; it
comes back only if the publisher changes that episode. On the server, run
them as `pod-server admin …` with the server's config (`POD_CONFIG` and the
same `POD_*` environment), so they reach the same database.

The server also runs a daily job that deletes expired sessions and
`applied_mutation` rows older than 30 days.

## Desktop client

`crates/pod-desktop` is the desktop app, a GPUI window built on `gpui-kit`
(see `docs/design.md`, "Desktop client"). It needs fontconfig, xkbcommon,
Wayland or X11, a Vulkan driver and libmpv on Linux; the flake provides them.

```bash
nix run .#pod-desktop             # build and open the window
nix develop -c cargo run -p pod-desktop
nix develop -c cargo test -p pod-desktop   # renders the shell headless
```

Logs go to stderr and to `pod-desktop.<date>.log` in the platform data
directory (`~/.local/share/pod/logs` on Linux), filtered by `RUST_LOG`
(default `info`). Without `WAYLAND_DISPLAY` or `DISPLAY`, the app exits with
an error instead of running without a window.

## Web client

`web/` is the browser client, SolidJS on Vite. It is online only: it reads
through the read endpoints (spec 4.5) and writes through `POST /v1/sync`, so
it keeps no copy of your library and has no downloads. pod-server serves the
built bundle when `web_dir` is set; the NixOS module does that by default
(`services.pod-server.web`).

```bash
cd web
npm install
npm run dev        # http://127.0.0.1:8001, with /v1 forwarded to a pod-server on :3000
npm test           # Vitest, including conformance/ids.json and the openapi.json drift check
npm run build      # type-checks, then writes web/dist for web_dir
npm run gen:api    # regenerates src/api/schema.ts after openapi.json changes
```

`nix build .#pod-web` builds the same bundle with its tests. After changing
`web/package-lock.json`, set the flake's `npmDepsHash` to `lib.fakeHash`,
build, and copy the hash Nix reports.
