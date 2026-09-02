"""The surface an app builder writes: ``db``, and the errors it raises.

This is ``DB_PRELUDE``'s counterpart, and it is Python for the reason the
prelude is JavaScript: the Rust side sees **plans**, so adding a chain method
touches no Rust, and the two languages cannot drift apart about what a chain
means because they lower to the same object and it is resolved in one place.

It is shipped inside the binary and installed by a meta-path loader at
interpreter start, so there is no file to find, no version to skew and nothing
to ``pip install`` for the surface itself.

Chain methods are **pure and cheap** — each answers a new query and touches
nothing — and terminals execute::

    overdue = (db.invoices
        .where(paid=False, due__lt=payload["today"])
        .select("id", "amount", "customerⱵemail")
        .order_by("due")
        .limit(50)
        .rows())

Nothing is awaited. A terminal blocks the run's own thread with the GIL
released, so every other resident run executes while this one waits.

Two kinds of error, kept apart on purpose. A mistake in what the *body* wrote —
an operator that is not one, a direction that is not ``asc`` or ``desc`` — is a
``TypeError`` or a ``ValueError``, which is what a Python author expects of a
library and what a bare ``except DbError`` in a retry loop must not swallow. A
**refusal** — an ownership rule that would not allow a write, a budget spent, a
whole-table update — is a ``DbError``, because it is the same refusal the host
makes and a body may legitimately catch it and fall back.
"""

import re

# The seam: five host functions and the exception hierarchy, in a built-in
# module the interpreter was started with. Bound here at module level, where
# Python's private-name mangling does not apply — inside a class body
# `__sc.__sc_db` would silently become `__sc._Query__sc_db`.
from __sc import __sc_db as _call_db
from __sc import (
    DbError,
    FetchError,
    FileError,
    ModuleError,
    SaltcornError,
    Timeout,
    TriggerError,
)

__all__ = [
    "Db",
    "DbError",
    "FetchError",
    "FileError",
    "ModuleError",
    "Query",
    "SaltcornError",
    "Timeout",
    "TriggerError",
    "and_",
    "db",
    "not_",
    "or_",
]

#: What a filter may say about one column — the vocabulary every other surface
#: in this system speaks, spelled as a keyword suffix: ``due__lt=today``.
OPERATORS = (
    "eq",
    "ne",
    "gt",
    "gte",
    "lt",
    "lte",
    "in",
    "nin",
    "like",
    "ilike",
    "is_null",
)

#: The key the **formula** spelling of a filter rides under.
_FORMULA = "formula"

#: An aggregate is written the way a formula writes one — ``count()``,
#: ``sum(price * qty)`` — and lowers to the plan's own ``{alias, fn, arg}``.
_AGGREGATE = re.compile(r"^\s*([A-Za-z_][A-Za-z_0-9]*)\s*\(([\s\S]*)\)\s*$")


def _condition(value):
    """One filter, in either of the two spellings §3 gives."""
    if isinstance(value, str):
        return {_FORMULA: value}
    if isinstance(value, dict):
        return value
    raise TypeError(
        "a condition is keywords (paid=False), a filter dict "
        "({'due': {'lt': '2026-01-01'}}) or a formula string, not a "
        f"{type(value).__name__}"
    )


def _split_operator(key):
    """``due__lt`` as ``("due", "lt")``; a bare name as ``(name, None)``.

    Only a trailing ``__<op>`` naming one of :data:`OPERATORS` is read as an
    operator, so a column whose own name ends that way is reached with the dict
    spelling instead — which can say everything the keyword one can.
    """
    for op in OPERATORS:
        suffix = "__" + op
        if key.endswith(suffix) and len(key) > len(suffix):
            return key[: -len(suffix)], op
    return key, None


def _keyword_condition(kwargs):
    """``.where(paid=False, due__lt=x)`` as one filter object.

    Flat while the fields are distinct, which is the common plan and the one a
    reader of the plan recognises; two comparisons on **one** field cannot share
    a key, so those become an explicit ``and``.
    """
    flat = {}
    extra = []
    for key, value in kwargs.items():
        field, op = _split_operator(key)
        term = value if op is None else {op: value}
        if field in flat:
            extra.append({field: term})
        else:
            flat[field] = term
    if not extra:
        return flat
    return {"and": [{field: term} for field, term in flat.items()] + extra}


def and_(*conditions):
    """Every one of these — what several ``.where()`` calls already mean."""
    return {"and": [_condition(c) for c in conditions]}


def or_(*conditions):
    """Any one of these."""
    return {"or": [_condition(c) for c in conditions]}


def not_(condition):
    """The opposite of this one."""
    return {"not": _condition(condition)}


