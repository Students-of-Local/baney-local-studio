import bisect
import hashlib
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys

FLOORS = {0: (130, 155, 161), 1000: (94, 123, 138)}
CUTS = (1, 3, 5)


def at(*key):
    return int.from_bytes(hashlib.sha256("\0".join(map(str, key)).encode()).digest()[:8], "big")


def crowd(skills, asks, count):
    words = {word for _, ask in asks for word in re.findall(r"[a-z]{4,}", ask.lower())}
    ring = sorted((at(word), word) for word in words)

    def line(*key):
        picks = (bisect.bisect(ring, (at(*key, k), "")) % len(ring) for k in range(10))
        return " ".join(ring[pick][1] for pick in picks)

    for n in range(count):
        folder = skills / f"made-up-{n}"
        folder.mkdir()
        parts = "\n\n".join(f"## Part {p}\n\n" + "\n".join(line(n, p, k) for k in range(4)) for p in range(3))
        (folder / "SKILL.md").write_text(f"---\nname: made-up-{n}\ndescription: {line(n)}\n---\n\n{parts}\n")


def recall(binary, shipped, asks, scratch, count):
    shutil.rmtree(scratch, ignore_errors=True)
    skills = scratch / "skills"
    shutil.copytree(shipped, skills)
    crowd(skills, asks, count)
    (scratch / "home").mkdir()
    env = {
        "HOME": str(scratch / "home"),
        "LOCAL_STUDIO_DATA_DIR": str(scratch / "data"),
        "LOCAL_STUDIO_SKILLS_DIR": str(skills),
        "PATH": os.environ["PATH"],
    }
    found = []
    for name, words in asks:
        args = [binary, "skills", "search", "--shipped", "--json", "--", *words.split()]
        out = subprocess.run(args, capture_output=True, text=True, env=env, cwd=scratch)
        if out.returncode:
            sys.exit(f"FAIL search {words!r}: {out.stdout}{out.stderr}")
        names = [hit["name"] for hit in json.loads(out.stdout)]
        found.append(names.index(name) if name in names else 99)
    return [sum(rank < cut for rank in found) for cut in CUTS]


def main():
    binary, shipped, tsv, scratch = sys.argv[1], *map(pathlib.Path, sys.argv[2:5])
    asks = [line.split("\t") for line in tsv.read_text().splitlines()]
    asked, there = {name for name, _ in asks}, {path.parent.name for path in shipped.glob("*/SKILL.md")}
    failed = asked != there
    for label, names in (("no such skill", asked - there), ("no asks for", there - asked)):
        if names:
            print(f"FAIL {label}: {', '.join(sorted(names))}")
    for count, floors in FLOORS.items():
        got = recall(binary, shipped, asks, scratch / f"crowd-{count}", count)
        for cut, hits, floor in zip(CUTS, got, floors):
            failed |= hits != floor
            word = "PASS" if hits == floor else "FAIL" if hits < floor else "FAIL raise the floor:"
            print(f"{word} top {cut} with {count} other skills: {hits} of {len(asks)}, floor {floor}")
    sys.exit(failed)


main()
