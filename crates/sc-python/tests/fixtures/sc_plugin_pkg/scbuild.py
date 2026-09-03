"""A self-contained PEP 517 backend: it builds the wheel with `zipfile` alone.

`sc_fixture_pkg/scbuild.py`'s sibling, and written out for the same reason —
installing this fixture must need **no network**, so pip builds it in an
isolated environment whose only requirement is nothing.

The one thing it does that the other does not is write `entry_points.txt`, which
is how a distribution advertises its Saltcorn plugin (§9).
"""

import base64
import hashlib
import os
import zipfile

NAME = "sc_plugin_fixture"
VERSION = "0.2.0"
DIST = f"{NAME}-{VERSION}"
HERE = os.path.dirname(os.path.abspath(__file__))

METADATA = f"""Metadata-Version: 2.1
Name: sc-plugin-fixture
Version: {VERSION}
Summary: A Saltcorn Python plugin for the module host's tests.
"""

WHEEL = """Wheel-Version: 1.0
Generator: scbuild
Root-Is-Purelib: true
Tag: py3-none-any
"""

# The declarations are in a **submodule**, reached through the entry point
# rather than by importing the top-level package: that is the path §9 names
# first, and the one that would break if the host imported the distribution's
# name and hoped.
ENTRY_POINTS = f"""[saltcorn.plugins]
plugin = {NAME}.plugin
"""


def _record_line(name, data):
    digest = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode()
    return f"{name},sha256={digest},{len(data)}\n"


def build_wheel(wheel_directory, config_settings=None, metadata_directory=None):
    filename = f"{DIST}-py3-none-any.whl"
    entries = {}
    for root, _dirs, files in os.walk(os.path.join(HERE, NAME)):
        for name in files:
            if name.endswith(".pyc"):
                continue
            full = os.path.join(root, name)
            arc = os.path.relpath(full, HERE).replace(os.sep, "/")
            with open(full, "rb") as handle:
                entries[arc] = handle.read()
    info = f"{DIST}.dist-info"
    entries[f"{info}/METADATA"] = METADATA.encode()
    entries[f"{info}/WHEEL"] = WHEEL.encode()
    entries[f"{info}/entry_points.txt"] = ENTRY_POINTS.encode()
    record = "".join(_record_line(n, d) for n, d in sorted(entries.items()))
    record += f"{info}/RECORD,,\n"
    with zipfile.ZipFile(os.path.join(wheel_directory, filename), "w", zipfile.ZIP_DEFLATED) as z:
        for name, data in sorted(entries.items()):
            z.writestr(name, data)
        z.writestr(f"{info}/RECORD", record)
    return filename


def build_sdist(sdist_directory, config_settings=None):
    raise NotImplementedError("this fixture builds wheels only")
