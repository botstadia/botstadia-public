#!/usr/bin/env python3
"""Export a private BotStadia repository's public part into this repository.

    export.py --source <private repo> --ref <commit> --list <list file> \
        --dest <this repository's checkout> --dir <subdirectory> [--commit]

The list file is the boundary between private and public, kept explicit
(TRUST.md#approving-builds). Only files tracked at the commit and matched by
it are copied, into <dir>/, which is emptied first:

    include <path>          a file, or a directory and everything under it
    exclude <path>          left out, even under an included directory
    root <path>             copied from the source's <path>/ to this
                            repository's root instead of under <dir>/
    workspace <path>        a member of the exported Cargo workspace (the
                            source's root Cargo.toml, its members replaced)
    drop-crate <name>       a private crate: every dependency on it, and every
                            feature naming it, is removed from the manifests
    config <KEY>            copied, resolved, from the source's config.yml into
                            <dir>/config.yml: public addresses and ids the
                            public source is checked against
    submodule <path>        a git submodule kept at the source's commit
    allow <path> <regex>    a scan hit in that file that is known and public

Every copied file is scanned for what must never be public (local paths,
private repositories, other projects, keys); a hit not allowed fails the
export. The exported Cargo.lock is the source's, pruned by Cargo to the
exported workspace, and must pin every package exactly as the source does.

With --commit, the result is committed (author and committer dates set to the
source commit's, so `git log -1 --format=%ct` of the tag gives the build's
SOURCE_DATE_EPOCH), unless nothing changed. It prints the commit.
"""

import argparse
import fnmatch
import os
import re
import shutil
import subprocess
import sys

FORBIDDEN = [
    r"/Users/",
    r"/home/(?!\.cargo|runner)[a-z]",
    r"REPOS/",
    r"DOCUMENTS/",
    r"AGENT-TEMP",
    r"(?i)lockstep",
    r"(?i)shonin",
    r"(?i)\byael\b",
    r"(?i)dakom",
    r"(?i)david",
    r"-----BEGIN [A-Z ]*PRIVATE KEY-----",
    r"(?i)(api|secret|private)[_-]?key\s*[:=]\s*['\"]?[A-Za-z0-9_\-]{24,}",
]
SECRET_NAMES = re.compile(r"(^|/)\.env(\..*)?$|(^|/)\.dev\.vars|\.key$|\.pem$")


def git(source, *args, binary=False):
    out = subprocess.run(["git", "-C", source, *args], check=True, capture_output=True).stdout
    return out if binary else out.decode()


def parse_list(path, text):
    rules = {k: [] for k in ["include", "exclude", "root", "workspace", "drop-crate", "config", "submodule", "allow"]}
    for n, line in enumerate(text.splitlines(), 1):
        line = line.split("#", 1)[0].strip()
        if not line:
            continue
        verb, _, rest = line.partition(" ")
        if verb not in rules or not rest:
            sys.exit(f"{path}:{n}: unknown line: {line}")
        rules[verb].append(rest.strip())
    return rules


def under(path, prefixes):
    return any(path == p or path.startswith(p.rstrip("/") + "/") or fnmatch.fnmatch(path, p) for p in prefixes)


def drop_crates(text, crates, dropped_features):
    """Remove dependency lines naming a dropped crate, features that need one,
    and mentions of those features. Manifests here keep each dependency and
    each feature on one line."""
    lines = []
    for line in text.splitlines():
        m = re.match(r"\s*([A-Za-z0-9_-]+)\s*=", line)
        name = m.group(1) if m else None
        if name in crates:
            continue
        if m and "[" in line and any(f'"dep:{c}"' in line or f'"{c}/' in line for c in crates):
            dropped_features.add(name)
            continue
        lines.append(line)
    out = []
    for line in lines:
        for f in dropped_features:
            line = re.sub(rf'\s*"{re.escape(f)}"\s*,?', "", line)
        line = re.sub(r",\s*\]", "]", line)
        out.append(line)
    return "\n".join(out) + "\n"


def workspace_manifest(text, members, crates, dropped_features):
    start = text.index("members = [")
    end = text.index("]", start)
    body = "members = [\n" + "".join(f'    "{m}",\n' for m in members) + "]"
    text = text[:start] + body + text[end + 1 :]
    if dropped_features:
        # Code behind a dropped feature stays in the source, never compiled.
        values = ", ".join(f'\\"{f}\\"' for f in sorted(dropped_features))
        if "check-cfg = [" not in text:
            sys.exit("the workspace has no check-cfg list to name dropped features in")
        text = text.replace("check-cfg = [", f'check-cfg = ["cfg(feature, values({values}))", ', 1)
    return drop_crates(text, crates, set())


