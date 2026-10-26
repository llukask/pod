# NixOS module for pod-server: the systemd unit, its config file, an optional
# local Postgres reached by peer auth with daily dumps and a restore command,
# and an optional nginx virtual host in front of it. TLS, firewalling and
# off-machine copies of the dumps stay with the host config.
{ self }:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.pod-server;
  settingsFormat = pkgs.formats.toml { };
  configFile = settingsFormat.generate "pod.toml" cfg.settings;

  # Peer auth maps the unix user to the Postgres role of the same name, so
  # the service user, the role and the database share this name.
  name = "pod";

  # `systemd-run` gives an admin command the service's user, config and
  # environment file without parsing the environment file in a shell.
  # `--pipe` passes stdin through, which `reset-password` reads.
  adminWrapper = pkgs.writeShellApplication {
    name = "pod-server-admin";
    text = ''
      exec ${config.systemd.package}/bin/systemd-run \
        --quiet --pipe --wait --collect \
        --uid=${name} --gid=${name} \
        --setenv=POD_CONFIG=${configFile} \
        ${
          lib.optionalString (cfg.environmentFile != null) "--property=EnvironmentFile=${cfg.environmentFile}"
        } \
        ${lib.getExe cfg.package} admin "$@"
    '';
  };

  postgres = config.services.postgresql.finalPackage;

  # Custom-format dumps are compressed by pg_dump itself and restore with
  # pg_restore. The names sort by time, which retention relies on.
  # `.partial` keeps a dump in progress out of the `pod-*.dump` glob that
  # retention and the host's backup tool see.
  backupScript = pkgs.writeShellApplication {
    name = "pod-server-backup";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.findutils
      postgres
    ];
    text = ''
      cd ${lib.escapeShellArg cfg.backup.location}
      find . -maxdepth 1 -name 'pod-*.dump.partial' -delete
      dump="pod-$(date -u +%Y%m%dT%H%M%SZ).dump"
      pg_dump --format=custom --file="$dump.partial" ${name}
      mv "$dump.partial" "$dump"
      find . -maxdepth 1 -name 'pod-*.dump' -printf '%f\n' \
        | sort -r \
        | tail -n +${toString (cfg.backup.keep + 1)} \
        | xargs --no-run-if-empty rm --
    '';
  };

  # The restore drill as one command, run as root. The dump comes in on
  # stdin, so it may sit anywhere root can read, and pg_restore runs as the
  # `pod` role, so every restored object belongs to the owner the server
  # connects as. `--single-transaction` leaves an empty database rather than
  # half a restore when it fails.
  restoreScript = pkgs.writeShellApplication {
    name = "pod-server-restore";
    runtimeInputs = [
      config.systemd.package
      pkgs.util-linux
      postgres
    ];
    text = ''
      if [ "$#" -ne 1 ]; then
        echo "usage: pod-server-restore <dump>" >&2
        exit 2
      fi
      dump=$1
      pg_restore --list "$dump" >/dev/null

      systemctl stop pod-server.service
      runuser -u postgres -- dropdb --force --if-exists ${name}
      runuser -u postgres -- createdb --owner=${name} ${name}
      runuser -u ${name} -- pg_restore --dbname=${name} --no-owner \
        --exit-on-error --single-transaction <"$dump"
      # The server applies migrations newer than the dump on start.
      systemctl start pod-server.service
    '';
  };
