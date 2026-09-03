"""A self-contained PEP 517 backend: it builds the wheel with `zipfile` alone.

Written out rather than depending on setuptools so that installing this fixture
needs **no network**: pip builds it in an isolated environment whose only
requirement is nothing, and `backend-path` puts this file on the path there.
"""

import base64
import hashlib
import os
import zipfile

NAME = "sc_fixture"
VERSION = "0.3.1"
DIST = f"{NAME}-{VERSION}"
HERE = os.path.dirname(os.path.abspath(__file__))

METADATA = f"""Metadata-Version: 2.1
Name: sc-fixture
Version: {VERSION}
Summary: A fixture distribution for Saltcorn's Python environment tests.
"""

WHEEL = """Wheel-Version: 1.0
Generator: scbuild
Root-Is-Purelib: true
Tag: py3-none-any
"""


def _record_line(name, data):
    digest = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode()
    return f"{name},sha256={digest},{len(data)}\n"


def build_wheel(wheel_directory, config_settings=None, metadata_directory=None):
    filename = f"{DIST}-py3-none-any.whl"
    entries = {}
    for root, _dirs, files in os.walk(os.path.join(HERE, NAME)):
        for name in files:
            full = os.path.join(root, name)
            arc = os.path.relpath(full, HERE).replace(os.sep, "/")
            with open(full, "rb") as handle:
                entries[arc] = handle.read()
    info = f"{DIST}.dist-info"
    entries[f"{info}/METADATA"] = METADATA.encode()
    entries[f"{info}/WHEEL"] = WHEEL.encode()
    record = "".join(_record_line(n, d) for n, d in sorted(entries.items()))
    record += f"{info}/RECORD,,\n"
    with zipfile.ZipFile(os.path.join(wheel_directory, filename), "w", zipfile.ZIP_DEFLATED) as z:
        for name, data in sorted(entries.items()):
            z.writestr(name, data)
        z.writestr(f"{info}/RECORD", record)
    return filename


def build_sdist(sdist_directory, config_settings=None):
    raise NotImplementedError("this fixture builds wheels only")
