#!/usr/bin/env python3
"""Usage: setup-wasi-sysroot.py RUSTC EXTRACTED_RUST_STD_COMPONENT NEW_SYSROOT

Create a project-local sysroot overlay for an immutable Rust installation.
The component must match RUSTC exactly. Nothing in the original sysroot changes.
Pass the printed --sysroot flag through RUSTFLAGS for the WASI target build.
"""
import pathlib
import subprocess
import sys

if len(sys.argv) != 4 or sys.argv[1] in ('help', '--help'):
    print(__doc__)
    raise SystemExit(0 if len(sys.argv) > 1 and sys.argv[1] in ('help', '--help') else 2)

compiler, component_arg, output_arg = sys.argv[1:]
original = pathlib.Path(subprocess.check_output([compiler, '--print', 'sysroot'], text=True).strip()).resolve()
component = pathlib.Path(component_arg).resolve()
target = component / 'lib/rustlib/wasm32-wasip1'
if not (target / 'lib').is_dir():
    raise SystemExit('Component must contain lib/rustlib/wasm32-wasip1/lib')
output = pathlib.Path(output_arg).resolve()
output.mkdir(parents=True, exist_ok=False)
for entry in original.iterdir():
    if entry.name != 'lib':
        (output / entry.name).symlink_to(entry, target_is_directory=entry.is_dir())
(output / 'lib/rustlib').mkdir(parents=True)
for entry in (original / 'lib').iterdir():
    if entry.name != 'rustlib':
        (output / 'lib' / entry.name).symlink_to(entry, target_is_directory=entry.is_dir())
for entry in (original / 'lib/rustlib').iterdir():
    if entry.name != 'wasm32-wasip1':
        (output / 'lib/rustlib' / entry.name).symlink_to(entry, target_is_directory=entry.is_dir())
(output / 'lib/rustlib/wasm32-wasip1').symlink_to(target, target_is_directory=True)
print(f'--sysroot={output}')
