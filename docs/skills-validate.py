#!/usr/bin/env python3
"""Read-only structural/provenance checks for the curated project skill pack.

Uses Python's standard library only. For full YAML validation also use the Codex
skill-creator quick_validate.py when available; this does not install that tool.
"""
import hashlib
import json
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[1]
PACK = ROOT / '.agents' / 'skills'


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def validate(root=ROOT):
    pack = root / '.agents' / 'skills'
    errors = []
    manifest = json.loads((root / 'docs' / 'skills-manifest.json').read_text())
    entries = manifest['skills']
    names = set()
    references = 0
    files = 0
    expected = {entry['name'] for entry in entries}
    actual = {path.parent.name for path in pack.glob('*/SKILL.md')}
    if actual != expected:
        errors.append('Manifest and installed skill directory inventory differ')
    for key, source in manifest['sources'].items():
        if not re.fullmatch(r'[0-9a-f]{40}', source['commit']):
            errors.append(f'{key}: missing full commit pin')
        license_path = root / source['license_file']
        if not license_path.is_file() or sha256(license_path) != source['license_sha256']:
            errors.append(f'{key}: license missing or hash differs')
    for entry in entries:
        path = root / entry['installed_path']
        text = path.read_text()
        fm = re.match(r'^---\n(.*?)\n---\n', text, re.S)
        if not fm:
            errors.append(f'{entry["name"]}: missing frontmatter')
            continue
        name = re.search(r'^name: ([a-z0-9-]+)$', fm.group(1), re.M)
        description = re.search(r'^description: .+', fm.group(1), re.M)
        if not name or name.group(1) != entry['name'] or len(name.group(1)) > 64:
            errors.append(f'{entry["name"]}: invalid/mismatched name')
        if not description:
            errors.append(f'{entry["name"]}: missing description')
        if name and name.group(1) in names:
            errors.append(f'{entry["name"]}: duplicate name')
        if name:
            names.add(name.group(1))
        installed = {str(p.relative_to(root)) for p in path.parent.rglob('*') if p.is_file()}
        if installed != set(entry['installed_files']):
            errors.append(f'{entry["name"]}: file manifest incomplete')
        for rel, checksum in entry['installed_files'].items():
            full = root / rel
            if not full.is_file() or full.is_symlink() or sha256(full) != checksum:
                errors.append(f'{rel}: missing, symlinked or changed file')
            files += 1
    for path in pack.rglob('*'):
        if path.is_symlink():
            errors.append(f'{path.relative_to(root)}: symlinks not permitted')
        if path.is_file() and path.suffix in {'.sh', '.py', '.js', '.mjs', '.ps1', '.exe'}:
            errors.append(f'{path.relative_to(root)}: executable support is outside declarative pack')
        if not path.is_file() or path.suffix != '.md':
            continue
        text = path.read_text()
        # Only resource paths, not illustrative application code snippets.
        targets = set(re.findall(r'\[[^\]]*\]\(([^)\s]+)\)', text))
        targets.update(re.findall(r'`((?:references|reference|resources)/[^`]+\.(?:md|json|yaml|yml|txt))`', text))
        for target in targets:
            if re.match(r'^[a-zA-Z][a-zA-Z0-9+.-]*:', target) or target.startswith('#'):
                continue
            target = target.split('#', 1)[0]
            resolved = (path.parent / target).resolve()
            references += 1
            if not resolved.is_relative_to(root.resolve()) or not resolved.is_file():
                errors.append(f'{path.relative_to(root)}: missing/out-of-root local reference {target}')
    result = {'skills': len(entries), 'sources': len(manifest['sources']),
              'manifested_skill_files': files, 'local_references_checked': references,
              'errors': errors, 'result': 'PASS' if not errors else 'FAIL'}
    return result


if __name__ == '__main__':
    result = validate()
    print(json.dumps(result, indent=2))
    sys.exit(bool(result['errors']))
