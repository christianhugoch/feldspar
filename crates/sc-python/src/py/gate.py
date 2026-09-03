"""The import gate: what a code body may import, and what it may not.

Specification §1 ("What a body may import") and §10 ("There is no sandbox, and
the screen says so"), which have to be read together:

    This is **hygiene, not privilege**. ``builtins.open`` exists, and so does
    ``().__class__.__mro__``. A determined body escapes the gate. What the gate
    buys is that ``import subprocess`` is a mistake shaped like an
    ``ImportError`` on the line that made it — and the real bound is the same one
    ``db.sql`` and installing a module already have: authoring a trigger body is
    an administrator's capability.

Everything below follows from that sentence. Nothing here is a security
boundary, nothing here is described as one, and the two holes anybody would find
first are named in this docstring rather than left to be discovered:
``importlib.import_module("subprocess")`` reaches around the gate, and so does
``sys.modules`` — the gate is what a body's own ``import`` statement goes
through, and it is not a supervisor of the interpreter.

# What is deliberately *not* on the list

This runtime's own modules — ``__sc``, the built-in the six bridge functions
live in, and ``__sc_boot``, the pipeline that compiles a body. Refusing them
would buy nothing and would contradict what `boot.py` already says out loud:
the bridge functions are bound into a run's globals anyway where the run holds
the surface, each one checks that for itself, and a body that reaches into the
pipeline is the same body that can reach ``().__class__.__mro__``. A rule that
only *looks* like a boundary is worse than no rule.

# Where the gate is, and why it is not on the meta path

The obvious implementation is a ``sys.meta_path`` finder that refuses a denied
name, and it is the wrong one, for a reason that only shows up once something is
installed: **a finder cannot tell whose import it is answering.** A body that
imports ``requests`` causes ``urllib3`` to import ``socket``, and a gate that
refused that would refuse the packages §1 exists to allow. Worse, a finder is
only consulted for a module that is *not already* in ``sys.modules``, so
``import socket`` would be refused or allowed depending on what some other body
imported first — an answer that changes under you is worse than the hole it
closes.

So the gate is the body's own ``__import__``: [`new_builtins`] hands each run a
copy of ``builtins`` with this module's [`gated_import`] in it, and a body's
globals carry that copy as ``__builtins__``. An import statement *in the body*
is looked up there and is checked; an import inside a library the body called is
looked up in that library's own module globals and is not. That is exactly the
line §1 draws — the author's mistakes, and nothing else.

# The list is a deny-list, and it is the same set §1 describes

§1 states the rule as an allow-list: the standard library minus the modules that
reach the process, the network and the disk, plus every package installed in
this server's environment. This module states its complement, because the two
are the same set and only one of them can be maintained. `interp::isolate_path`
has already taken the host's own packages off ``sys.path`` and put this server's
environment on it, so a name that is not standard library and not installed does
not resolve at all — a deny-list over that path is the allow-list §1 asks for,
and a name nobody installed fails as CPython's own ``ModuleNotFoundError``
rather than as a refusal that would read as though the gate had an opinion
about it.
"""

import builtins
import os as _os_module
import types

#: The real ``__import__``, captured before anything replaces it.
_REAL_IMPORT = builtins.__import__

# --- why a module is refused ------------------------------------------------
#
# One sentence each, and every refusal carries one: "you may not import this" is
# an obstacle, and "you may not import this, because X, and here is what to use
# instead" is an answer.

_PROCESS = (
    "the import gate refuses the standard-library modules that reach this "
    "process — starting one, signalling one, or loading foreign code into it. A "
    "code body runs inside the server, and the work it is meant to do outside "
    "it goes through the five handles"
)
_NETWORK = (
    "the import gate refuses the standard-library modules that open their own "
    "network connections. `fetch(url, ...)` is how a body makes an HTTP "
    "request, and it is the one this run's budget and deadline can see"
)
_DISK = (
    "the import gate refuses the standard-library modules that operate on this "
    "server's own disk. `fs(store)` is how a body reads and writes files, and "
    "it is the one a store's permissions apply to"
)
_IMPORTS = (
    "the import gate refuses the import machinery itself, which a body has no "
    "use for"
)
_THREADS = (
    "a run is one thread and its deadline is enforced on that thread, so a "
    "thread a body starts outlives the run and nothing stops it. Concurrency "
    "between bodies is the runtime's: every host call releases the interpreter, "
    "and other runs execute while this one waits"
)
_ENVIRONMENT = (
    "the process environment is where this server keeps its database URL and "
    "its secrets. A value a body needs belongs in Settings, and reaches the "
    "body as a binding"
)

#: Module → the rule that refuses it. A dotted entry refuses that module and
#: everything under it and nothing else, which is how ``urllib.request`` is
#: refused while ``urllib.parse`` — a pure function over a string — is not.
_DENIED = {
    # Processes, signals, foreign code, and the interpreter's own re-entry
    # points.
    "subprocess": _PROCESS,
    "multiprocessing": _PROCESS,
    "_multiprocessing": _PROCESS,
    "_posixsubprocess": _PROCESS,
    "posix": _PROCESS,
    "nt": _PROCESS,
    "signal": _PROCESS,
    "pty": _PROCESS,
    "tty": _PROCESS,
    "fcntl": _PROCESS,
    "termios": _PROCESS,
    "resource": _PROCESS,
    "pwd": _PROCESS,
    "grp": _PROCESS,
    "msvcrt": _PROCESS,
    "winreg": _PROCESS,
    "ctypes": _PROCESS,
    "_ctypes": _PROCESS,
    "runpy": _PROCESS,
    "venv": _PROCESS,
    "ensurepip": _PROCESS,
    "webbrowser": _PROCESS,
    # The network. `fetch` is the one way out.
    "socket": _NETWORK,
    "_socket": _NETWORK,
    "socketserver": _NETWORK,
    "ssl": _NETWORK,
    "_ssl": _NETWORK,
    "select": _NETWORK,
    "selectors": _NETWORK,
    "http": _NETWORK,
    "urllib.request": _NETWORK,
    "urllib.error": _NETWORK,
    "urllib.response": _NETWORK,
    "ftplib": _NETWORK,
    "smtplib": _NETWORK,
    "poplib": _NETWORK,
    "imaplib": _NETWORK,
    "nntplib": _NETWORK,
    "xmlrpc": _NETWORK,
    "wsgiref": _NETWORK,
    # The disk. `builtins.open` is still there — §10 — so this is the foot-gun
    # end of it rather than a boundary.
    "shutil": _DISK,
    # The import machinery.
    "importlib": _IMPORTS,
    "pkgutil": _IMPORTS,
    "zipimport": _IMPORTS,
    "modulefinder": _IMPORTS,
    # Threads.
    "threading": _THREADS,
    "_thread": _THREADS,
    "concurrent": _THREADS,
}