def config_subset(source, commit, keys):
    """Each key's entry exactly as the commit's config.yml has it (one line,
    or a folded `>-` block), so what reads one reads the other. A `{{.KEY}}`
    in a value must name another exported key."""
    lines = git(source, "show", f"{commit}:config.yml").splitlines()
    blocks = {}
    for i, line in enumerate(lines):
        m = re.match(r"  ([A-Z0-9_]+):(.*)$", line)
        if m and m.group(1) in keys:
            block = [line]
            if m.group(2).strip() in (">-", ">", "|", "|-"):
                block += [l for l in lines[i + 1 :] if l.startswith("    ")][: next(
                    (j for j, l in enumerate(lines[i + 1 :]) if not l.startswith("    ")), len(lines)
                )]
            blocks[m.group(1)] = "\n".join(block)
    missing = [k for k in keys if k not in blocks]
    if missing:
        sys.exit(f"config keys not in config.yml: {missing}")
    for k, v in blocks.items():
        for ref in re.findall(r"\{\{\.([A-Z0-9_]+)\}\}", v):
            if ref not in blocks:
                sys.exit(f"config key {k} names {ref}, which isn't exported")
    head = (
        "# Public values the public source is built and checked against: chain ids,\n"
        "# contract addresses, trust anchors. Exported from BotStadia's private\n"
        "# config.yml, entry by entry; `{{.KEY}}` names another entry here.\n"
    )
    return head + "vars:\n" + "".join(blocks[k] + "\n" for k in keys)


def lock_packages(text):
    pkgs = {}
    for block in text.split("[[package]]")[1:]:
        name = re.search(r'^name = "(.*)"', block, re.M).group(1)
        version = re.search(r'^version = "(.*)"', block, re.M).group(1)
        source = re.search(r'^source = "(.*)"', block, re.M)
        checksum = re.search(r'^checksum = "(.*)"', block, re.M)
        pkgs.setdefault((name, version), (source and source.group(1), checksum and checksum.group(1)))
    return pkgs


