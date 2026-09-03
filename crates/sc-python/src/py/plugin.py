"""What a Python **plugin module** declares, and how this server reads it.

Specification §2 of the Python API ("A plugin module"), §8 (two module
languages, one ``_sc_modules``) and §11 (a reload is best-effort).

A plugin is an ordinary ``pip``-installable distribution that says what it
supplies with decorators::

    import saltcorn as sc

    sc.settings(sc.Field.string("api_key", label="API key", secret=True))

    @sc.on_load
    def load(configuration):
        global client
        client = Scorer(configuration["api_key"])

    @sc.action(description="Score a lead",
               config=[sc.Field.string("model", required=True)])
    def score_lead(row, config, user):
        return {"score": client.score(row["email"], model=config["model"])}

    @sc.function(description="Markdown to HTML")
    def md_to_html(text: str) -> str: ...

    @sc.table_provider("CSV file", config=[sc.Field.string("path", required=True)])
    class CsvTable: ...

Everything above is re-exported from ``saltcorn`` so an author writes ``sc.``
and never names this module; the host imports *this* one, because the ops below
are its side of the same conversation.

# The registry is per package, and the key is the code's own ``__module__``

One interpreter holds every plugin (§1), so two of them must not be able to see
each other's declarations. Each registration is filed under the **root package**
of the decorated object's own ``__module__`` — not under "whichever plugin the
host happens to be importing", which would misfile anything a plugin registered
outside its own import, and not under a global list, which would merge them.
``settings()`` decorates nothing, so it reads its caller's frame instead, which
is the same name one level of indirection away.

# What the host asks for, and what it is told

One entry point, [`dispatch`], and one payload each way. The manifest it answers
a load with is the **same shape** a JavaScript module answers
(``sc_module::ModuleManifest``), which is what lets one Modules tab, one action
registry and one pair of catalog hosts serve both languages without either
knowing the other exists.

# What is *not* here

A sandbox (§10). A plugin's code runs with the server's privileges, as its
``pip install`` already did, and this module does not pretend otherwise: there
is no gate on a plugin's imports, because a plugin is a package an admin chose
to install rather than a body somebody typed into a form.
"""

import importlib
import importlib.metadata
import inspect
import os
import re
import sys
import traceback

#: The distribution metadata group a package advertises its plugin under — the
#: idiomatic way a Python distribution says "this is a plugin of X" (§9).
ENTRY_POINT_GROUP = "saltcorn.plugins"

#: The parameters an action may ask for by name (§2). Anything else in a
#: signature is a mistake, and is refused by name rather than passed as ``None``.
ACTION_PARAMETERS = (
    "row",
    "old",
    "table",
    "user",
    "payload",
    "config",
    "configuration",
    "trigger",
    "mode",
)

#: The types [`Field`] offers, in **this system's** vocabulary rather than v1's
#: (§2: "it declares its settings in this system's field vocabulary").
FIELD_TYPES = ("string", "int", "float", "bool", "date", "json")

#: Python annotations that mean one of those, for the signature a function
#: reports. An annotation this does not know is reported under its own name: a
#: signature is a hint in an editor, not a contract anything here enforces.
_ANNOTATIONS = {
    str: "string",
    int: "int",
    float: "float",
    bool: "bool",
    dict: "json",
    list: "json",
}