in
{
  options.services.pod-server = {
    enable = lib.mkEnableOption "the Pod sync server";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.pod-server;
      defaultText = lib.literalExpression "pod.packages.\${pkgs.stdenv.hostPlatform.system}.pod-server";
      description = "The pod-server package to run.";
    };

    settings = lib.mkOption {
      description = ''
        Server configuration, written to `pod.toml` and passed through
        `POD_CONFIG`. `pod.example.toml` in the repository documents every
        key. The server rejects unknown keys at startup.
      '';
      default = { };
      type = lib.types.submodule {
        freeformType = settingsFormat.type;
        options = {
          base_url = lib.mkOption {
            type = lib.types.str;
            example = "https://pod.tailnet-name.ts.net";
            description = "Public URL of the server. Feed hosts see it in the poller's User-Agent.";
          };
          listen_addr = lib.mkOption {
            type = lib.types.str;
            default = "127.0.0.1:3000";
            description = "Address the HTTP server binds. Loopback by default, for a reverse proxy in front.";
          };
          allow_registration = lib.mkOption {
            type = lib.types.bool;
            default = true;
            description = ''
              Whether anyone who reaches the server may create an account.
              Accounts only come from registration, so turn it off after
              creating them.
            '';
          };
        };
      };
    };

    environmentFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      example = "/run/agenix/pod-server.env";
      description = ''
        A systemd `EnvironmentFile` with `POD_*` variables, for secrets that
        must stay out of the Nix store, such as a `POD_DATABASE_URL` with a
        password. Read by the unit and by `pod-server-admin`.
      '';
    };

    database.createLocally = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Whether to run a local Postgres with a `pod` database owned by the
        `pod` role, reached over the unix socket with peer auth. Without it,
        set `database_url` in `settings` or `POD_DATABASE_URL` in
        `environmentFile`.
      '';
    };

    backup = {
      enable = lib.mkEnableOption ''
        a scheduled `pg_dump` of the local `pod` database into
        `backup.location`. Copying the dumps off the machine is left to the
        host's backup tool
      '';
      location = lib.mkOption {
        type = lib.types.path;
        default = "/var/backup/pod-server";
        description = ''
          Directory of the dumps, owned by `pod` with mode 0700. Each dump is
          a custom-format `pg_dump` named `pod-<UTC timestamp>.dump`; a dump
          in progress ends in `.partial`.
        '';
      };
      startAt = lib.mkOption {
        type = with lib.types; either str (listOf str);
        default = "*-*-* 03:15:00";
        description = ''
          When to dump, in `systemd.time` calendar format. The timer is
          persistent, so a run missed while the machine was off happens at
          the next boot.
        '';
      };
      keep = lib.mkOption {
        type = lib.types.ints.positive;
        default = 14;
        description = ''
          How many dumps to keep. After each dump the oldest beyond this
          count are deleted. With the daily default, that is two weeks.
        '';
      };
    };

    nginx = {
      enable = lib.mkEnableOption "an nginx virtual host proxying to pod-server";
      virtualHost = lib.mkOption {
        type = lib.types.str;
        example = "pod.tailnet-name.ts.net";
        description = ''
          Name of the nginx virtual host. TLS is left to the host config,
          which can set `services.nginx.virtualHosts.<name>.sslCertificate`,
          `forceSSL` and so on; those merge with what this module sets.
        '';
      };
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = cfg.backup.enable -> cfg.database.createLocally;
        message = "services.pod-server.backup dumps the local database; back up a remote one where it runs.";
      }
    ];

    services.pod-server.settings = lib.mkMerge [
      # sqlx would otherwise take the role from the process user, which it
      # can't resolve inside the sandbox (it falls back to "anonymous").
      (lib.mkIf cfg.database.createLocally {
        database_url = "postgresql:///${name}?host=/run/postgresql&user=${name}";
      })
      # nginx overwrites the header with the address it saw, and pod-server
      # trusts it only from loopback peers (`trusted_proxies`), so the auth
      # rate limit keys on the real client.
      (lib.mkIf cfg.nginx.enable { client_ip_header = lib.mkDefault "X-Real-IP"; })
    ];

    users.users.${name} = {
      isSystemUser = true;
      group = name;
    };
    users.groups.${name} = { };

    environment.systemPackages = [
      adminWrapper
    ]
    ++ lib.optional cfg.database.createLocally restoreScript;

    services.postgresql = lib.mkIf cfg.database.createLocally {
      enable = true;
      ensureDatabases = [ name ];
      ensureUsers = [
        {
          inherit name;
          ensureDBOwnership = true;
        }
      ];
    };

    systemd.services.pod-server = {
      description = "Pod sync server";
      wantedBy = [ "multi-user.target" ];
      # The poller fetches feeds as soon as it starts.
      wants = [ "network-online.target" ];
      # postgresql.target also covers postgresql-setup.service, which creates
      # the role and database.
      requires = lib.optional cfg.database.createLocally "postgresql.target";
      after = [ "network-online.target" ] ++ lib.optional cfg.database.createLocally "postgresql.target";

      environment.POD_CONFIG = configFile;

      serviceConfig = {
        ExecStart = lib.getExe cfg.package;
        EnvironmentFile = lib.mkIf (cfg.environmentFile != null) cfg.environmentFile;
        User = name;
        Group = name;
        Restart = "on-failure";
        RestartSec = 5;

        # The server keeps no files; everything lives in Postgres.
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        PrivateDevices = true;
        PrivateUsers = true;
        NoNewPrivileges = true;
        CapabilityBoundingSet = "";
        AmbientCapabilities = "";
        ProtectClock = true;
        ProtectControlGroups = true;
        ProtectHostname = true;
        ProtectKernelLogs = true;
        ProtectKernelModules = true;
        ProtectKernelTunables = true;
        ProtectProc = "invisible";
        ProcSubset = "pid";
        # Unix for the Postgres socket, IP for clients and outbound feeds.
        RestrictAddressFamilies = [
          "AF_UNIX"
          "AF_INET"
          "AF_INET6"
        ];
        RestrictNamespaces = true;
        RestrictRealtime = true;
        RestrictSUIDSGID = true;
        LockPersonality = true;
        MemoryDenyWriteExecute = true;
        RemoveIPC = true;
        SystemCallArchitectures = "native";
        SystemCallFilter = [
          "@system-service"
          "~@privileged"
          "~@resources"
        ];
        UMask = "0077";
      };
    };

    systemd.tmpfiles.rules = lib.mkIf cfg.backup.enable [
      "d ${cfg.backup.location} 0700 ${name} ${name} - -"
    ];

    # Runs as `pod`, the database owner, over peer auth.
    systemd.services.pod-server-backup = lib.mkIf cfg.backup.enable {
      description = "Dump the Pod database";
      requires = [ "postgresql.target" ];
      after = [ "postgresql.target" ];
      startAt = cfg.backup.startAt;
      serviceConfig = {
        Type = "oneshot";
        ExecStart = lib.getExe backupScript;
        User = name;
        Group = name;
        UMask = "0077";
        ProtectSystem = "strict";
        ReadWritePaths = [ cfg.backup.location ];
        ProtectHome = true;
        PrivateTmp = true;
        PrivateDevices = true;
        PrivateNetwork = true;
        NoNewPrivileges = true;
        CapabilityBoundingSet = "";
        RestrictAddressFamilies = [ "AF_UNIX" ];
      };
    };
    systemd.timers.pod-server-backup = lib.mkIf cfg.backup.enable {
      timerConfig.Persistent = true;
    };

    services.nginx = lib.mkIf cfg.nginx.enable {
      enable = true;
      virtualHosts.${cfg.nginx.virtualHost}.locations."/" = {
        proxyPass = "http://${cfg.settings.listen_addr}";
        # The recommended set would add its own X-Real-IP, so this location
        # sets the one header pod-server reads itself.
        recommendedProxySettings = false;
        extraConfig = ''
          proxy_set_header X-Real-IP $remote_addr;
          # axum's default body limit. A sync push carries at most 200
          # mutations (spec 4.2), well below it.
          client_max_body_size 2m;
        '';
      };
    };
  };
}
