#!/usr/bin/env python3
"""Compare strict and scoped waits on an existing host without changing its profile.

Example: DOCKER_CONTEXT=colima python3 tests/live-readiness.py --profile vps \
  --url https://example.com/ --selector '#prices' --out /tmp/readiness --build
"""
import argparse
import json
from pathlib import Path
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--url', required=True)
    parser.add_argument('--profile', required=True)
    parser.add_argument('--selector', required=True)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--timeout-ms', type=int, default=45000)
    parser.add_argument('--build', action='store_true')
    parser.add_argument('--wait-mode', choices=['both', 'strict', 'target', 'load'], default='both')
    parser.add_argument('--driver', type=Path)
    parser.add_argument('--tab', help='Existing tab, only with a single wait mode')
    args = parser.parse_args()
    if args.tab and args.wait_mode == 'both':
        parser.error('--tab requires one wait mode, because completed capture can recycle it')
    root = Path(__file__).resolve().parent.parent
    driver = (args.driver or root / 'target/debug/afhttp').resolve()
    processes = subprocess.run(['ps', '-axo', 'pid,command'], capture_output=True,
                               text=True, check=True).stdout.splitlines()
    busy = [line.split(maxsplit=1)[0] for line in processes if any(
        token in line for token in ('afhttp fetch ', 'afhttp ui takeover',
                                    'inspect-page.py ', 'AFUI Review.app/'))]
    if busy:
        raise SystemExit('host may be in use; inspect process IDs: ' + ', '.join(busy))
    if args.build:
        subprocess.run([str(root / 'tests/build.sh'), '--native', '--bin', 'afhttp'],
                       cwd=root, check=True)
    status = subprocess.run([str(driver), 'container', 'status'], check=True,
                            capture_output=True, text=True)
    host = json.loads(status.stdout)['result']
    if not host.get('running') or host.get('profile') != args.profile:
        raise SystemExit('expected host/profile is not active; refusing to switch it')
    args.out.mkdir(parents=True, exist_ok=True)
    summaries = []
    modes = [('strict', 'auto'), ('target', 'selector-stable:' + args.selector), ('load', 'load')]
    for label, wait in modes:
        if args.wait_mode != label and not (args.wait_mode == 'both' and label != 'load'):
            continue
        result_file = args.out.resolve() / (label + '-result.json')
        command = [
            str(driver), 'fetch', args.url, '--profile', args.profile,
            '--takeover', '--render', 'always', '--wait', wait,
            '--timeout-ms', str(args.timeout_ms), '--want', 'body',
            '--want', 'content', '--want', 'content_json', '--want', 'observation',
            '--want', 'network', '--out', str(args.out.resolve() / label),
            '--stdout-file', str(result_file),
        ]
        if args.tab:
            command.extend(['--tab', args.tab])
        subprocess.run(command, check=True)
        result = json.loads(result_file.read_text())['result']
        summary = {key: result.get(key) for key in ('status', 'page_kind', 'tab_id', 'readiness', 'warnings')}
        summary.update(wait=wait, result_file=str(result_file), driver=str(driver))
        summary['trace'] = {key: result['trace'].get(key) for key in (
            'duration_ms', 'network_quiet', 'dom_stable', 'text_stable', 'capture_reason')}
        if result.get('tab_id'):
            subprocess.run([str(driver), 'cdp', 'Page.bringToFront', '--tab', result['tab_id']], check=True)
            summary['retained_tab_verified'] = True
        summaries.append(summary)
        if result.get('next_action') or result.get('page_kind'):
            break
    (args.out / 'summary.json').write_text(json.dumps(summaries, indent=2))
    print(json.dumps({'summary_file': str(args.out.resolve() / 'summary.json'),
                      'driver': str(driver)}))


if __name__ == '__main__':
    main()