class Field:
    """One setting, in the vocabulary every configurable thing here speaks.

    A thin constructor over this system's ``FormField`` (§6.2): what an admin
    sees is rendered by the same trigger and settings forms that render a file
    store's backend, which is why no screen has to learn what a Python module
    is.

    ``primary_key`` and ``unique`` are past §2's list and mean nothing for a
    *setting*: they are here because a **table provider**'s ``fields()`` answers
    the same objects, and a provided table with no primary key is a table
    nothing can address a row of.
    """

    __slots__ = (
        "name",
        "type",
        "label",
        "required",
        "default",
        "options",
        "secret",
        "multiline",
        "primary_key",
        "unique",
    )

    def __init__(
        self,
        name,
        type="string",  # noqa: A002 — the field's own word for it
        label=None,
        required=False,
        default=None,
        options=None,
        secret=False,
        multiline=False,
        primary_key=False,
        unique=False,
    ):
        if not isinstance(name, str) or not name.strip():
            raise ValueError("a field needs a name")
        if type not in FIELD_TYPES:
            raise ValueError(
                f"`{type}` is not a field type; the types are {', '.join(FIELD_TYPES)}"
            )
        self.name = name.strip()
        self.type = type
        self.label = label
        self.required = bool(required)
        self.default = default
        self.options = list(options) if options is not None else None
        self.secret = bool(secret)
        self.multiline = bool(multiline)
        self.primary_key = bool(primary_key)
        self.unique = bool(unique)

    @classmethod
    def string(cls, name, **kwargs):
        return cls(name, "string", **kwargs)

    @classmethod
    def int(cls, name, **kwargs):  # noqa: A003
        return cls(name, "int", **kwargs)

    @classmethod
    def float(cls, name, **kwargs):  # noqa: A003
        return cls(name, "float", **kwargs)

    @classmethod
    def bool(cls, name, **kwargs):  # noqa: A003
        return cls(name, "bool", **kwargs)

    @classmethod
    def date(cls, name, **kwargs):
        return cls(name, "date", **kwargs)

    @classmethod
    def json(cls, name, **kwargs):
        return cls(name, "json", **kwargs)

    def to_json(self):
        """The declaration as it crosses the seam, for the host to translate."""
        declared = {
            "name": self.name,
            "type": self.type,
            "required": self.required,
            "secret": self.secret,
            "multiline": self.multiline,
        }
        if self.label is not None:
            declared["label"] = self.label
        if self.default is not None:
            declared["default"] = self.default
        if self.options is not None:
            declared["options"] = self.options
        if self.primary_key:
            declared["primary_key"] = True
        if self.unique:
            declared["unique"] = True
        return declared

    def __repr__(self):
        return f"Field({self.name!r}, {self.type!r})"


def _fields(declared, what):
    """A ``config=`` list as [`Field`]s, refusing anything that is not one."""
    out = []
    for field in declared or ():
        if isinstance(field, Field):
            out.append(field)
        elif isinstance(field, dict):
            # A dict of the same keys, for a plugin that would rather build its
            # settings than write them out.
            out.append(Field(**field))
        else:
            raise TypeError(
                f"{what} declares a setting that is not a saltcorn.Field: {field!r}"
            )
    return out


class _Action:
    __slots__ = ("name", "fn", "description", "config", "require_row")

    def __init__(self, name, fn, description, config, require_row):
        self.name = name
        self.fn = fn
        self.description = description
        self.config = config
        self.require_row = require_row


class _Function:
    __slots__ = ("name", "fn", "description")

    def __init__(self, name, fn, description):
        self.name = name
        self.fn = fn
        self.description = description


class _Provider:
    __slots__ = ("name", "cls", "config", "_instance")

    def __init__(self, name, cls, config):
        self.name = name
        self.cls = cls
        self.config = config
        self._instance = None

    def instance(self):
        """The provider object, built once.

        Lazily, because a provider's ``__init__`` is the plugin's code and a
        module that would not load must still be able to show its settings form
        — which is what an admin needs to fix it.
        """
        if self._instance is None:
            self._instance = self.cls()
        return self._instance


class Registry:
    """What one package declared. One per package, kept apart by name."""

    def __init__(self, package):
        self.package = package
        #: The distribution the host loaded it as, once it has.
        self.distribution = None
        self.settings = []
        self.on_load = None
        self.actions = {}
        self.functions = {}
        self.providers = {}
        self.issues = []
        #: The module's own configuration, as the last load was given it.
        self.configuration = {}


#: Every package that has declared anything, by root package name.
_REGISTRIES = {}

