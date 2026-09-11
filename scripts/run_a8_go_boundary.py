"""Run Go classification tests with Gonka's pinned Docker builder, no blockchain.

Usage: python3 scripts/run_a8_go_boundary.py GONKA_LINUX_CHECKOUT NEW_OUTPUT_DIR
The output retains Go JSON even when tests fail. This is NOT Wasm ABI proof.
"""
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile


def main():
    root, output = map(lambda x: Path(x).resolve(), sys.argv[1:])
    if str(root).startswith('/mnt/'):
        raise SystemExit('Use a clean Linux checkout; do not build from /mnt/c')
    sha = subprocess.check_output(['git', '-C', str(root), 'rev-parse', 'HEAD'], text=True).strip()
    if subprocess.check_output(['git', '-C', str(root), 'status', '--porcelain']).strip():
        raise SystemExit('Gonka checkout must be clean')
    output.mkdir(parents=True, exist_ok=False)
    original = (root / 'inference-chain/Dockerfile').read_text()
    prefix = original.split('ARG LDFLAGS', 1)[0]
    if prefix == original or 'golang:1.24.2-alpine3.21' not in prefix:
        raise SystemExit('Unexpected pinned builder layout')
    dockerfile = prefix + '''
RUN --mount=type=cache,id=go-build-cache,target=/root/.cache/go-build \
    --mount=type=cache,id=go-mod-cache,target=/go/pkg/mod \
    mkdir -p /boundary-evidence; \
    go test -mod=readonly -tags=muslc -count=1 -json ./app/a8faults \
      -run 'TestToQuerierResultClassifiesVMSystemErrors|TestStrictPlanValidation' \
      > /boundary-evidence/go-test.json 2>&1; \
    echo $? > /boundary-evidence/exit-code
FROM scratch AS boundary-evidence
COPY --from=builder /boundary-evidence/ /
'''
    with tempfile.TemporaryDirectory(prefix='a8-go-boundary-') as tmp:
        spec = Path(tmp) / 'Dockerfile'
        spec.write_text(dockerfile)
        with (output / 'build.log').open('w') as log:
            result = subprocess.run(['docker', 'build', '--file', str(spec), '--target',
                'boundary-evidence', '--output', f'type=local,dest={output / "raw"}', str(root)],
                stdout=log, stderr=subprocess.STDOUT, timeout=1800)
    raw = output / 'raw'
    code = (raw / 'exit-code').read_text().strip() if (raw / 'exit-code').exists() else None
    report = {'gonka_sha': sha, 'level': 'Go classification and JSON roundtrip, not FFI',
        'docker_exit': result.returncode, 'go_exit': code,
        'dockerfile_sha256': hashlib.sha256(dockerfile.encode()).hexdigest(),
        'status': 'FAIL'}
    if result.returncode == 0 and code == '0':
        events = [json.loads(line) for line in (raw / 'go-test.json').read_text().splitlines()
                  if line.startswith('{')]
        required = {'TestToQuerierResultClassifiesVMSystemErrors', 'TestStrictPlanValidation'}
        passed = {e.get('Test') for e in events if e.get('Action') == 'pass'}
        if required <= passed:
            report['status'] = 'PASS'
    if (raw / 'go-test.json').exists():
        report['test_output_sha256'] = hashlib.sha256((raw / 'go-test.json').read_bytes()).hexdigest()
    (output / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
    raise SystemExit(0 if report['status'] == 'PASS' else 1)


if __name__ == '__main__':
    main()