def _projections(value):
    """One argument of ``.select()`` as the plan's projections."""
    if isinstance(value, str):
        return [value]
    if isinstance(value, dict):
        return [
            {"alias": alias, _FORMULA: formula} for alias, formula in value.items()
        ]
    raise TypeError(
        "select() takes field names, Ⱶ-paths and alias=\"formula\" keywords, "
        f"not a {type(value).__name__}"
    )


def _aggregates(spec):
    """``.aggregate(total="sum(amount)")`` as the plan's own ``{alias, fn, arg}``."""
    out = []
    for alias, source in spec.items():
        found = _AGGREGATE.match(str(source))
        if found is None:
            raise ValueError(
                f"`{source}` is not an aggregate: write count(), sum(field) or "
                "sum(an expression)"
            )
        arg = found.group(2).strip()
        out.append({"alias": alias, "fn": found.group(1), "arg": arg or None})
    return out


def _direction(dir):
    if dir is None:
        return "asc"
    if isinstance(dir, str) and dir.lower() in ("asc", "desc"):
        return dir.lower()
    raise ValueError(f'order_by() sorts "asc" or "desc", not `{dir}`')


def _whole_number(value, what):
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        raise ValueError(f"{what} takes a whole number of rows, e.g. .{what}(50)")
    return value


