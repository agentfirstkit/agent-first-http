"""Internal normalized inspect helpers for the maintained deployment entrypoint."""

import json
from pathlib import Path
import subprocess
import sys


def require_data_volume(state):
    if "configuration" in state:
        mounts = state["configuration"].get("mounts", [])
        valid = any(
            mount.get("destination") == "/data"
            and mount.get("type", {}).get("volume", {}).get("name") == "afhttp-host-data"
            for mount in mounts
        )
    else:
        valid = any(
            mount.get("Destination") == "/data"
            and mount.get("Type") == "volume"
            and mount.get("Name") == "afhttp-host-data"
            for mount in state.get("Mounts", [])
        )
    if not valid:
        raise ValueError("expected the existing afhttp-host-data volume at /data")


def container_image(state):
    require_data_volume(state)
    if "configuration" in state:
        return state["configuration"]["image"]["reference"]
    return state["Image"]


def image_revision(state):
    if "variants" in state:
        revisions = {
            (variant.get("config", {}).get("config", {}).get("Labels") or {}).get(
                "org.opencontainers.image.revision"
            )
            for variant in state["variants"]
        }
        if len(revisions) != 1:
            raise ValueError("image variants have inconsistent source revisions")
        revision = revisions.pop()
    else:
        revision = (state.get("Config", {}).get("Labels") or {}).get(
            "org.opencontainers.image.revision"
        )
    if not revision:
        raise ValueError("image has no source revision label")
    return revision


def main():
    action, binary = sys.argv[1:3]
    if binary not in ("container", "podman", "docker"):
        raise ValueError("unsupported container runtime")
    if action == "stored-image":
        print(container_image(json.loads(Path(sys.argv[3]).read_text())[0]))
    elif action == "revision":
        state = json.loads(subprocess.check_output([binary, "image", "inspect", sys.argv[3]]))[0]
        print(image_revision(state))
    else:
        state = json.loads(subprocess.check_output([binary, "inspect", "afhttp-host"]))[0]
        if action == "image":
            print(container_image(state))
        elif action == "volume":
            require_data_volume(state)
        else:
            raise ValueError("unknown inspect action")


if __name__ == "__main__":
    main()
