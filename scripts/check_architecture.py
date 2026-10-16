#!/usr/bin/env python3
"""Enforce dependency direction without loading native SDKs."""
from pathlib import Path
import re
import tomllib
root = Path(__file__).resolve().parent.parent
core = root / 'crates/core'
manifest = tomllib.loads((core / 'Cargo.toml').read_text(encoding='utf-8'))
forbidden_dependencies = {'windows', 'windows-sys', 'objc2', 'gpui-kit', 'russh', 'hyper', 'axum', 'screencapturekit', 'security-framework'}
assert not forbidden_dependencies.intersection(manifest['dependencies']), 'core depends on a concrete adapter'
assert not manifest.get('target'), 'core must not select native platform dependencies'
for path in (core / 'src').rglob('*.rs'):
    text = path.read_text(encoding='utf-8')
    assert not re.search(r'cfg\([^\n]*(?:windows|target_os)', text), f'native platform branch in {path}'
    assert not re.search(r'landesk_client::|landesk::|crate::platform|crate::adapters', text), f'reverse dependency in {path}'
    if 'ports' in path.relative_to(core).parts:
        assert 'application::' not in text, f'port contract depends on application implementation in {path}'
    if 'domain' in path.relative_to(core).parts:
        assert not re.search(r'tokio::|hyper::|russh::|std::fs|ports::|application::', text), f'domain depends on IO/application in {path}'
for app in ['client', 'server']:
    package = root / 'apps' / app
    assert not (package / 'Cargo.lock').exists(), 'workspace must have one lockfile'
    for path in (package / 'src').rglob('*.rs'):
        assert '#[path' not in path.read_text(encoding='utf-8'), f'cross-source inclusion in {path}'
storage = root / 'crates/storage'
assert storage.exists(), 'shared storage adapter missing'
assert 'landesk-storage' not in manifest['dependencies'], 'core depends on storage adapter'
for area in ['apps/client/src', 'apps/server/src', 'crates/storage/src']:
    for path in (root / area).rglob('*.rs'):
        text = path.read_text(encoding='utf-8')
        assert not re.search(r'\b(?:CredReadW|CredWriteW|CredDeleteW|CryptProtectData|CryptUnprotectData)\s*\(', text), f'OS credential store in {path}'
        assert 'security_framework::' not in text, f'keychain dependency in {path}'
print('architecture boundaries: OK')
