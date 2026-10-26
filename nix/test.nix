# Boots services.pod-server with its local Postgres, backups and nginx,
# registers an account through the proxy, checks that it survives a restart,
# then dumps the database and runs the restore drill on the dump.
{ self, pkgs }:
pkgs.testers.runNixOSTest {
  name = "pod-server";

  nodes.machine = {
    imports = [ self.nixosModules.pod-server ];
    services.pod-server = {
      enable = true;
      settings = {
        base_url = "http://pod.test";
        allow_registration = true;
      };
      nginx = {
        enable = true;
        virtualHost = "pod.test";
      };
      backup = {
        enable = true;
        keep = 2;
      };
    };
    networking.hosts."127.0.0.1" = [ "pod.test" ];
  };

  testScript = ''
    import json

    machine.wait_for_unit("pod-server.service")
    machine.wait_for_open_port(3000)
    machine.wait_for_unit("nginx.service")

    machine.succeed("curl --fail-with-body http://pod.test/healthz")

    def register(username):
        body = json.dumps({"username": username, "password": "correct horse", "device_name": "VM test"})
        machine.succeed(
            "curl --fail-with-body -H 'Content-Type: application/json' "
            f"--data '{body}' http://pod.test/v1/auth/register"
        )

    def listed(username):
        return f"\t{username}\t" in machine.succeed("pod-server-admin list-users")

    def assert_alice_listed():
        assert listed("alice"), "alice is listed"

    register("alice")
    assert_alice_listed()
    machine.succeed("echo 'battery staple' | pod-server-admin reset-password alice")
    machine.systemctl("restart pod-server.service")
    machine.wait_for_unit("pod-server.service")
    machine.wait_for_open_port(3000)
    assert_alice_listed()

    def assert_alice_logs_in():
        login = json.dumps({"username": "alice", "password": "battery staple", "device_name": "VM test"})
        machine.succeed(
            "curl --fail-with-body -H 'Content-Type: application/json' "
            f"--data '{login}' http://pod.test/v1/auth/login"
        )

    assert_alice_logs_in()

    # The server binds loopback only, behind nginx.
    machine.fail("ss -tlnH | grep -E '(0\\.0\\.0\\.0|\\[::\\]|\\*):3000'")

    print(machine.succeed("systemd-analyze security pod-server.service"))

    backups = "/var/backup/pod-server"

    def dumps():
        return machine.succeed(f"ls {backups}").split()

    machine.succeed("systemctl list-timers pod-server-backup.timer | grep -q pod-server-backup")
    machine.succeed("stat -c '%U %a' " + backups + " | grep -qx 'pod 700'")

    machine.systemctl("start pod-server-backup.service")
    [first] = dumps()
    assert first.startswith("pod-") and first.endswith(".dump"), first
    machine.succeed(f"stat -c '%U %a' {backups}/{first} | grep -qx 'pod 600'")
    machine.succeed(f"cp {backups}/{first} /root/alice-only.dump")

    register("bob")
    assert listed("bob"), "bob is listed"

    # Dump names have one-second resolution.
    for _ in range(2):
        machine.succeed("sleep 1")
        machine.systemctl("start pod-server-backup.service")
    kept = dumps()
    assert len(kept) == 2 and first not in kept, f"keep = 2 rotated out the oldest dump: {kept}"

    machine.fail("pod-server-restore /root/missing.dump")
    machine.succeed("systemctl is-active pod-server.service")

    machine.succeed("pod-server-restore /root/alice-only.dump")
    machine.wait_for_unit("pod-server.service")
    machine.wait_for_open_port(3000)
    machine.succeed("curl --fail-with-body http://pod.test/healthz")
    assert_alice_listed()
    assert not listed("bob"), "bob registered after the dump and is gone"
    assert_alice_logs_in()
    owners = machine.succeed(
        "runuser -u postgres -- psql -tA pod -c "
        "\"select distinct tableowner from pg_tables where schemaname = 'public'\""
    ).split()
    assert owners == ["pod"], f"restored tables belong to pod: {owners}"
  '';
}
