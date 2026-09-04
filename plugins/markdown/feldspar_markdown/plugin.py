"""What `feldspar-markdown` supplies: two functions over the `markdown` package.

Small on purpose. This is the Python half of the bundled catalog
(`plugins/README.md`), and what it demonstrates is the shape rather than the
subject: a distribution that lives in this repository, ships in the release
tarball with no dependencies beside it, and pulls `markdown` from PyPI at the
moment an admin installs it.

Both functions are reachable everywhere a function is — a calculated field's
formula, a view's expression, a Python or JavaScript code body — which is what
makes "render this column as Markdown" a formula rather than a trigger.
"""

import re

import markdown as _markdown
import saltcorn as sc

#: One converter, reused. `markdown.Markdown` compiles its extensions when it is
#: built, and building one per row of a table is the difference between a
#: rendered column and a slow one. `reset()` before each conversion because the
#: instance carries per-document state (footnotes, references).
_converter = _markdown.Markdown(extensions=["extra", "sane_lists"])

#: Tags left after a conversion, for the plain-text function. Matching HTML with
#: a regular expression is wrong in general and right here: the input is what
#: this module's own converter just produced, not arbitrary markup off the web.
_TAGS = re.compile(r"<[^>]+>")
_SPACES = re.compile(r"\s+")


@sc.function(description="Render Markdown as HTML")
def markdown_to_html(text: str) -> str:
    """The Markdown in `text` as an HTML fragment.

    Nothing is escaped or sanitised beyond what `markdown` does itself: the
    input is a column of this application's own database, and a renderer that
    quietly dropped half of it would be worse than one that renders what it was
    given. Do not point it at text a visitor typed and then render the result
    unescaped.
    """
    if not text:
        return ""
    _converter.reset()
    return _converter.convert(str(text))


@sc.function(description="Markdown as plain text, cut to a length")
def markdown_to_text(text: str, length: int = 0) -> str:
    """`text` with its markup removed, collapsed to single spaces.

    What a list view wants where a column holds a paragraph of Markdown: the
    words, on one line, ending in an ellipsis when `length` cuts them off. A
    `length` of 0 — the default — cuts nothing.
    """
    if not text:
        return ""
    _converter.reset()
    plain = _SPACES.sub(" ", _TAGS.sub(" ", _converter.convert(str(text)))).strip()
    limit = int(length or 0)
    if limit <= 0 or len(plain) <= limit:
        return plain
    # Cut at the last space before the limit, so the result ends in a word.
    cut = plain[:limit].rsplit(" ", 1)[0] or plain[:limit]
    return f"{cut}…"