class Query:
    """One table, and what has been said about it so far.

    Immutable: every chain method answers a **new** query, so a query held in a
    variable can be narrowed two ways without either narrowing the other, and a
    ``for`` loop over one may be walked twice.
    """

    __slots__ = (
        "_table",
        "_authority",
        "_where",
        "_select",
        "_order",
        "_group",
        "_having",
        "_aggregate",
        "_limit",
        "_offset",
    )

    def __init__(self, table, authority):
        self._table = table
        self._authority = authority
        self._where = ()
        self._select = ()
        self._order = ()
        self._group = ()
        self._having = ()
        self._aggregate = ()
        self._limit = None
        self._offset = None

    # -- the chain, which touches nothing ---------------------------------

    def _derive(self, **patch):
        clone = Query(self._table, self._authority)
        for slot in Query.__slots__:
            setattr(clone, slot, getattr(self, slot))
        for name, value in patch.items():
            setattr(clone, "_" + name, value)
        return clone

    def where(self, *conditions, **kwargs):
        """Narrow the rows. Several conditions, and several calls, are ANDed."""
        added = [_condition(c) for c in conditions]
        if kwargs:
            added.append(_keyword_condition(kwargs))
        if not added:
            raise TypeError(
                "where() needs a condition: keywords (paid=False), a filter "
                "dict, or a formula string"
            )
        return self._derive(where=self._where + tuple(added))

    def select(self, *columns, **aliased):
        """Which values to answer: columns, Ⱶ-paths, and aliased formulas."""
        added = []
        for column in columns:
            added.extend(_projections(column))
        added.extend(_projections(aliased))
        return self._derive(select=self._select + tuple(added))

    def order_by(self, field, dir=None):
        """Sort by `field`, ascending unless told otherwise."""
        return self._derive(
            order=self._order + ({"field": field, "dir": _direction(dir)},)
        )

    def group_by(self, *fields):
        """One answer row per distinct combination of these."""
        return self._derive(group=self._group + tuple(fields))

    def aggregate(self, **spec):
        """The aggregate values a group answers: ``total="sum(amount)"``."""
        return self._derive(aggregate=self._aggregate + tuple(_aggregates(spec)))

    def having(self, *conditions, **kwargs):
        """Narrow the **groups**, by this aggregate's own aliases."""
        added = [_condition(c) for c in conditions]
        if kwargs:
            added.append(_keyword_condition(kwargs))
        if not added:
            raise TypeError("having() needs a condition")
        return self._derive(having=self._having + tuple(added))

    def limit(self, rows):
        """At most this many rows."""
        return self._derive(limit=_whole_number(rows, "limit"))

    def offset(self, rows):
        """Skip this many first."""
        return self._derive(offset=_whole_number(rows, "offset"))

    def as_user(self):
        """Run under the event's **caller**, where §7.3's ownership rule decides."""
        return self._derive(authority="user")

    def as_admin(self):
        """Run under the trigger's own authority — the default."""
        return self._derive(authority="admin")

    # -- the plan ----------------------------------------------------------

    def _plan(self, op, **extra):
        plan = {"op": op, "table": self._table, "authority": self._authority}
        # Repeated .where() calls AND; one is itself, so the common plan is flat.
        if len(self._where) == 1:
            plan["where"] = self._where[0]
        elif self._where:
            plan["where"] = {"and": list(self._where)}
        if self._select:
            plan["select"] = list(self._select)
        if self._order:
            plan["order"] = list(self._order)
        if self._group:
            plan["group"] = list(self._group)
        if len(self._having) == 1:
            plan["having"] = self._having[0]
        elif self._having:
            plan["having"] = {"and": list(self._having)}
        if self._aggregate:
            plan["aggregate"] = list(self._aggregate)
        if self._limit is not None:
            plan["limit"] = self._limit
        if self._offset is not None:
            plan["offset"] = self._offset
        plan.update(extra)
        return plan

    def _bounded(self, op):
        """A whole table rewritten or emptied is not something an omitted call
        should be able to cause. The host refuses it too; here it is named at the
        place the author can see."""
        if not self._where:
            raise DbError(
                f"db.{self._table}.{op}() without a .where() would touch every "
                "row; add a .where()"
            )

    def _read(self, **extra):
        """What a terminal reads: the rows of a select, or the groups of an
        aggregate — which is one object when there is nothing to group by."""
        if not self._aggregate:
            if self._group:
                raise ValueError(
                    f"db.{self._table}.group_by(...) needs an .aggregate(...): a "
                    "group answers aggregate values, so say which"
                )
            return _call_db(self._plan("select", **extra))
        answer = _call_db(self._plan("aggregate", **extra))
        return answer if isinstance(answer, list) else [answer]

    def _scalar(self, fn, arg=None):
        """A scalar terminal is one nameless group: the same plan, the one value
        unwrapped. Grouped, there is no one value to unwrap, so it says so rather
        than answering the first group's."""
        if self._group or self._aggregate:
            raise ValueError(
                f".{fn}() answers one value, and this query groups; ask for it by "
                f'name: .aggregate({fn}="{fn}({arg or ""})").rows()'
            )
        answer = _call_db(
            self._plan("aggregate", aggregate=[{"alias": "value", "fn": fn, "arg": arg}])
        )
        if not isinstance(answer, dict):
            return None
        return answer.get("value")

    # -- the terminals, which execute --------------------------------------

    def rows(self):
        """Every matching row, as a ``list`` of ``dict``."""
        return self._read()

    def iter(self, batch=None):
        """The rows, **one batch per host call**, as a generator.

        The interpreter holds one batch rather than the whole answer, so a body
        can walk a table it could never fit in memory — and a body that stops
        early has paid for only what it read, because nothing is fetched until
        the loop asks for it. What bounds it is the call budget rather than the
        row cap.

        The order is the host's business: it appends the primary key to whatever
        this query sorts by, so no two rows tie and no batch boundary can skip or
        repeat one. A ``.limit()`` bounds the **iteration** and is spent here, by
        stopping; an ``.offset()`` skips rows once, at the start.
        """
        if self._aggregate or self._group:
            raise ValueError(
                f"db.{self._table}.iter() streams rows, and this query aggregates "
                "them; ask for the groups with .rows(), which answers them all at "
                "once"
            )
        if batch is not None and (
            isinstance(batch, bool) or not isinstance(batch, int) or batch < 1
        ):
            raise ValueError(
                "iter()'s argument is how many rows to read at a time, e.g. "
                ".iter(200)"
            )
        total = self._limit
        taken = 0
        after = None
        while True:
            want = batch
            if total is not None:
                left = total - taken
                if left <= 0:
                    return
                if want is None or left < want:
                    want = left
            plan = self._plan("select", cursor=True)
            if want is not None:
                plan["limit"] = want
            if after is not None:
                plan["after"] = after
                # The host refuses a resumed batch that carries an offset, which
                # is the same rule said where a guest cannot reach it.
                plan.pop("offset", None)
            answer = _call_db(plan)
            for row in answer["rows"]:
                yield row
                taken += 1
                if total is not None and taken >= total:
                    return
            if answer.get("cursor") is None:
                return
            after = answer["cursor"]

    def __iter__(self):
        """``for row in db.books.where(...)`` — the same walk :meth:`iter` is."""
        return self.iter()

    def first(self):
        """The first matching row, or ``None``."""
        found = self._read(limit=1)
        return found[0] if found else None

    def get(self, pk):
        """The row with this primary key, or ``None``."""
        found = _call_db(self._plan("select", pk=pk, limit=1))
        return found[0] if found else None

    def exists(self):
        """Whether anything matches."""
        return len(_call_db(self._plan("select", limit=1))) > 0

    def count(self):
        """How many rows match."""
        return self._scalar("count")

    def sum(self, field):
        """The total of `field` over the matching rows."""
        return self._scalar("sum", field)

    def avg(self, field):
        """The mean of `field` over the matching rows."""
        return self._scalar("avg", field)

    def min(self, field):
        """The least `field` among the matching rows."""
        return self._scalar("min", field)

    def max(self, field):
        """The greatest `field` among the matching rows."""
        return self._scalar("max", field)

    def insert(self, values=None, **fields):
        """Write a row, or a list of rows.

        ``insert(title="Dune")``, ``insert({"title": "Dune"})`` and
        ``insert([{...}, {...}])`` are the same call spelled three ways. The
        write goes through the row layer, so it is coerced against its columns,
        validated, and **observed by triggers** exactly as an API caller's write
        is.
        """
        if values is not None and fields:
            raise TypeError(
                "insert() takes the row as keywords or as one dict, not both"
            )
        row = fields if values is None else values
        if not isinstance(row, (dict, list, tuple)):
            raise TypeError(
                "insert() takes keywords, a dict, or a list of dicts, not a "
                f"{type(row).__name__}"
            )
        if isinstance(row, tuple):
            row = list(row)
        return _call_db(self._plan("insert", values=row))

    def update(self, values=None, **fields):
        """Change every matching row. **Refused without a** ``.where()``."""
        if values is not None and fields:
            raise TypeError(
                "update() takes the assignments as keywords or as one dict, not both"
            )
        assignments = fields if values is None else values
        if not isinstance(assignments, dict):
            raise TypeError(
                "update() takes keywords or a dict of assignments, not a "
                f"{type(assignments).__name__}"
            )
        self._bounded("update")
        return _call_db(self._plan("update", values=assignments))

    def delete(self):
        """Remove every matching row. **Refused without a** ``.where()``."""
        self._bounded("delete")
        return _call_db(self._plan("delete"))

    def __repr__(self):
        return f"<saltcorn query on `{self._table}` as {self._authority}>"


