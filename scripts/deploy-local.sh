#!/usr/bin/env bash
# shellcheck source-path=SCRIPTDIR
# Rebuild the selected runtime's standard host locally, retaining its data volume and a
# private, stopped-container backup. No registry push or version mutation.
set -euo pipefail
umask 077

_AFDATA_BASH_SOURCE="$("${AFDATA_BIN:-afdata}" shell bash)"
# shellcheck source=/dev/stdin
source /dev/stdin <<<"$_AFDATA_BASH_SOURCE"
unset _AFDATA_BASH_SOURCE

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SOURCE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

runtime="${AFHTTP_CONTAINER_RUNTIME:-}"
driver="${AFHTTP_BIN:-afhttp}"
profile=""
backup_root="${XDG_STATE_HOME:-$HOME/.local/state}/afhttp/local-deploy"
diagnose_image=""
resume_backup=""
verify_backup_dir=""
afdata_args_begin "deploy-local.sh --profile NAME [OPTIONS]"
afdata_args_option runtime --runtime NAME "apple, podman, or docker; default: automatic" "$runtime"
afdata_args_option driver --driver PATH "Locally built/installed afhttp driver" "$driver"
afdata_args_option profile --profile NAME "Existing active persistent profile to preserve" ""
afdata_args_option backup_root --backup-root DIR "Private backup directory on the Mac/host filesystem" "$backup_root"
afdata_args_option diagnose_image --diagnose-image IMAGE "Run a bounded isolated display startup diagnostic without profile access" ""
afdata_args_option resume_backup --resume-backup DIR "Resume a failed deployment using its existing private backup and built image" ""
afdata_args_option verify_backup_dir --verify-backup DIR "Verify retained profile files and host token against a private backup without redeploying" ""
afdata_args_parse "$@"

fail() { afdata_error "$1" "$2" || :; exit 1; }
# shellcheck source=container-runtime.sh
source "$SCRIPT_DIR/container-runtime.sh"
af_container_resolve "$runtime"
afdata_run --quiet af_container_start
verify_backup() {
  afdata_run python3 - "$1" "$CONTAINER_BIN" "$SCRIPT_DIR" <<'PY'
import hashlib
import json
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, sys.argv[3])
from container_state import require_data_volume
binary = sys.argv[2]
backup = Path(sys.argv[1]).resolve(strict=True)
data = backup / 'data'
profiles = data / 'afhttp' / 'profiles'
if not profiles.is_dir():
    raise SystemExit('Profile backup missing; verification stopped')
state = json.loads(subprocess.check_output([binary, 'inspect', 'afhttp-host']))[0]
require_data_volume(state)
token = data / 'afhttp' / 'host-token'
expected = hashlib.sha256(token.read_bytes()).hexdigest()
actual = subprocess.check_output([binary, 'exec', 'afhttp-host', 'sha256sum',
                                  '/data/afhttp/host-token'], text=True).split()[0]
if expected != actual:
    raise SystemExit('Host token differs from the stopped-container backup')
checked = 0
cookie_files = 0
profile_dirs = [profile for backend in profiles.iterdir() if backend.is_dir()
                for profile in backend.iterdir() if profile.is_dir() and not profile.is_symlink()]
for path in profile_dirs:
    subprocess.run([binary, 'exec', 'afhttp-host', 'test', '-d',
                    '/data/' + str(path.relative_to(data))], check=True)
for name in ('afhttp-profile.json', 'Cookies', 'Local State'):
    for path in profiles.rglob(name):
        if not path.is_file() or path.is_symlink():
            continue
        subprocess.run([binary, 'exec', 'afhttp-host', 'test', '-f',
                        '/data/' + str(path.relative_to(data))], check=True)
        checked += 1
        cookie_files += name == 'Cookies'
report = {'data_volume_preserved': True, 'host_token_preserved': True,
          'retained_profile_count': len(profile_dirs),
          'checked_profile_files': checked, 'checked_cookie_files': cookie_files}