#: The registries the host has loaded, by normalised distribution name — the
#: name `_sc_modules` holds and every host call arrives under.
_BY_DISTRIBUTION = {}


def normalise(name):
    """A distribution name in the one spelling everything here compares."""
    return re.sub(r"[-_.]+", "-", str(name)).strip().lower()


def _root(module_name):
    return (module_name or "").split(".")[0]


def _registry(package):
    registry = _REGISTRIES.get(package)
    if registry is None:
        registry = Registry(package)
        _REGISTRIES[package] = registry
    return registry


def _registry_of(obj):
    """The registry of whatever package `obj` was defined in."""
    return _registry(_root(getattr(obj, "__module__", None)))


# --- the decorators ---------------------------------------------------------


def settings(*fields):
    """Declare the module's own settings — what the Modules tab asks an admin.

    Called at module level rather than as a decorator, because it decorates
    nothing: the fields *are* the declaration. The package it belongs to is the
    caller's own module, which is the same answer the decorators get from
    ``__module__``.
    """
    package = _root(sys._getframe(1).f_globals.get("__name__"))
    registry = _registry(package)
    registry.settings = _fields(fields, f"the module `{package}`")
    return registry.settings


def on_load(fn):
    """Called at load and after every configuration change (§11).

    Where a plugin builds what its actions close over: a client, a model, a
    connection. Its failure is an **issue** on the module rather than a failed
    load, so an admin whose API key is wrong still gets the settings form that
    is where they would fix it.
    """
    _registry_of(fn).on_load = fn
    return fn


def action(fn=None, *, name=None, description="", config=(), require_row=False):
    """Register an action, which is an ordinary trigger action to this server.

    ``config`` is what the *trigger form* asks for when an admin picks this
    action; the module's own settings are separate and reach the function as
    ``configuration``.

    The function asks for what it wants — any of ``row``, ``old``, ``table``,
    ``user``, ``payload``, ``config``, ``configuration``, ``trigger``, ``mode``,
    or ``**kwargs`` for all of them.
    """

    def register(fn):
        registered = name or fn.__name__
        registry = _registry_of(fn)
        if inspect.iscoroutinefunction(fn):
            # Nothing here can await one: a run is a thread, and there is no
            # event loop in the interpreter to give it. Said now, by name,
            # rather than as a coroutine object arriving where a result was
            # expected.
            registry.issues.append(
                f"the action `{registered}` is `async def`, which this version cannot "
                f"call; it is not available"
            )
            return fn
        registry.actions[registered] = _Action(
            registered,
            fn,
            description or (inspect.getdoc(fn) or "").split("\n")[0],
            _fields(config, f"the action `{registered}`"),
            bool(require_row),
        )
        return fn

    return register(fn) if fn is not None else register


def function(fn=None, *, name=None, description=""):
    """Register a function: callable from a formula and from a code body.

    The signature the editor shows is read from the function itself
    (``inspect``), which is the one place this API is better than the JavaScript
    one rather than merely different.
    """

    def register(fn):
        registered = name or fn.__name__
        registry = _registry_of(fn)
        if inspect.iscoroutinefunction(fn):
            registry.issues.append(
                f"the function `{registered}` is `async def`, which this version cannot "
                f"call; it is not available"
            )
            return fn
        registry.functions[registered] = _Function(
            registered,
            fn,
            description or (inspect.getdoc(fn) or "").split("\n")[0],
        )
        return fn

    return register(fn) if fn is not None else register


def table_provider(name, *, config=()):
    """Register a table provider: a table whose rows this class produces.

    The class answers ``fields(configuration)`` and ``rows(configuration, …)``,
    and *may* answer ``insert_row``, ``update_row`` and ``delete_rows`` — whose
    **presence** is what makes a table backed by it writable, which is v1's rule
    too.
    """

    def register(cls):
        _registry_of(cls).providers[name] = _Provider(
            name, cls, _fields(config, f"the table provider `{name}`")
        )
        return cls

    return register


# --- discovery --------------------------------------------------------------