class Db:
    """The tables, under one authority.

    ``db.invoices`` is a table and ``db.table("customer orders")`` is the same
    thing for a name that is not an identifier — so a table called ``table``,
    ``sql``, ``as_user`` or ``as_admin`` is reached the second way, the four
    names this handle has of its own being the only ones attribute access does
    not answer with a query.
    """

    __slots__ = ("_authority",)

    def __init__(self, authority="admin"):
        self._authority = authority

    def table(self, name):
        """The table called `name`, whatever it is called."""
        return Query(name, self._authority)

    def as_user(self):
        """Delegate to the event's **caller**: §7.3's ownership rule then decides
        every row, and a refusal is a catchable :class:`DbError`."""
        return Db("user")

    def as_admin(self):
        """The trigger's own authority — the default, because a trigger is
        server-side configuration and the audit row a caller may not insert is
        the archetype of what a trigger exists to write."""
        return Db("admin")

    def sql(self, sql, params=None, as_user=None):
        """The body's own SQL, for the question the chain does not ask — a window
        function, a recursive CTE, an ``ON CONFLICT``::

            ranked = db.sql(
                "select owner, title, rank() over (partition by owner "
                "order by pages desc) as r from books where pages > $1",
                [200],
            )

        The text is the author's and runs as written; the values are **binds**
        and never part of it. It is the same admission a custom SQL query is, and
        it carries the same consequences: raw SQL does not go through the row
        layer, so no ownership formula filters it, no rich type coerces it, and a
        write inside one **raises no table event**. The row cap, the call budget
        and the caller's transaction all still apply.
        """
        if not isinstance(sql, str):
            raise TypeError('sql() takes the SQL text, e.g. db.sql("select 1 as n")')
        if params is None:
            params = []
        elif isinstance(params, tuple):
            params = list(params)
        elif not isinstance(params, list):
            raise TypeError(
                "sql()'s second argument is the list of values its placeholders "
                "stand for"
            )
        if as_user is None:
            authority = self._authority
        else:
            authority = "user" if as_user else "admin"
        return _call_db(
            {"op": "sql", "authority": authority, "sql": sql, "params": params}
        )

    def __getattr__(self, name):
        # Only reached for names this handle does not have of its own. A private
        # or dunder name is never a table: answering `copy.copy` or
        # `__deepcopy__` with a query would make every such protocol think this
        # object supports it.
        if name.startswith("_"):
            raise AttributeError(name)
        return Query(name, self._authority)

    def __repr__(self):
        return f"<saltcorn db as {self._authority}>"


#: The handle a code body is given, and the one module code uses. Building it
#: costs nothing and holds nothing: whose run this is, what it may reach and
#: what it has spent all live on the **thread**, which is why one shared handle
#: is safe and why a body cannot reach another run's authority — there is no
#: name for it in the interpreter.
db = Db("admin")