#: ``os`` attributes a body may not have. `os` itself is allowed, because
#: ``os.path`` is a library of string functions everybody uses; what is refused
#: is the part of it that is the process (§1: "`os` is allowed for `os.path` and
#: `os.environ` is not readable").
_OS_DENIED = {
    "environ": _ENVIRONMENT,
    "environb": _ENVIRONMENT,
    "getenv": _ENVIRONMENT,
    "getenvb": _ENVIRONMENT,
    "putenv": _ENVIRONMENT,
    "unsetenv": _ENVIRONMENT,
    "system": _PROCESS,
    "popen": _PROCESS,
    "fork": _PROCESS,
    "forkpty": _PROCESS,
    "kill": _PROCESS,
    "killpg": _PROCESS,
    "abort": _PROCESS,
    "_exit": _PROCESS,
    "chroot": _PROCESS,
    "setuid": _PROCESS,
    "seteuid": _PROCESS,
    "setgid": _PROCESS,
    "setegid": _PROCESS,
}

#: The families of `os` the list above would otherwise have to enumerate, and
#: would fall behind: `execv`, `execve`, `execvp`, `spawnl`, `posix_spawn`, …
_OS_DENIED_PREFIXES = ("exec", "spawn", "posix_spawn")


def _os_refusal(name):
    """The rule that refuses ``os.<name>``, or ``None``."""
    reason = _OS_DENIED.get(name)
    if reason is not None:
        return reason
    if name.startswith(_OS_DENIED_PREFIXES):
        return _PROCESS
    return None


class _GatedOs(types.ModuleType):
    """``os`` as a body sees it: ``os.path``, and not the process.

    A ``ModuleType`` subclass rather than a stand-in object, so a body that
    checks gets a module; the instance dictionary is left empty on purpose, so
    every attribute misses it and arrives at ``__getattr__`` and there is one
    place where the rule is applied.
    """

    def __getattr__(self, name):
        reason = _os_refusal(name)
        if reason is not None:
            # Not an `ImportError`: this is `os.environ` on line 4, and the
            # reader is looking at an attribute rather than at an import. Not an
            # `AttributeError` either — that is the one exception
            # `from os import environ` would swallow and re-raise as "cannot
            # import name", losing the sentence that says why.
            raise PermissionError(f"a Saltcorn code body may not read `os.{name}`: {reason}.")
        return getattr(_os_module, name)

    def __dir__(self):
        return [name for name in dir(_os_module) if _os_refusal(name) is None]


def _gated_os():
    """The single shared ``os`` stand-in, built once."""
    module = _GatedOs("os")
    module.__doc__ = _os_module.__doc__
    # What `ModuleType.__init__` put there would otherwise shadow the real
    # module's own: a body reading `os.__spec__` should see `os`'s.
    for shadowed in ("__package__", "__loader__", "__spec__"):
        module.__dict__.pop(shadowed, None)
    return module


_OS = _gated_os()


def _refuse(name, reason):
    return ImportError(f"a Saltcorn code body may not import `{name}`: {reason}.")


def _check(name):
    """Raise if ``name`` — a dotted module name — is refused."""
    for denied, reason in _DENIED.items():
        if name == denied or name.startswith(denied + "."):
            raise _refuse(name, reason)


def gated_import(name, globals=None, locals=None, fromlist=(), level=0):  # noqa: A002
    """``__import__`` for a code body: the same import, checked first.

    ``fromlist`` is checked as well as ``name``, because ``from urllib import
    request`` asks for ``urllib`` and gets the submodule handed to it — and the
    check has to happen *before* the real import runs, so that a refused module
    is not imported and then withheld.
    """
    # `level > 0` is a relative import, which can only appear inside a package.
    # A body is not one, so CPython refuses it before the name means anything.
    if level == 0:
        _check(name)
        for item in fromlist or ():
            if isinstance(item, str) and item != "*":
                _check(f"{name}.{item}")
    module = _REAL_IMPORT(name, globals, locals, fromlist, level)
    # `import os`, `import os.path` and `from os import path` all end here with
    # the real `os` module in hand; the body gets the stand-in instead, and
    # `from os.path import join` — which returns `os.path` itself — is untouched.
    if module is _os_module:
        return _OS
    return module


def new_builtins():
    """The ``__builtins__`` one run's globals carry.

    A **copy** of the real one with ``__import__`` replaced, so that the gate
    reaches the body's own import statements and nothing else, and so that a
    body which assigns into its builtins has not done it to the next body. Every
    other name is the real object — ``open`` included, which §10 says out loud.
    """
    namespace = dict(builtins.__dict__)
    namespace["__import__"] = gated_import
    return namespace
