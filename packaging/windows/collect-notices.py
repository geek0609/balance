"""Collect the notice files for the release build and its bundled Rust crates."""

from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
from pathlib import Path
import subprocess
import urllib.request
import sys

def command(*args):
    return subprocess.check_output(args, text=True, encoding='utf-8').strip()

cargo = json.loads(command(
    'cargo', 'metadata', '--locked', '--format-version', '1',
    '--features', 'windows-updater', '--filter-platform', 'x86_64-pc-windows-msvc',
))
packages = {package['id']: package for package in cargo['packages']}
nodes = {node['id']: node for node in cargo['resolve']['nodes']}
root = cargo['resolve']['root']
pending = [root]
runtime_graph = set()
while pending:
    current = pending.pop()
    if current in runtime_graph:
        continue
    runtime_graph.add(current)
    pending.extend(
        dependency['pkg']
        for dependency in nodes[current]['deps']
        if any(entry['kind'] is None for entry in dependency['dep_kinds'])
    )

names = ('license', 'licence', 'copying', 'copyright', 'notice', 'unlicense')
upstreams = {
    'clipboard-win': ('DoumanAsh/clipboard-win', ('LICENSE',)),
    'profiling': ('aclysma/profiling', ('LICENSE-APACHE', 'LICENSE-MIT')),
}

def fetch_at_revision(repository, revision, filename):
    url = f'https://raw.githubusercontent.com/{repository}/{revision}/{filename}'
    with urllib.request.urlopen(url, timeout=20) as response:
        return response.read()

def collect(package):
    name, version = package['name'], package['version']
    source = Path(package['manifest_path']).parent
    prefix = f'licenses/{name}-{version}/'
    notices = {
        path.name: path.read_bytes()
        for path in sorted(source.iterdir())
        if path.is_file() and path.name.lower().startswith(names)
    }
    if package['license_file']:
        path = source / package['license_file']
        notices[path.name] = path.read_bytes()
    if name == 'epaint_default_fonts':
        notices.update({
            f'fonts/{path.name}': path.read_bytes()
            for path in sorted((source / 'fonts').glob('*.txt'))
        })
    if not any('/' not in file for file in notices):
        if package['repository'].startswith('https://github.com/emilk/egui'):
            repository, files = ('emilk/egui', ('LICENSE-APACHE', 'LICENSE-MIT'))
        else:
            repository, files = upstreams[name]
        revision = json.loads((source / '.cargo_vcs_info.json').read_bytes())['git']['sha1']
        notices.update({
            file: fetch_at_revision(repository, revision, file)
            for file in files
        })
    if not notices:
        raise SystemExit(f'No license files for {name}')
    return {
        prefix + name: contents
        for name, contents in notices.items()
    }

dependencies = sorted(
    (packages[key] for key in runtime_graph if key != root),
    key=lambda p: (p['name'], p['version']),
)
with ThreadPoolExecutor(max_workers=8) as pool:
    dependency_files = {
        filename: contents
        for notices in pool.map(collect, dependencies)
        for filename, contents in notices.items()
    }

toolchain = command('rustc', '-vV')
revision = next(line.split(': ', 1)[1] for line in toolchain.splitlines()
                if line.startswith('commit-hash: '))
for file in ('COPYRIGHT', 'LICENSE-APACHE', 'LICENSE-MIT'):
    dependency_files[f'licenses/rust/{file}'] = fetch_at_revision('rust-lang/rust', revision, file)

index = '\n'.join(
    f"{p['name']} {p['version']}  {p['license']}  {p['repository'] or 'https://crates.io/crates/' + p['name']}"
    for p in dependencies
)
dependency_files['licenses/INDEX.txt'] = ('Windows build dependency notices\n\n' + index + '\n').encode('utf-8')

destination = Path(sys.argv[1])
for relative_path, contents in dependency_files.items():
    path = destination / relative_path
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(contents)
