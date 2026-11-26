import re
import sys
import tempfile
import unicodedata
from pathlib import Path

KEPT = {"SKILL.md", "asks.txt", "where.txt", "names.txt", "LICENSE"}
FILLERS = {0x034F, 0x115F, 0x1160, 0x17B4, 0x17B5, 0x2800, 0x3164, 0xFFA0}


def hidden(char):
    code = ord(char)
    if char in "\n\t":
        return False
    variation = 0xFE00 <= code <= 0xFE0F or 0xE0100 <= code <= 0xE01EF
    return variation or code in FILLERS or unicodedata.category(char) in ("Cc", "Cf", "Co", "Cs", "Cn", "Zl", "Zp")


def mixed(word):
    scripts = {unicodedata.name(char, "").split(" ")[0] for char in word if char.isalpha()}
    return "LATIN" in scripts and scripts & {"CYRILLIC", "GREEK", "ARMENIAN", "CHEROKEE", "FULLWIDTH", "MATHEMATICAL"}


def problems(folder):
    if folder.is_symlink() or not folder.is_dir():
        return ["not a folder"]
    found = []
    for path in sorted(folder.rglob("*")):
        if path.is_symlink() or not path.is_file() or path.name not in KEPT or path.parent != folder:
            found.append(f"{path.name}: only {', '.join(sorted(KEPT))}, as plain files")
            continue
        try:
            text = path.read_bytes().decode()
        except UnicodeDecodeError:
            found.append(f"{path.name}: not UTF-8")
            continue
        for number, line in enumerate(text.split("\n"), 1):
            codes = {f"U+{ord(char):04X}" for char in line if hidden(char)}
            if codes:
                found.append(f"{path.name}:{number}: hidden {' '.join(sorted(codes))}")
            if any(mixed(word) for word in re.findall(r"\w+", line)):
                found.append(f"{path.name}:{number}: one word in two alphabets")
            if "<!--" in line:
                found.append(f"{path.name}:{number}: an HTML comment")
    skill = folder / "SKILL.md"
    if not skill.is_file():
        return found + ["no SKILL.md"]
    text = skill.read_bytes().decode(errors="replace")
    if not re.match(rf"---\nname: {re.escape(folder.name)}\n", text):
        found.append("SKILL.md: the first line of the front matter is not the folder's name")
    if len(text) > 8000 or text.count("\n") > 150:
        found.append(f"SKILL.md: {len(text)} characters, {text.count(chr(10))} lines; at most 8000 and 150")
    return found


def refused(name, change):
    with tempfile.TemporaryDirectory() as scratch:
        folder = Path(scratch) / "probe"
        folder.mkdir()
        (folder / "SKILL.md").write_text("---\nname: probe\ndescription: d\n---\nBody\n")
        assert problems(folder) == [], problems(folder)
        change(folder)
        if not problems(folder):
            sys.exit(f"FAIL {name} is not caught")


def add(text):
    def change(folder):
        with open(folder / "SKILL.md", "a", newline="") as file:
            file.write(text)
    return change


refused("a zero-width character", add("a​b\n"))
refused("a tag character", add("a\U000e0041\n"))
refused("a bidi override", add("a‮b\n"))
refused("a variation selector", add("a️b\n"))
refused("a carriage return", add("a\rb\n"))
refused("a lookalike letter", add("pаssword\n"))
refused("an HTML comment", add("<!-- x -->\n"))
refused("a long skill", add("x" * 8000 + "\n"))
refused("another name", lambda folder: (folder / "SKILL.md").write_text("---\nname: other\n---\n"))
refused("a script", lambda folder: (folder / "run.sh").write_text("true\n"))
refused("a link", lambda folder: (folder / "asks.txt").symlink_to("/etc/hostname"))
refused("a hidden ask", lambda folder: (folder / "asks.txt").write_text("a⁠b\n"))

root = Path(sys.argv[1])
bad = [f"{folder.name}: {problem}" for folder in sorted(root.iterdir()) for problem in problems(folder)]
print("\n".join(bad))
if bad:
    sys.exit(f"FAIL {len(bad)} problems")
print(f"PASS {len(list(root.iterdir()))} skills hold plain text only")