(backup / 'preservation-check.json').write_text(json.dumps(report, indent=2) + '\n')
PY
}
if [ -n "$verify_backup_dir" ]; then
  verify_backup "$verify_backup_dir"
  afdata_result "Original profile directories, cookie files, metadata and host token verified against the private backup"
  exit 0
fi
if [ -n "$diagnose_image" ]; then
  probe_status=0
  "$CONTAINER_BIN" run --rm --entrypoint /usr/bin/timeout "$diagnose_image" 3 \
    /usr/bin/Xvnc :90 -geometry 1280x720 -depth 24 -interface 127.0.0.1 \
    -rfbport 23456 -websocketPort 23457 -httpd /usr/share/kasmvnc/www \
    -sslOnly=0 -SecurityTypes None -disableBasicAuth -PublicIP 127.0.0.1 \
    -Log '*:stderr:30' || probe_status=$?
  [ "$probe_status" -eq 124 ] || fail display_startup_failed "The isolated display exited before its diagnostic deadline"
  afdata_result "Display process stayed alive until the diagnostic deadline; no profiles were mounted"
  exit 0
fi
[ -n "$profile" ] || fail missing_profile "Pass the existing active persistent profile with --profile"
command -v python3 >/dev/null || fail runtime_unavailable "Python 3 is required"
command -v "$driver" >/dev/null || fail driver_unavailable "The local afhttp driver is unavailable"

# Refuse known fetch/UI owners before changing the standard host.
busy="$(ps -axo pid=,command= | awk '/afhttp (fetch|ui takeover)/ || /AFUI Review[.]app\// {print $1}')"
[ -z "$busy" ] || fail host_in_use "A fetch or takeover process is active; finish it before deploying"
afdata_run --quiet python3 "$SCRIPT_DIR/container_state.py" volume "$CONTAINER_BIN"

stamp="$(date -u +%Y%m%dT%H%M%SZ)-$$"
backup_dir="$backup_root/$stamp"
if [ -n "$resume_backup" ]; then
  backup_dir="$resume_backup"
  stamp="$(basename "$backup_dir")"
fi
afdata_cli guard path "$backup_dir" >/dev/null
mkdir -p "$backup_dir"
if [ -z "$resume_backup" ]; then
  "$driver" container status --runtime "$CONTAINER_RUNTIME" > "$backup_dir/status-before.json"
fi
active_profile="$(afdata_config_get "$backup_dir/status-before.json" result.profile)"
profile_kind="$(afdata_config_get "$backup_dir/status-before.json" result.profile_kind)"
[ "$active_profile" = "$profile" ] && [ "$profile_kind" = persistent ] || fail profile_mismatch "Requested profile is not the active persistent profile; refusing a profile switch"
"$driver" --version > "$backup_dir/driver-version.json"
version="$(afdata_config_get "$backup_dir/driver-version.json" result.version)"
revision="$(git -C "$SOURCE_DIR" rev-parse HEAD)"
if [ -n "$resume_backup" ]; then
  old_image="$(python3 "$SCRIPT_DIR/container_state.py" stored-image "$CONTAINER_BIN" "$backup_dir/container-before.json")"
  [ -d "$backup_dir/data/afhttp/profiles" ] || fail backup_missing "The stopped-container profile backup is missing"
else
  old_image="$(python3 "$SCRIPT_DIR/container_state.py" image "$CONTAINER_BIN")"
fi
canonical_image="afhttp-host:$version"
rollback_image="afhttp-host:local-rollback-$stamp"
candidate_image="afhttp-host:local-$stamp"
afdata_run "$CONTAINER_BIN" image tag "$old_image" "$rollback_image"
if [ -z "$resume_backup" ]; then
  "$CONTAINER_BIN" inspect afhttp-host > "$backup_dir/container-before.json"
fi

