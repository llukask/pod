# Boots services.pod-server with its local Postgres and nginx, registers an
# account through the proxy and checks that it survives a restart.
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
    };
    networking.hosts."127.0.0.1" = [ "pod.test" ];
  };

  testScript = ''
    import json

    machine.wait_for_unit("pod-server.service")
    machine.wait_for_open_port(3000)
    machine.wait_for_unit("nginx.service")

    machine.succeed("curl --fail-with-body http://pod.test/healthz")

    register = json.dumps({"username": "alice", "password": "correct horse", "device_name": "VM test"})
    machine.succeed(
        "curl --fail-with-body -H 'Content-Type: application/json' "
        f"--data '{register}' http://pod.test/v1/auth/register"
    )

    def assert_alice_listed():
        users = machine.succeed("pod-server-admin list-users")
        assert "\talice\t" in users, users

    assert_alice_listed()
    machine.succeed("echo 'battery staple' | pod-server-admin reset-password alice")
    machine.systemctl("restart pod-server.service")
    machine.wait_for_unit("pod-server.service")
    machine.wait_for_open_port(3000)
    assert_alice_listed()

    login = json.dumps({"username": "alice", "password": "battery staple", "device_name": "VM test"})
    machine.succeed(
        "curl --fail-with-body -H 'Content-Type: application/json' "
        f"--data '{login}' http://pod.test/v1/auth/login"
    )

    # The server binds loopback only, behind nginx.
    machine.fail("ss -tlnH | grep -E '(0\\.0\\.0\\.0|\\[::\\]|\\*):3000'")

    print(machine.succeed("systemd-analyze security pod-server.service"))
  '';
}