def _entry_point_module(distribution):
    """The module a distribution advertises as its plugin, if it does (§9)."""
    for entry in distribution.entry_points:
        if entry.group == ENTRY_POINT_GROUP:
            # `module:attribute` is the entry-point syntax; what is imported is
            # the module, because the declarations are made by importing it.
            return entry.value.split(":")[0].strip()
    return None


def _top_level(distribution):
    """The distribution's own top-level package, off its metadata."""
    declared = None
    try:
        declared = distribution.read_text("top_level.txt")
    except (OSError, ValueError):
        declared = None
    for line in (declared or "").splitlines():
        if line.strip():
            return line.strip()
    # No `top_level.txt` — a wheel built by something other than setuptools.
    # The installed files say the same thing: the first directory that is not
    # metadata is the package.
    for path in distribution.files or ():
        parts = str(path).split("/")
        if len(parts) > 1 and not parts[0].endswith((".dist-info", ".egg-info", ".data")):
            return parts[0]
    return None


def module_name(name):
    """Which module to import for the distribution `name` (§9).

    The ``saltcorn.plugins`` entry point when the distribution declares one,
    else its top-level package, else the name itself with the spelling a
    distribution name is allowed and a module name is not.
    """
    try:
        distribution = importlib.metadata.distribution(name)
    except Exception:  # noqa: BLE001 — not installed, or metadata that will not parse
        distribution = None
    if distribution is not None:
        found = _entry_point_module(distribution) or _top_level(distribution)
        if found:
            return found
    return normalise(name).replace("-", "_")


def _forget(package):
    """Drop a package's modules from ``sys.modules`` (§11).

    Correct for a pure-Python package and best-effort for anything else: every
    object built from the old modules lives on, and a package with a C extension
    in it cannot be re-initialised at all. The Modules tab says a version change
    takes full effect at the next restart, which is the guarantee this is not.
    """
    for name in [n for n in sys.modules if n == package or n.startswith(package + ".")]:
        sys.modules.pop(name, None)


# --- the manifest -----------------------------------------------------------


def _annotation(parameter):
    """The declared type of one parameter, in this system's vocabulary."""
    annotation = parameter.annotation
    if annotation is inspect.Parameter.empty:
        return None
    known = _ANNOTATIONS.get(annotation)
    if known is not None:
        return known
    return getattr(annotation, "__name__", None) or str(annotation)


def _arguments(fn):
    """A function's signature, as the manifest reports it."""
    try:
        signature = inspect.signature(fn)
    except (TypeError, ValueError):
        # A builtin or a C function has no signature to read. Reported as
        # unknown rather than as none, which would read as "takes nothing".
        return []
    out = []
    for parameter in signature.parameters.values():
        if parameter.kind in (parameter.VAR_POSITIONAL, parameter.VAR_KEYWORD):
            continue
        out.append({"name": parameter.name, "type": _annotation(parameter)})
    return out


def manifest(distribution, registry):
    """What this package supplies, in the shape both languages answer with."""
    return {
        "name": distribution,
        "plugin_name": registry.package,
        "actions": [
            {
                "name": action.name,
                "description": action.description,
                "requireRow": action.require_row,
                "configFields": [field.to_json() for field in action.config],
            }
            for action in registry.actions.values()
        ],
        "functions": [
            {
                "name": function.name,
                "description": function.description,
                # Nothing here is awaited: an `async def` never reaches the
                # manifest, so this is what a signature says and not a promise.
                "isAsync": False,
                "arguments": _arguments(function.fn),
            }
            for function in registry.functions.values()
        ],
        "table_providers": [
            {
                "name": provider.name,
                "config_fields": [field.to_json() for field in provider.config],
            }
            for provider in registry.providers.values()
        ],
        "config_fields": [field.to_json() for field in registry.settings],
        # Every entity type this version does not load is one a Python plugin
        # has no way to declare in the first place: there is no decorator for a
        # view, a type or a fieldview, so there is nothing to count.
        "unsupported": [],
        "issues": list(registry.issues),
    }