available_kib="$("$CONTAINER_BIN" run --rm --mount type=volume,source=afhttp-host-data,target=/data,readonly --entrypoint /bin/df "$old_image" -Pk /data | awk 'NR == 2 {print $4}')"
case "$available_kib" in ''|*[!0-9]*) fail disk_probe_failed "Could not determine available data-volume space" ;; esac
[ "$available_kib" -ge 1048576 ] || fail disk_full "Less than 1 GiB is writable; free build storage before deploying; the old host has not been stopped"

if [ -z "$resume_backup" ]; then
  afdata_log info "Building a source image before stopping the existing host"
  build_args=()
  if [ "$CONTAINER_RUNTIME" = apple ]; then
    build_args=(--cpus "${AFHTTP_TEST_BUILDER_CPUS:-8}" --memory "${AFHTTP_TEST_BUILDER_MEMORY:-16g}")
  fi
  afdata_run "$CONTAINER_BIN" build ${build_args[@]+"${build_args[@]}"} --file "$SOURCE_DIR/container/docker/Dockerfile" \
    --tag "$candidate_image" --label "org.opencontainers.image.revision=$revision" \
    --build-arg WITH_BRAVE=1 --build-arg WITH_KASMVNC=1 "$SOURCE_DIR"
fi
available_kib="$("$CONTAINER_BIN" run --rm --mount type=volume,source=afhttp-host-data,target=/data,readonly --entrypoint /bin/df "$old_image" -Pk /data | awk 'NR == 2 {print $4}')"
case "$available_kib" in ''|*[!0-9]*) fail disk_probe_failed "Could not determine remaining space after the build" ;; esac
[ "$available_kib" -ge 1048576 ] || fail disk_full "Build completed but less than 1 GiB remains writable; the old host has not been stopped"
image_revision="$(python3 "$SCRIPT_DIR/container_state.py" revision "$CONTAINER_BIN" "$candidate_image")"
[ -n "$image_revision" ] || fail image_unverified "The source image has no revision label"
revision="$image_revision"

# The backup is consistent: browser and host have stopped before any profile
# file is copied. Keep all profiles and the stable token, not just one profile.
if [ -z "$resume_backup" ]; then
  afdata_run "$CONTAINER_BIN" stop --time 30 afhttp-host
  if ! "$CONTAINER_BIN" cp afhttp-host:/data/. "$backup_dir/data"; then
    "$CONTAINER_BIN" start afhttp-host >/dev/null || :
    fail backup_failed "Data backup failed; the old container was retained and restart was attempted"
  fi
fi
afdata_log info "Stopped-container data backup saved to $backup_dir/data"
afdata_run "$CONTAINER_BIN" image tag "$candidate_image" "$canonical_image"

# The maintained installer reuses this already-built image and the same named
# volume. It does not use --purge and never downloads a release in this path.
if ! "$driver" container install --runtime "$CONTAINER_RUNTIME" --profile "$profile" \
  --takeover-provider kasmvnc -- --browser brave; then
  afdata_log warn "New host failed; restoring the prior image with the same data volume"
  "$CONTAINER_BIN" image tag "$rollback_image" "$canonical_image"
  "$driver" container install --runtime "$CONTAINER_RUNTIME" --profile "$profile" \
    --takeover-provider kasmvnc -- --browser brave || :
  fail deployment_failed "New host failed; inspect host health and the private backup before proceeding"
fi
"$driver" container status --runtime "$CONTAINER_RUNTIME" > "$backup_dir/status-after.json"
AFHTTP_CONTAINER_RUNTIME="$CONTAINER_RUNTIME" "$driver" health > "$backup_dir/health-after.json"
ready="$(afdata_config_get "$backup_dir/health-after.json" result.takeover.ready)"
[ "$ready" = true ] || fail takeover_unavailable "The new host did not confirm live browser/CDP/display readiness"
"$CONTAINER_BIN" image inspect "$canonical_image" > "$backup_dir/image-after.json"
verify_backup "$backup_dir"
afdata_result "Local host deployed; existing profiles retained; backup=$backup_dir; rollback_image=$rollback_image; source_revision=$revision"
