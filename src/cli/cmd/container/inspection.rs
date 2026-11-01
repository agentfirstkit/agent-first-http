//! Runtime inspect shapes and container existence checks.
use super::Runtime;
use super::process::capture;

pub(super) fn image_exists(runtime: Runtime, image: &str) -> bool {
    capture(
        runtime.bin(),
        &["image".into(), "inspect".into(), image.to_string()],
    )
    .map(|o| o.status.success())
    .unwrap_or(false)
}

#[derive(Debug, Clone)]
pub(super) struct ContainerState {
    pub(super) running: bool,
    pub(super) exit_code: Option<i64>,
    /// The host port the runtime publishes the afhttp listener on, or `None`
    /// when it publishes none. The only port a token may be sent to.
    pub(super) host_port: Option<u16>,
}

fn inspect_container(runtime: Runtime, name: &str) -> Option<serde_json::Value> {
    let out = capture(runtime.bin(), &inspect_arguments(runtime, name)).ok()?;
    if !out.status.success() {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    inspected_name_matches(&value, name).then_some(value)
}

pub(super) fn inspect_container_state(runtime: Runtime, name: &str) -> Option<ContainerState> {
    let value = inspect_container(runtime, name)?;
    let mut state = parse_container_state(&value)?;
    state.host_port = published_host_port(&value);
    Some(state)
}

/// A bare Docker/Podman `inspect` also resolves images, volumes and networks,
/// so an absent container could be answered by the image of the same name.
/// Apple's `inspect` already covers containers only.
fn inspect_arguments(runtime: Runtime, name: &str) -> Vec<String> {
    if runtime == Runtime::Apple {
        vec!["inspect".into(), name.into()]
    } else {
        vec![
            "inspect".into(),
            "--type".into(),
            "container".into(),
            name.into(),
        ]
    }
}

/// Docker and Podman also resolve a container by ID prefix, so a name that
/// looks like hex could inspect a different container. They report the
/// resolved `Name` (Docker with a leading `/`); Apple's shape has none and
/// resolves names exactly.
fn inspected_name_matches(value: &serde_json::Value, name: &str) -> bool {
    let first = value
        .as_array()
        .and_then(|arr| arr.first())
        .unwrap_or(value);
    match first.get("Name").and_then(serde_json::Value::as_str) {
        Some(resolved) => resolved.trim_start_matches('/') == name,
        None => true,
    }
}

/// Read a container's state out of whichever `inspect` shape the runtime emits.
///
/// Docker and Podman answer `{"State": {"Running": bool, "ExitCode": n}}`.
/// Apple answers `{"status": {"state": "running"}}` — a string, under a
/// different key, with no exit code anywhere. Reading only the first shape made
/// this return `None` on Apple for every container, and the caller's fallback
/// then ran `container ps`, which that runtime does not have (`Plugin
/// 'container-ps' not found`). Between them, every container on Apple looked
/// stopped, and `install` reported a host that was running perfectly as having
/// "exited before /health became ready".
pub(super) fn parse_container_state(value: &serde_json::Value) -> Option<ContainerState> {
    let first = value
        .as_array()
        .and_then(|arr| arr.first())
        .unwrap_or(value);

    if let Some(state) = first.get("State") {
        return Some(ContainerState {
            running: state
                .get("Running")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            exit_code: state.get("ExitCode").and_then(serde_json::Value::as_i64),
            host_port: None,
        });
    }

    let state = first
        .get("status")
        .and_then(|status| status.get("state"))
        .and_then(serde_json::Value::as_str)?;
    Some(ContainerState {
        running: state.eq_ignore_ascii_case("running"),
        exit_code: None,
        host_port: None,
    })
}

/// The image a container was created from, as the reference it was run with.
pub(super) fn inspect_container_image(runtime: Runtime, name: &str) -> Option<String> {
    container_image_reference(&inspect_container(runtime, name)?)
}

/// Docker answers `Config.Image`, Podman `ImageName`, Apple
/// `configuration.image.reference`.
fn container_image_reference(value: &serde_json::Value) -> Option<String> {
    let first = value
        .as_array()
        .and_then(|arr| arr.first())
        .unwrap_or(value);
    [
        "/ImageName",
        "/Config/Image",
        "/configuration/image/reference",
    ]
    .iter()
    .find_map(|pointer| first.pointer(pointer).and_then(serde_json::Value::as_str))
    .map(str::to_string)
}

/// The port the entrypoint listens on inside the container: its `AFHTTP_PORT`,
/// which `install` sets, else the entrypoint's own default.
fn listener_port(first: &serde_json::Value) -> u16 {
    let env = first
        .pointer("/Config/Env")
        .or_else(|| first.pointer("/configuration/initProcess/environment"))
        .and_then(serde_json::Value::as_array);
    env.into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .find_map(|entry| entry.strip_prefix("AFHTTP_PORT="))
        .and_then(|port| port.trim().parse().ok())
        .unwrap_or(super::DEFAULT_CONTAINER_PORT)
}

/// The host port published for the container's afhttp listener.
///
/// Docker and Podman answer `NetworkSettings.Ports` — `{"9222/tcp":
/// [{"HostPort": "9333"}]}`, string ports — while the container runs, and keep
/// the requested mapping in `HostConfig.PortBindings` either way. Apple answers
/// `configuration.publishedPorts`, `[{"containerPort": 9222, "hostPort": 9333,
/// "count": 1, "proto": "tcp"}]`, numbers, where `count` widens one entry to a
/// range. A mapping for any other container port is not afhttp's and is never
/// an answer, so a container that publishes none reports `None` rather than a
/// guess.
pub(super) fn published_host_port(value: &serde_json::Value) -> Option<u16> {
    let first = value
        .as_array()
        .and_then(|arr| arr.first())
        .unwrap_or(value);
    let listener = listener_port(first);

    let key = format!("{listener}/tcp");
    for map in ["/NetworkSettings/Ports", "/HostConfig/PortBindings"] {
        let bindings = first
            .pointer(map)
            .and_then(|ports| ports.get(&key))
            .and_then(serde_json::Value::as_array);
        let port = bindings.into_iter().flatten().find_map(|binding| {
            binding
                .get("HostPort")
                .and_then(serde_json::Value::as_str)
                .and_then(|port| port.trim().parse::<u16>().ok())
                .filter(|port| *port != 0)
        });
        if port.is_some() {
            return port;
        }
    }

    let published = first
        .pointer("/configuration/publishedPorts")
        .and_then(serde_json::Value::as_array)?;
    published.iter().find_map(|entry| {
        let proto = entry
            .get("proto")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("tcp");
        if !proto.eq_ignore_ascii_case("tcp") {
            return None;
        }
        let container = u16::try_from(entry.get("containerPort")?.as_u64()?).ok()?;
        let host = u16::try_from(entry.get("hostPort")?.as_u64()?).ok()?;
        let count = entry
            .get("count")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(1)
            .max(1);
        let offset = u64::from(listener.checked_sub(container)?);
        if offset >= count {
            return None;
        }
        u16::try_from(u64::from(host) + offset)
            .ok()
            .filter(|port| *port != 0)
    })
}

pub(super) fn container_running(runtime: Runtime, name: &str) -> bool {
    if let Some(state) = inspect_container_state(runtime, name) {
        return state.running;
    }
    // Apple lists containers with `ls` and has no `ps` subcommand at all, so
    // asking for `ps` there failed and reported every container as stopped.
    let args = running_list_arguments(runtime);
    capture(runtime.bin(), &args)
        .map(|output| output.status.success() && lists_container(&output.stdout, name))
        .unwrap_or(false)
}

fn running_list_arguments(runtime: Runtime) -> Vec<String> {
    if runtime == Runtime::Apple {
        vec!["ls".into(), "--quiet".into()]
    } else {
        vec!["ps".into(), "--format".into(), "{{.Names}}".into()]
    }
}

fn lists_container(output: &[u8], name: &str) -> bool {
    !name.is_empty()
        && String::from_utf8_lossy(output)
            .lines()
            .any(|line| line.trim() == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspect_is_limited_to_the_named_container() {
        assert_eq!(
            inspect_arguments(Runtime::Docker, "reviewer"),
            ["inspect", "--type", "container", "reviewer"]
        );
        assert_eq!(
            inspect_arguments(Runtime::Apple, "reviewer"),
            ["inspect", "reviewer"]
        );
        let docker = serde_json::json!([{"Name": "/reviewer", "State": {"Running": true}}]);
        assert!(inspected_name_matches(&docker, "reviewer"));
        let podman = serde_json::json!([{"Name": "reviewer", "State": {"Running": true}}]);
        assert!(inspected_name_matches(&podman, "reviewer"));
        let prefix = serde_json::json!([{"Name": "/inspector", "State": {"Running": true}}]);
        assert!(!inspected_name_matches(&prefix, "abc123"));
        let apple = serde_json::json!([{"status": {"state": "running"}}]);
        assert!(inspected_name_matches(&apple, "reviewer"));
    }

    #[test]
    fn the_published_port_is_read_from_every_runtime_shape() {
        let docker = serde_json::json!([{
            "Name": "/reviewer",
            "Config": {"Env": ["PATH=/usr/bin", "AFHTTP_PORT=9222"]},
            "NetworkSettings": {"Ports": {
                "5900/tcp": [{"HostIp": "127.0.0.1", "HostPort": "5901"}],
                "9222/tcp": [{"HostIp": "127.0.0.1", "HostPort": "9333"}]
            }}
        }]);
        assert_eq!(published_host_port(&docker), Some(9333));

        // A stopped Docker/Podman container has no live mapping, only the
        // requested one.
        let stopped = serde_json::json!([{
            "Config": {"Env": ["AFHTTP_PORT=9400"]},
            "NetworkSettings": {"Ports": {}},
            "HostConfig": {"PortBindings": {"9400/tcp": [{"HostIp": "127.0.0.1", "HostPort": "9400"}]}}
        }]);
        assert_eq!(published_host_port(&stopped), Some(9400));

        let apple = serde_json::json!([{
            "configuration": {
                "initProcess": {"environment": ["AFHTTP_PORT=9222"]},
                "publishedPorts": [
                    {"containerPort": 80, "hostPort": 8080, "count": 1, "proto": "tcp"},
                    {"containerPort": 9222, "hostPort": 9444, "count": 1, "proto": "tcp"}
                ]
            },
            "status": {"state": "running"}
        }]);
        assert_eq!(published_host_port(&apple), Some(9444));

        let range = serde_json::json!([{
            "configuration": {
                "initProcess": {"environment": ["AFHTTP_PORT=9224"]},
                "publishedPorts": [{"containerPort": 9220, "hostPort": 19220, "count": 8, "proto": "tcp"}]
            }
        }]);
        assert_eq!(published_host_port(&range), Some(19224));
    }

    #[test]
    fn the_image_reference_is_read_from_every_runtime_shape() {
        let docker = serde_json::json!([{"Config": {"Image": "afhttp-host:1-brave"}}]);
        let podman = serde_json::json!([{"ImageName": "localhost/afhttp-host:1-brave"}]);
        let apple =
            serde_json::json!([{"configuration": {"image": {"reference": "afhttp-host:1-brave"}}}]);
        assert_eq!(
            container_image_reference(&docker).as_deref(),
            Some("afhttp-host:1-brave")
        );
        assert_eq!(
            container_image_reference(&podman).as_deref(),
            Some("localhost/afhttp-host:1-brave")
        );
        assert_eq!(
            container_image_reference(&apple).as_deref(),
            Some("afhttp-host:1-brave")
        );
    }

    /// A port belonging to something else is never where the token goes.
    #[test]
    fn a_container_without_the_listener_published_has_no_port() {
        let other = serde_json::json!([{
            "Config": {"Env": ["AFHTTP_PORT=9222"]},
            "NetworkSettings": {"Ports": {"80/tcp": [{"HostPort": "9222"}], "9222/tcp": null}}
        }]);
        assert_eq!(published_host_port(&other), None);
        let apple_udp = serde_json::json!([{
            "configuration": {
                "publishedPorts": [{"containerPort": 9222, "hostPort": 9222, "proto": "udp"}]
            }
        }]);
        assert_eq!(published_host_port(&apple_udp), None);
        let none = serde_json::json!([{"status": {"state": "running"}}]);
        assert_eq!(published_host_port(&none), None);
    }

    #[test]
    fn running_list_requires_a_complete_container_name() {
        let output = b"reviewer-backup\nmy-reviewer\nreviewer-2\n";
        assert!(!lists_container(output, "reviewer"));
        assert!(lists_container(output, "reviewer-2"));
        assert!(lists_container(b"reviewer\r\n", "reviewer"));
        assert!(!lists_container(b"", "reviewer"));
        assert!(!lists_container(b"\n", ""));
    }
}
