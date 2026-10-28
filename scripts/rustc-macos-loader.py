#!/usr/bin/env python3
"""Work around the verified Rust 1.99.1 macOS objcopy rpath packaging defect."""
import os
from pathlib import Path
import subprocess
import sys


def compiler_environment(arguments, environment, platform):
    child = dict(environment)
    if platform != "darwin":
        return child
    compiler = arguments[0]
    # Queries do not participate in Cargo's jobserver: check_output closes their
    # descriptors. Preserve jobserver variables and descriptors for the exec below.
    query_environment = dict(environment)
    for name in ("CARGO_MAKEFLAGS", "MAKEFLAGS", "MFLAGS"):
        query_environment.pop(name, None)
    version = subprocess.check_output([compiler, "--version"], text=True, env=query_environment).strip()
    if not version.startswith("rustc 1.99.1 "):
        return child
    sysroot = Path(subprocess.check_output([compiler, "--print", "sysroot"], text=True, env=query_environment).strip())
    library = sysroot / "lib"
    if not (library / "libLLVM.dylib").is_file():
        raise RuntimeError("Rust 1.99.1 compiler sysroot lacks its bundled lib/libLLVM.dylib")
    previous = child.get("DYLD_LIBRARY_PATH", "")
    child["DYLD_LIBRARY_PATH"] = str(library) + (":" + previous if previous else "")
    return child


def main():
    arguments = sys.argv[1:]
    if not arguments:
        raise SystemExit("compiler wrapper requires the actual rustc executable")
    child = compiler_environment(arguments, os.environ, sys.platform)
    os.execvpe(arguments[0], arguments, child)


if __name__ == "__main__":
    main()