# --- errors -----------------------------------------------------------------


def format_error(exc):
    """A plugin's failure as its own frames, most recent last.

    This module's frames and the surface's are dropped: somebody looking at
    ``score_lead`` failing wants the line in ``their`` file, not the dispatch
    that reached it.
    """
    kind = type(exc).__name__
    text = str(exc)
    head = f"{kind}: {text}" if text else kind
    lines = []
    for frame in traceback.extract_tb(exc.__traceback__):
        if frame.filename.startswith("<saltcorn"):
            continue
        where = os.path.basename(frame.filename)
        source = (frame.line or "").strip()
        lines.append(
            f"  {where}, line {frame.lineno}, in {frame.name}"
            + (f": {source}" if source else "")
        )
    if lines:
        return head + "\n" + "\n".join(lines[-4:])
    return head


def _require(distribution):
    registry = _BY_DISTRIBUTION.get(normalise(distribution))
    if registry is None:
        raise LookupError(
            f"the Python module `{distribution}` is not loaded in this interpreter; "
            f"it may have been uninstalled, or failed to load"
        )
    return registry


def _selected(fn, available, what):
    """The arguments `fn` asked for, of the ones there are.

    ``**kwargs`` takes them all. A parameter that is not one of the available
    names is refused **by name**, rather than passed as ``None``: a plugin that
    asks for `rows` when the name is `row` has a typo, and finding it as an
    empty value inside somebody's action is the silent failure this system
    refuses.
    """
    try:
        signature = inspect.signature(fn)
    except (TypeError, ValueError):
        return dict(available)
    selected = {}
    for parameter in signature.parameters.values():
        if parameter.kind == parameter.VAR_KEYWORD:
            return dict(available)
        if parameter.kind == parameter.VAR_POSITIONAL:
            continue
        if parameter.name in available:
            selected[parameter.name] = available[parameter.name]
        elif parameter.default is inspect.Parameter.empty:
            raise TypeError(
                f"{what} asks for `{parameter.name}`, which is not one of the "
                f"parameters it is passed: {', '.join(sorted(available))}"
            )
    return selected


# --- the ops ----------------------------------------------------------------


def op_load(payload):
    """Import a distribution's plugin module and read what it declared."""
    distribution = payload["module"]
    configuration = payload.get("configuration") or {}
    site_packages = payload.get("site_packages")
    if site_packages and site_packages not in sys.path:
        # The environment may have been created, or installed into, after this
        # interpreter fixed its `sys.path` at start — an install is a
        # subprocess, and the boot path cannot know what a later one will put
        # there.
        sys.path.append(site_packages)
    # A distribution installed since the last import is invisible to the
    # finders' directory caches until this is called.
    importlib.invalidate_caches()

    name = module_name(distribution)
    package = _root(name)
    # §11: a reload is a re-import, so the old registrations go first — a
    # decorator that is no longer there must not survive as an action.
    _forget(package)
    _REGISTRIES.pop(package, None)
    module = importlib.import_module(name)

    registry = _REGISTRIES.get(package)
    if registry is None:
        registry = _registry(package)
        registry.issues.append(
            f"`{getattr(module, '__name__', name)}` declared nothing: a Saltcorn Python "
            f"plugin registers what it supplies with the `saltcorn` decorators "
            f"(`@saltcorn.action`, `@saltcorn.function`, `@saltcorn.table_provider`)"
        )
    registry.distribution = distribution
    registry.configuration = configuration
    _BY_DISTRIBUTION[normalise(distribution)] = registry

    if registry.on_load is not None:
        try:
            registry.on_load(configuration)
        except Exception as exc:  # noqa: BLE001 — reported, not fatal
            registry.issues.append(f"its `on_load` failed: {format_error(exc)}")
    return manifest(distribution, registry)


