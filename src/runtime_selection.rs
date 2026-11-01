//! Canonical detection policy, copied into independently built repositories.

pub(crate) fn detect_runtime() -> Option<&'static str> {
    detect_with(cfg!(target_os = "macos"), |binary| {
        let Some(paths) = std::env::var_os("PATH") else {
            return false;
        };
        std::env::split_paths(&paths).any(|dir| {
            let path = dir.join(binary);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                path.metadata().is_ok_and(|metadata| {
                    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                })
            }
            #[cfg(not(unix))]
            {
                path.is_file() || path.with_extension("exe").is_file()
            }
        })
    })
}

fn detect_with(is_macos: bool, available: impl Fn(&str) -> bool) -> Option<&'static str> {
    [
        ("apple", "container"),
        ("podman", "podman"),
        ("docker", "docker"),
    ]
    .into_iter()
    .find(|(runtime, binary)| (*runtime != "apple" || is_macos) && available(binary))
    .map(|(runtime, _)| runtime)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_returns_only_supported_runtime_names() {
        assert!(matches!(
            detect_runtime(),
            None | Some("apple" | "podman" | "docker")
        ));
    }

    #[test]
    fn runtime_priority_covers_every_availability_combination() {
        for is_macos in [false, true] {
            for mask in 0..8 {
                let apple = mask & 1 != 0;
                let podman = mask & 2 != 0;
                let docker = mask & 4 != 0;
                let expected = match (is_macos && apple, podman, docker) {
                    (true, _, _) => Some("apple"),
                    (_, true, _) => Some("podman"),
                    (_, _, true) => Some("docker"),
                    _ => None,
                };
                assert_eq!(
                    detect_with(is_macos, |binary| match binary {
                        "container" => apple,
                        "podman" => podman,
                        "docker" => docker,
                        _ => false,
                    }),
                    expected
                );
            }
        }
    }
}
