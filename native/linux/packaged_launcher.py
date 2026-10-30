#!/usr/bin/python3 -I
"""Explicit desktop composition for a trusted installed/portable Linux layout.

Runtime is a sibling of this file. Registration is a separate user-invoked
operation; opening a document never installs files or changes associations.
"""
import os
from pathlib import Path
import re
import stat
import sys


MIME = "application/x-migration-stream"
DESKTOP_NAME = "migration-stream.desktop"
MIME_NAME = "migration-stream.xml"
MIME_XML = ("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n"
            "<mime-info xmlns=\"http://www.freedesktop.org/standards/shared-mime-info\">\n"
            f"  <mime-type type=\"{MIME}\"><comment>Migration Stream object</comment>"
            "<glob pattern=\"*.mstream\"/></mime-type>\n</mime-info>\n")
DESKTOP_ENVIRONMENT = (
    "HOME", "DISPLAY", "WAYLAND_DISPLAY", "XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS",
    "XDG_CURRENT_DESKTOP", "XDG_SESSION_TYPE", "XDG_CONFIG_HOME", "XDG_DATA_HOME",
)


def desktop_entry(launcher):
    # Match the existing template's conservative installation-path contract.
    # Documents still use native byte argv, with no such filename restriction.
    path = str(launcher)
    if not re.fullmatch(r"/[A-Za-z0-9_./-]+", path) or ".." in Path(path).parts:
        raise ValueError("desktop binding requires an ordinary absolute ASCII path")
    return ("[Desktop Entry]\nType=Application\nName=Migration Stream\n"
            "Comment=Open a bound Migration Stream object\n"
            f"Exec=/usr/bin/python3 -I {path} %f\n"
            f"MimeType={MIME};\nTerminal=false\nNoDisplay=true\nDBusActivatable=false\n")


def presenter_environment(environment):
    # An explicit OS desktop presenter, not a browser executable search. Never
    # inherit LD_*, PYTHON*, BROWSER, shell startup options or controller env.
    result = {"PATH": "/usr/bin:/bin"}
    for name in DESKTOP_ENVIRONMENT:
        value = environment.get(name)
        if value is not None:
            if not isinstance(value, str) or len(os.fsencode(value)) > 4096 or "\0" in value:
                raise ValueError("invalid desktop environment")
            result[name] = value
    return result


def _real_directory(path, *, owned=False):
    if not path.is_absolute() or ".." in path.parts:
        raise ValueError("ordinary absolute directory required")
    for part in [*reversed(path.parents), path]:
        info = part.lstat()
        if not stat.S_ISDIR(info.st_mode) or stat.S_ISLNK(info.st_mode):
            raise ValueError("real directory required")
    if owned and path.stat().st_uid != os.geteuid():
        raise ValueError("user-owned registration directory required")


def register_desktop(root, data_home, run):
    """Create only our two user files; update caches, never the default app.

    A failed update retains created files for inspection. No rollback deletes
    shared/user directories or previously existing association files.
    """
    if os.geteuid() == 0:
        raise ValueError("portable registration is not a root installer")
    _real_directory(root)
    _real_directory(data_home, owned=True)
    entry = desktop_entry(root / "packaged_launcher.py").encode("ascii")
    applications = data_home / "applications"
    mime = data_home / "mime"
    packages = mime / "packages"
    for directory in (applications, mime, packages):
        try:
            directory.mkdir(mode=0o700)
        except FileExistsError:
            pass
        _real_directory(directory, owned=True)
    targets = ((applications / DESKTOP_NAME, entry),
               (packages / MIME_NAME, MIME_XML.encode("utf-8")))
    # Refuse either collision before writing either file. O_EXCL is the actual
    # no-overwrite boundary if another process races this preflight.
    if any(os.path.lexists(path) for path, _ in targets):
        raise ValueError("association file already exists")
    for path, data in targets:
        descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        with os.fdopen(descriptor, "wb") as output:
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
    for argv in ((b"/usr/bin/update-mime-database", os.fsencode(mime)),
                 (b"/usr/bin/update-desktop-database", os.fsencode(applications))):
        if run(argv, timeout=10, environment={"PATH": "/usr/bin:/bin"}).status != 0:
            raise ValueError("desktop cache update failed")


def main(arguments=None):
    arguments = sys.argv[1:] if arguments is None else arguments
    try:
        if not sys.flags.isolated:
            raise ValueError("explicit isolated interpreter required")
        root = Path(__file__).absolute().parent
        _real_directory(root)
        sys.path.insert(0, str(root))
        from document_open_launcher import Failure, launch, run_bounded

        if len(arguments) == 2 and arguments[0] == "--register-desktop":
            register_desktop(root, Path(arguments[1]), run_bounded)
            return 0

        def present(destination):
            output = run_bounded(
                [b"/usr/bin/xdg-open", destination], timeout=10,
                environment=presenter_environment(os.environ),
            )
            if output.status != 0:
                raise Failure("desktop presentation failed")

        return launch(arguments, trusted_runtime_root=root / "Runtime", browser=present)
    except Exception:
        # Includes the adapter's bounded-tool failures during explicit cache
        # registration. Never expose their diagnostics or installation paths.
        print("Migration Stream Linux desktop operation failed.", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
