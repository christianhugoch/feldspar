#!/usr/bin/env python3
"""Write saltcorn-v1-BooksDB-library.zip from saltcorn-v1-BooksDB.zip.

    python3 crates/sc-server/tests/fixtures/make-v1-library-fixture.py

The output is the real BooksDB backup with three things added to its pack.json,
each in the shape Saltcorn 1.7.0-alpha.1 writes and reads it (see README.md
beside this script for the upstream source of each shape):

- two library entries, the first placing the second (a nested item), both with
  slots; like every v1 pack entry they carry no `id`;
- a `library` segment at the top of *Show Books*' layout, placing the first
  entry by the serial v1's restore gives it (1), with a field slot;
- a page, *Featured book*, whose layout places the second entry (serial 2) with
  a content slot and embeds *Show Books* with a **legacy** fixed state: the
  `view` segment has no `configuration`, and the state is in the page's
  `fixed_states` under the segment's name.

Every other entry is copied byte for byte. Timestamps are fixed, so running the
script again writes the same file.
"""

import json
import pathlib
import zipfile

HERE = pathlib.Path(__file__).resolve().parent
SOURCE = HERE / "saltcorn-v1-BooksDB.zip"
TARGET = HERE / "saltcorn-v1-BooksDB-library.zip"
# The time the BooksDB backup was taken (backup-info.json's backup_date).
STAMP = (2026, 9, 12, 17, 50, 42)


def blank(contents, text_style=""):
    return {"type": "blank", "block": False, "contents": contents, "textStyle": text_style}


LIBRARY = [
    {
        "name": "Book header",
        "icon": "fas fa-book",
        "layout": {
            "above": [
                blank("Book", "h3"),
                {"type": "library-slot", "name": "title"},
                {
                    "type": "library",
                    "library_id": 2,
                    "slots": [
                        {
                            "name": "note",
                            "kind": "content",
                            "contents": blank("From the BooksDB library"),
                        }
                    ],
                },
            ]
        },
    },
    {
        "name": "Book note",
        "icon": "fas fa-sticky-note",
        "layout": {"above": [blank("Note:"), {"type": "library-slot", "name": "note"}]},
    },
]

SHOW_BOOKS_HEADER = {
    "type": "library",
    "library_id": 1,
    "slots": [{"name": "title", "kind": "field", "field": "title", "fieldview": "as_text"}],
}

FEATURED_PAGE = {
    "name": "Featured book",
    "title": "Featured",
    "description": "",
    "min_role": 1,
    "layout": {
        "above": [
            {
                "type": "library",
                "library_id": 2,
                "slots": [
                    {"name": "note", "kind": "content", "contents": blank("Featured this week")}
                ],
            },
            {
                "name": "f3a7c1",
                "type": "view",
                "view": "Show Books",
                "state": "fixed",
                "relation": ".",
            },
        ]
    },
    "fixed_states": {"f3a7c1": {"id": 2}},
    "attributes": {"no_menu": False, "request_fluid_layout": False},
    "root_page_for_roles": [],
}


def main():
    with zipfile.ZipFile(SOURCE) as source:
        pack = json.loads(source.read("pack.json"))
        assert pack["library"] == [], "the BooksDB pack already has a library"
        pack["library"] = LIBRARY
        show = next(v for v in pack["views"] if v["name"] == "Show Books")
        show["configuration"]["layout"]["above"].insert(0, SHOW_BOOKS_HEADER)
        pack["pages"].append(FEATURED_PAGE)

        with zipfile.ZipFile(TARGET, "w", zipfile.ZIP_DEFLATED) as target:
            for info in source.infolist():
                data = source.read(info.filename)
                if info.filename == "pack.json":
                    data = json.dumps(pack, indent=2).encode()
                out = zipfile.ZipInfo(info.filename, date_time=STAMP)
                out.compress_type = zipfile.ZIP_DEFLATED
                out.external_attr = 0o644 << 16
                target.writestr(out, data)
    print(f"wrote {TARGET.name}")


if __name__ == "__main__":
    main()
