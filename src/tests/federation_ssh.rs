// Owns federation SSH argv regression coverage, not real SSH or host enrollment.
// New coverage alongside tests/remote.rs; no source was split from that file.

use super::*;

fn satellite_ssh_config() -> RemoteConfig {
    RemoteConfig {
        id: "satellite".to_owned(),
        name: "Satellite host".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("satellite.example".to_owned()),
        port: Some(2222),
        user: Some("operator".to_owned()),
    }
}

fn federation_forwards() -> FederationForwards {
    FederationForwards {
        local_forward_port: 47101,
        satellite_listener_port: 47102,
        reverse_forward_port: 47103,
        central_listener_port: 47104,
        host_key_alias: "pair-host-key".to_owned(),
    }
}

#[test]
fn federation_ssh_uses_one_strict_batch_session_and_two_loopback_forwards() {
    let args = federation_ssh_command_args(
        &satellite_ssh_config(),
        &federation_forwards(),
        FsPath::new(".tmp/pair-known-hosts"),
    )
    .unwrap();
    for option in [
        "BatchMode=yes",
        "StrictHostKeyChecking=yes",
        "ExitOnForwardFailure=yes",
        "ServerAliveInterval=15",
        "ServerAliveCountMax=3",
        "GatewayPorts=no",
        "ControlMaster=no",
        "ControlPath=none",
        "ControlPersist=no",
        "GlobalKnownHostsFile=none",
        "UpdateHostKeys=no",
        "VerifyHostKeyDNS=no",
        "CheckHostIP=no",
        "HostKeyAlias=pair-host-key",
    ] {
        assert!(
            args.windows(2).any(|pair| pair == ["-o", option]),
            "{option}"
        );
    }
    assert!(
        args.windows(2)
            .any(|pair| pair == ["-L", "127.0.0.1:47101:127.0.0.1:47102"])
    );
    assert!(
        args.windows(2)
            .any(|pair| pair == ["-R", "127.0.0.1:47103:127.0.0.1:47104"])
    );
    assert!(args.iter().any(|arg| arg == "-N"));
    assert!(
        args.iter()
            .any(|arg| arg == "UserKnownHostsFile=.tmp/pair-known-hosts")
    );
    assert_eq!(
        &args[args.len() - 2..],
        ["--", "operator@satellite.example"]
    );
    assert!(!args.iter().any(|arg| arg == "server"));
}

#[test]
fn federation_ssh_rejects_disabled_or_local_configs_and_unusable_ports() {
    let mut remote = satellite_ssh_config();
    remote.enabled = false;
    assert!(
        federation_ssh_command_args(&remote, &federation_forwards(), FsPath::new("known-hosts"))
            .is_err()
    );
    assert!(
        federation_ssh_command_args(
            &RemoteConfig::local(),
            &federation_forwards(),
            FsPath::new("known-hosts")
        )
        .is_err()
    );
    for position in 0..4 {
        let mut forwards = federation_forwards();
        match position {
            0 => forwards.local_forward_port = 0,
            1 => forwards.satellite_listener_port = 0,
            2 => forwards.reverse_forward_port = 0,
            _ => forwards.central_listener_port = 0,
        }
        assert!(
            federation_ssh_command_args(
                &satellite_ssh_config(),
                &forwards,
                FsPath::new("known-hosts")
            )
            .is_err()
        );
    }
}

#[test]
fn federation_ssh_rejects_option_injection_and_ambiguous_known_hosts_paths() {
    for host in [
        "-oProxyCommand=whoami",
        "satellite\n-oStrictHostKeyChecking=no",
    ] {
        let mut remote = satellite_ssh_config();
        remote.host = Some(host.to_owned());
        assert!(
            federation_ssh_command_args(
                &remote,
                &federation_forwards(),
                FsPath::new("known-hosts")
            )
            .is_err()
        );
    }
    for path in [
        "",
        "known-hosts\nIdentityFile=other",
        "known-hosts\0other",
        "%h-known-hosts",
        "~/.ssh/known_hosts",
        "known\"hosts",
    ] {
        assert!(
            federation_ssh_command_args(
                &satellite_ssh_config(),
                &federation_forwards(),
                FsPath::new(path)
            )
            .is_err()
        );
    }
}

#[test]
fn federation_ssh_quotes_one_pin_path_and_rejects_alias_option_injection() {
    let path = FsPath::new(".tmp/pin with spaces");
    let args =
        federation_ssh_command_args(&satellite_ssh_config(), &federation_forwards(), path).unwrap();
    assert!(
        args.windows(2)
            .any(|pair| pair == ["-o", "UserKnownHostsFile=\".tmp/pin with spaces\""])
    );
    assert!(args.windows(2).any(|pair| pair == ["-F", "none"]));
    let mut forwards = federation_forwards();
    forwards.host_key_alias = "alias IdentityFile=other".to_owned();
    assert!(federation_ssh_command_args(&satellite_ssh_config(), &forwards, path).is_err());
}