def op_unload(payload):
    """Forget a distribution, after an uninstall.

    The **import** stays behind, which §11 says out loud: Python has no unload,
    and a package whose modules were dropped from ``sys.modules`` has still left
    every object it built alive. What this does is what can be done — nothing
    here will answer for it again.
    """
    registry = _BY_DISTRIBUTION.pop(normalise(payload["module"]), None)
    if registry is not None:
        _REGISTRIES.pop(registry.package, None)
        _forget(registry.package)
    return None


def op_action(payload):
    """Run one action, passing only the parameters it declared (§2)."""
    registry = _require(payload["module"])
    name = payload["action"]
    action = registry.actions.get(name)
    if action is None:
        raise LookupError(
            f"the Python module `{registry.distribution}` supplies no action `{name}`"
        )
    available = dict(payload.get("args") or {})
    # The module's own settings are the host's to supply rather than the
    # caller's: they are what an admin typed on the Modules tab, and a trigger
    # cannot change them.
    available["configuration"] = registry.configuration
    for parameter in ACTION_PARAMETERS:
        available.setdefault(parameter, None)
    return action.fn(**_selected(action.fn, available, f"the action `{name}`"))


def op_function(payload):
    """Call one function with v1's positional arguments."""
    registry = _require(payload["module"])
    name = payload["function"]
    function = registry.functions.get(name)
    if function is None:
        raise LookupError(
            f"the Python module `{registry.distribution}` supplies no function `{name}`"
        )
    return function.fn(*(payload.get("args") or []))


def _provider(payload):
    registry = _require(payload["module"])
    name = payload["provider"]
    provider = registry.providers.get(name)
    if provider is None:
        raise LookupError(
            f"the Python module `{registry.distribution}` supplies no table provider "
            f"`{name}`"
        )
    return provider


def _provider_call(payload, method, arguments, required=True):
    provider = _provider(payload)
    instance = provider.instance()
    fn = getattr(instance, method, None)
    if fn is None:
        if not required:
            return None
        raise TypeError(
            f"the table provider `{provider.name}` has no `{method}`, which every "
            f"provider must answer"
        )
    available = dict(arguments)
    available["configuration"] = payload.get("configuration") or {}
    return fn(**_selected(fn, available, f"the table provider `{provider.name}`"))


def op_provider_fields(payload):
    """The columns this provider presents for one configuration."""
    declared = _provider_call(payload, "fields", {}) or []
    return [field.to_json() if isinstance(field, Field) else field for field in declared]


def op_provider_rows(payload):
    rows = _provider_call(
        payload,
        "rows",
        {
            "where": payload.get("where") or {},
            "options": payload.get("options") or {},
            "table": payload.get("table"),
        },
    )
    return list(rows or [])


def op_provider_writes(payload):
    """Which writes this provider answers — by which methods it **defines**."""
    instance = _provider(payload).instance()
    return {
        "insert": callable(getattr(instance, "insert_row", None)),
        "update": callable(getattr(instance, "update_row", None)),
        "delete": callable(getattr(instance, "delete_rows", None)),
    }


def op_provider_insert(payload):
    key = _provider_call(payload, "insert_row", {"record": payload.get("record")})
    return {"key": key}


def op_provider_update(payload):
    _provider_call(
        payload,
        "update_row",
        {"record": payload.get("record"), "id": payload.get("id")},
    )
    return None


def op_provider_delete(payload):
    _provider_call(payload, "delete_rows", {"where": payload.get("where") or {}})
    return None


_OPS = {
    "load": op_load,
    "unload": op_unload,
    "action": op_action,
    "function": op_function,
    "provider_fields": op_provider_fields,
    "provider_rows": op_provider_rows,
    "provider_writes": op_provider_writes,
    "provider_insert": op_provider_insert,
    "provider_update": op_provider_update,
    "provider_delete": op_provider_delete,
}


def dispatch(op, payload):
    """The one entry point the host calls, for every op it has."""
    handler = _OPS.get(op)
    if handler is None:
        # Unreachable from the Rust side, which spells all of them.
        raise LookupError(f"`{op}` is not something a Python module can be asked")
    return handler(payload)
