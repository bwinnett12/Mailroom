#!/usr/bin/env python3
"""
import_recipes.py — send a recipes/ + ingredients/ markdown repo to Mailroom.

    python3 import_recipes.py ~/path/to/repo                    # dry run: print what would be sent
    python3 import_recipes.py ~/path/to/repo --send             # POST to http://127.0.0.1:3000
    python3 import_recipes.py ~/path/to/repo --send --url http://127.0.0.1:3000

Standard library only.

What becomes what
-----------------
  recipes/<name>.md       envelope  data_type "recipe",     key "recipe/<slug>"
  ingredients/<name>.md   envelope  data_type "ingredient", key "ingredient/<slug>"

  [[Link]] inside a recipe
    → USES      ingredient/<slug>     (any link not starting with "__")
    → LISTED_IN index/<slug>          (links starting with "__", e.g. [[__Recipes]]:
                                       index pages, kept apart so they don't
                                       become a hub every recipe connects through)
  [[Link]] inside an ingredient
    → RELATED   ingredient/<slug>

  slug: lowercase, spaces and underscores-runs → "-", other punctuation dropped.
  "Chipotle peppers in adobo" and chipotle-peppers-in-adobo.md meet at the same node.

  title: the "# " heading if the file has exactly one; otherwise (none, or
  several "# " headings used as sections, like Juice / Zest / Whole) the
  file name, e.g. lemon.md → "Lemon".

Links to ingredients with no file yet still become nodes; they fill in when
the file is written and the importer runs again.

Running it twice is safe for the graph (same keys, same edges: set semantics),
but every run appends new records to the ledger. Try it on a throwaway vault
first: the ledger has no delete yet (tombstones are phase 5).
"""

import argparse
import json
import re
import sys
import urllib.error
import urllib.request
from pathlib import Path

LINK = re.compile(
    r"\[\[([^\]|#]+)(?:[#|][^\]]*)?\]\]"
)  # [[Target]], [[Target|alias]], [[Target#heading]]


def slug(text: str) -> str:
    s = text.strip().lower().replace("_", " ")
    s = re.sub(r"[^\w\s-]", "", s)
    return re.sub(r"[\s-]+", "-", s).strip("-")


def title_of(text: str, stem: str) -> str:
    tops = [line[2:].strip() for line in text.splitlines() if line.startswith("# ")]
    if len(tops) == 1:
        return tops[0]
    name = stem.replace("-", " ").replace("_", " ").strip()
    return name[:1].upper() + name[1:]


def links_in(text: str, kind: str):
    seen, out = set(), []
    for raw in LINK.findall(text):
        name = raw.strip()
        if name.startswith("__"):
            link = ("LISTED_IN", f"index/{slug(name)}")
        elif kind == "recipe":
            link = ("USES", f"ingredient/{slug(name)}")
        else:
            link = ("RELATED", f"ingredient/{slug(name)}")
        if link not in seen:
            seen.add(link)
            out.append({"label": link[0], "to": link[1]})
    return out


def envelopes(root: Path):
    for folder, kind in (("ingredients", "ingredient"), ("recipes", "recipe")):
        for path in sorted((root / folder).glob("*.md")):
            text = path.read_text(encoding="utf-8")
            yield {
                "source": "manual",
                "data_type": kind,
                "payload": {"kind": "text", "data": text},
                "meta": {
                    "key": f"{kind}/{slug(path.stem)}",
                    "title": title_of(text, path.stem),
                },
                "links": links_in(text, kind),
            }


def send(url: str, env: dict):
    req = urllib.request.Request(
        url.rstrip("/") + "/dispatch",
        data=json.dumps(env).encode(),
        headers={"content-type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return r.status, json.loads(r.read() or b"{}")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode(errors="replace")


def main():
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("root", type=Path, help="repo with recipes/ and ingredients/")
    ap.add_argument(
        "--send", action="store_true", help="actually POST (default: dry run)"
    )
    ap.add_argument("--url", default="http://127.0.0.1:3000")
    a = ap.parse_args()

    if not (a.root / "recipes").is_dir():
        sys.exit(f"{a.root}/recipes not found")
    envs = list(envelopes(a.root))
    failed = 0
    for env in envs:
        key = env["meta"]["key"]
        edges = len(env["links"])
        if not a.send:
            print(
                f"{key:45} {edges:3} links  "
                + ", ".join(f"{l['label']}→{l['to']}" for l in env["links"][:4])
                + (" …" if edges > 4 else "")
            )
            continue
        status, body = send(a.url, env)
        ok = (
            200 <= status < 300
            and isinstance(body, dict)
            and body.get("status") == "Completed"
        )
        failed += not ok
        print(
            f"{'ok  ' if ok else 'FAIL'} {key:45} {edges:3} links"
            + ("" if ok else f"   {status} {body}")
        )
    print(
        f"\n{len(envs)} files, {sum(len(e['links']) for e in envs)} links"
        + ("" if a.send else "  (dry run; add --send)")
    )
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