def scan(dest, written, rules):
    """Scan the files this export wrote (paths relative to dest)."""
    allows = [(p, re.compile(r)) for p, _, r in (a.partition(" ") for a in rules["allow"])]
    hits = []
    for rel in sorted(written):
        full = os.path.join(dest, rel)
        if SECRET_NAMES.search(rel) and not rel.endswith(".env.example"):
            hits.append(f"{rel}: a secret's file name")
            continue
        try:
            text = open(full, encoding="utf-8").read()
        except UnicodeDecodeError:
            continue
        for n, line in enumerate(text.splitlines(), 1):
            for pattern in FORBIDDEN:
                if re.search(pattern, line) and not any(under(rel, [p]) and r.search(line) for p, r in allows):
                    hits.append(f"{rel}:{n}: {pattern}: {line.strip()[:120]}")
    return hits


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--source", required=True)
    ap.add_argument("--ref", required=True)
    ap.add_argument("--list", required=True)
    ap.add_argument("--dest", required=True)
    ap.add_argument("--dir", required=True)
    ap.add_argument("--commit", action="store_true")
    args = ap.parse_args()
    commit = git(args.source, "rev-parse", "--verify", f"{args.ref}^{{commit}}").strip()
    # The list, like everything else, as the commit has it.
    rules = parse_list(args.list, git(args.source, "show", f"{commit}:{args.list}"))
    if os.path.realpath(args.dest) == os.path.realpath(args.source):
        sys.exit("--dest is the source repository")
    tracked = git(args.source, "ls-tree", "-r", "--full-tree", commit).splitlines()

    out_dir = os.path.normpath(os.path.join(args.dest, args.dir))
    if args.dir in (".", ""):
        # The whole repository is this export's: empty it but for .git.
        for entry in os.listdir(args.dest):
            if entry != ".git":
                path = os.path.join(args.dest, entry)
                shutil.rmtree(path) if os.path.isdir(path) and not os.path.islink(path) else os.remove(path)
    elif os.path.exists(out_dir):
        shutil.rmtree(out_dir)
    os.makedirs(out_dir, exist_ok=True)
    root_prefixes = [r.rstrip("/") + "/" for r in rules["root"]]

    copied = 0
    written = set()
    gitlinks = {}
    dropped_features = set()
    for entry in tracked:
        meta, path = entry.split("\t", 1)
        mode, kind, sha = meta.split()
        if kind == "commit":
            if under(path, rules["submodule"]):
                gitlinks[path] = sha
            continue
        if path in ("Cargo.toml", "Cargo.lock", "config.yml"):
            continue
        root_hit = next((p for p in root_prefixes if path.startswith(p)), None)
        if root_hit:
            target = os.path.join(args.dest, path[len(root_hit) :])
        elif under(path, rules["include"]) and not under(path, rules["exclude"]):
            target = os.path.join(out_dir, path)
        else:
            continue
        os.makedirs(os.path.dirname(target), exist_ok=True)
        data = git(args.source, "cat-file", "blob", sha, binary=True)
        if path.endswith("Cargo.toml") and rules["drop-crate"]:
            # A dropped feature is this manifest's own: other crates may use
            # the same name for something else.
            own = set()
            data = drop_crates(data.decode(), set(rules["drop-crate"]), own).encode()
            dropped_features |= own
        with open(target, "wb") as f:
            f.write(data)
        os.chmod(target, 0o755 if mode == "100755" else 0o644)
        written.add(os.path.relpath(target, args.dest))
        copied += 1
    # Root files this list owns: one dropped from it since the last export is
    # removed, so nothing stale stays public.
    if root_prefixes and args.dir not in (".", ""):
        record = os.path.join(args.dest, f".exported-root-{os.path.basename(os.path.normpath(out_dir))}")
        now = sorted(w for w in written if not w.startswith(args.dir.rstrip("/") + "/"))
        if os.path.exists(record):
            for old in open(record).read().split():
                if old not in now and os.path.exists(os.path.join(args.dest, old)):
                    os.remove(os.path.join(args.dest, old))
        with open(record, "w") as f:
            f.write("\n".join(now) + "\n")
    for path in rules["submodule"]:
        if path not in gitlinks:
            sys.exit(f"submodule {path} isn't a gitlink at {commit}")

    if rules["workspace"]:
        manifest = git(args.source, "show", f"{commit}:Cargo.toml")
        with open(os.path.join(out_dir, "Cargo.toml"), "w") as f:
            f.write(workspace_manifest(manifest, rules["workspace"], set(rules["drop-crate"]), dropped_features))
        source_lock = git(args.source, "show", f"{commit}:Cargo.lock")
        with open(os.path.join(out_dir, "Cargo.lock"), "w") as f:
            f.write(source_lock)
        written.update(os.path.relpath(os.path.join(out_dir, f), args.dest) for f in ("Cargo.toml", "Cargo.lock"))
        subprocess.run(
            ["cargo", "metadata", "--offline", "--format-version", "1", "--manifest-path", os.path.join(out_dir, "Cargo.toml")],
            check=True,
            capture_output=True,
        )
        before, after = lock_packages(source_lock), lock_packages(open(os.path.join(out_dir, "Cargo.lock")).read())
        moved = [k for k in after if k not in before or (before[k] != after[k] and after[k][0])]
        if moved:
            sys.exit(f"the exported Cargo.lock pins packages the source's doesn't: {moved}")
    if rules["config"]:
        with open(os.path.join(out_dir, "config.yml"), "w") as f:
            f.write(config_subset(args.source, commit, rules["config"]))
        written.add(os.path.relpath(os.path.join(out_dir, "config.yml"), args.dest))

    hits = scan(args.dest, written, rules)
    if hits:
        sys.exit("✗ the export carries what must stay private:\n  " + "\n  ".join(hits))
    print(f"exported {copied} files from {commit[:12]} into {args.dir}/", file=sys.stderr)

    if not args.commit:
        return

    def sub(path):
        return os.path.normpath(os.path.join(args.dir, path))

    if gitlinks:
        modules = "".join(
            f'[submodule "{sub(p)}"]\n\tpath = {sub(p)}\n\turl = {url}\n'
            for p, url in submodule_urls(args.source, commit, gitlinks).items()
        )
        others = strip_modules(args.dest, args.dir)
        with open(os.path.join(args.dest, ".gitmodules"), "w") as f:
            f.write(others + modules)
    subprocess.run(["git", "-C", args.dest, "add", "-A"], check=True)
    for path, sha in gitlinks.items():
        subprocess.run(
            ["git", "-C", args.dest, "update-index", "--add", "--cacheinfo", f"160000,{sha},{sub(path)}"],
            check=True,
        )
    staged = subprocess.run(["git", "-C", args.dest, "diff", "--cached", "--quiet"]).returncode
    head = subprocess.run(["git", "-C", args.dest, "rev-parse", "--verify", "-q", "HEAD"], capture_output=True)
    if staged == 0 and head.returncode == 0:
        print(head.stdout.decode().strip())
        print("unchanged: the export is the last commit's", file=sys.stderr)
        return
    when = git(args.source, "log", "-1", "--format=%cI", commit).strip()
    env = dict(
        os.environ,
        GIT_AUTHOR_NAME="BotStadia release",
        GIT_AUTHOR_EMAIL="release@botstadia.com",
        GIT_COMMITTER_NAME="BotStadia release",
        GIT_COMMITTER_EMAIL="release@botstadia.com",
        GIT_AUTHOR_DATE=when,
        GIT_COMMITTER_DATE=when,
    )
    message = f"{args.dir if args.dir not in ('.', '') else 'export'}: export of {os.path.basename(os.path.abspath(args.source)).lower()} {commit[:12]}"
    subprocess.run(["git", "-C", args.dest, "commit", "-q", "-m", message], check=True, env=env)
    print(git(args.dest, "rev-parse", "HEAD").strip())


def submodule_urls(source, commit, gitlinks):
    text = git(source, "show", f"{commit}:.gitmodules")
    urls = {}
    for path in gitlinks:
        m = re.search(rf"path = {re.escape(path)}\n\s*url = (\S+)", text)
        if not m:
            sys.exit(f"no url for submodule {path} in .gitmodules")
        urls[path] = m.group(1)
    return urls


def strip_modules(dest, subdir):
    path = os.path.join(dest, ".gitmodules")
    if not os.path.exists(path):
        return ""
    blocks = re.split(r"(?=\[submodule )", open(path).read())
    return "".join(b for b in blocks if b.strip() and f'"{subdir}/' not in b)


if __name__ == "__main__":
    main()
