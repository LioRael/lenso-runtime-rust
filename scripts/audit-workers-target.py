#!/usr/bin/env python3
"""Capture production-only Wasm checks; failures are audit results, not certification.

Pass absolute checkout paths and --cargo pointing to the workspace wrapper locally.
Each case records its own status; inspect report.json rather than the script exit
status to distinguish a compatible build from a successfully collected failure.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess

CASES = [
    ('runtime', 'runtime', ['-p', 'lenso-kernel', '-p', 'lenso-native-adapter', '-p', 'lenso-browser-driver']),
    ('http-contract', 'web', ['-p', 'lenso-capability-http-endpoint']),
    ('native-ingress', 'web', ['-p', 'lenso-web-ingress-plugin']),
    ('auth-router', 'auth', ['-p', 'lenso-auth-router-plugin']),
    ('auth-account', 'auth', ['-p', 'lenso-auth-account-plugin']),
    ('auth-password', 'auth', ['-p', 'lenso-auth-password-plugin']),
    ('auth-web-session', 'auth', ['-p', 'lenso-auth-web-session-plugin']),
    ('auth-oidc-client', 'auth', ['-p', 'lenso-auth-oidc-client-plugin']),
    ('signature-consumer', 'cli', ['--no-default-features']),
]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for owner in ('runtime', 'web', 'auth', 'cli'):
        parser.add_argument('--' + owner, type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--cargo', default='cargo')
    parser.add_argument('--toolchain', default='1.94.0')
    parser.add_argument('--case', choices=[c[0] for c in CASES], action='append')
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    report = {'target': 'wasm32-unknown-unknown', 'toolchain': args.toolchain,
              'scope': 'cargo check --lib; no link, runtime or Workers certification',
              'cases': []}
    for name, owner, selection in CASES:
        if args.case and name not in args.case:
            continue
        root = getattr(args, owner).resolve()
        prefix = [args.cargo, '+' + args.toolchain]
        common = ['--locked', '--target', report['target'], *selection]
        head = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root, text=True).strip()
        lock_digest = hashlib.sha256((root / 'Cargo.lock').read_bytes()).hexdigest()
        entry = {'name': name, 'owner': owner, 'head': head, 'lock_sha256': lock_digest,
                 'worktree_status': subprocess.check_output(['git', 'status', '--porcelain'], cwd=root, text=True),
                 'steps': []}
        for kind, command in [('check', prefix + ['check', *common, '--lib']),
                              ('features', prefix + ['tree', *common, '-e', 'normal,build,features'])]:
            path = args.output / (name + '.' + kind + '.log')
            with path.open('w') as log:
                result = subprocess.run(command, cwd=root, stdout=log, stderr=subprocess.STDOUT, check=False)
            entry['steps'].append({'kind': kind, 'command': command, 'exit_code': result.returncode,
                                   'log': path.name, 'sha256': hashlib.sha256(path.read_bytes()).hexdigest()})
        report['cases'].append(entry)
        (args.output / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
        print(name, entry['steps'][0]['exit_code'], flush=True)


if __name__ == '__main__':
    main()
