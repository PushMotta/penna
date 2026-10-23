"""The fan chips each motherboard driver knows, for docs/SUPPORT.md: read from the kernel's own documentation and the it87
fork's README, and written to data/chips.toml with where each list came from. Run it again against a newer kernel tree.

    python3 -I tools/support/chips.py KERNEL_TREE IT87_FORK_README FORK_COMMIT > data/chips.toml

KERNEL_TREE needs Documentation/hwmon/{it87,nct6775,nct6683,f71882fg,w83627ehf}.rst (a sparse checkout is enough).
"""
import pathlib
import re
import subprocess
import sys

DRIVERS = ["it87", "nct6775", "nct6683", "f71882fg", "w83627ehf"]


def kernel_chips(tree: pathlib.Path, driver: str) -> list[str]:
    """The bullets under "Supported chips:" in Documentation/hwmon/<driver>.rst."""
    text = (tree / "Documentation/hwmon" / f"{driver}.rst").read_text()
    after = text.split("Supported chips:", 1)[1]
    chips = []
    for line in after.splitlines():
        if re.match(r"^\S", line) and chips:
            break  # the next section: "Authors:", "Description", …
        m = re.match(r"^\s{2,}\*\s+(.+?)\s*$", line)
        if m:
            chips.append(re.sub(r"\s+", " ", m.group(1)))
    return chips


def fork_chips(readme: pathlib.Path) -> list[str]:
    """The "Supports:" block of the it87 fork's README: one chip per line, its name first."""
    lines = readme.read_text().splitlines()
    start = next(i for i, l in enumerate(lines) if "Supports:" in l)
    chips = []
    for line in lines[start:]:
        m = re.match(r"^\s*\*\s+(?:Supports:)?\s*(\S+)\s+(?:Super I/O chip|A clone)", line)
        if not m:
            if chips:
                break
            continue
        chips.append(m.group(1))
    return chips


def toml_list(items: list[str]) -> str:
    return "[" + ", ".join('"' + i.replace("\\", "\\\\").replace('"', '\\"') + '"' for i in items) + "]"


def main() -> None:
    tree, readme, fork_commit = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2]), sys.argv[3]
    commit = subprocess.run(["git", "-C", str(tree), "log", "-1", "--format=%h"], capture_output=True, text=True).stdout.strip()
    version = "7.4"
    print("# The fan chips each motherboard driver knows (docs/SUPPORT.md). Written by tools/support/chips.py, not by hand.")
    for driver in DRIVERS:
        print()
        print("[[driver]]")
        print(f'name = "{driver}"')
        print(f'source = "Linux {version} ({commit}), Documentation/hwmon/{driver}.rst"')
        print(f"chips = {toml_list(kernel_chips(tree, driver))}")
    print()
    print("[[driver]]")
    print('name = "it87-dkms-git"')
    print(f'source = "github.com/frankcrawford/it87 at {fork_commit}, README"')
    print(f"chips = {toml_list(fork_chips(readme))}")


main()
