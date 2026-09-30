#!/usr/bin/env python3
"""Render the same synthetic store independently, then compare untouched bytes."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

from validate_fixture import validate

LIQUID_SHA = "4e39ae4cc3da73921923c0669e0fc84a66b2f696"
ROOT = Path(__file__).resolve().parents[2]


def run(command, **kwargs):
    return subprocess.run(command, check=True, text=True, **kwargs)


def pin(path, expected):
    actual = subprocess.check_output(["git", "-C", str(path), "rev-parse", "HEAD"], text=True).strip()
    dirty = subprocess.check_output(["git", "-C", str(path), "status", "--porcelain", "--untracked-files=all"], text=True)
    if actual != expected or dirty:
        raise ValueError(f"reference must be clean and pinned at {expected}: {path}")


def fingerprint(path):
    content = path.read_bytes()
    return {"bytes": len(content), "sha256": hashlib.sha256(content).hexdigest()}


def compare(left, right, label):
    a, b = left.read_bytes(), right.read_bytes()
    if a == b:
        return fingerprint(left)
    offset = next((i for i, (x, y) in enumerate(zip(a, b)) if x != y), min(len(a), len(b)))
    raise ValueError(f"{label} differs at byte {offset}: Ruby {len(a)} bytes, Rust {len(b)} bytes; "
                     f"Ruby={a[max(0, offset-60):offset+100]!r}; Rust={b[max(0, offset-60):offset+100]!r}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--liquid-root", required=True, type=Path)
    parser.add_argument("--theme-root", required=True, type=Path)
    parser.add_argument("--fixture", type=Path, default=Path(__file__).with_name("store.json"))
    parser.add_argument("--scope", choices=["hero", "template", "page"], default="page")
    parser.add_argument("--output-dir", required=True, type=Path)
    args = parser.parse_args()
    args.output_dir = args.output_dir.resolve()
    for source in (ROOT, args.liquid_root.resolve(), args.theme_root.resolve()):
        if args.output_dir == source or source in args.output_dir.parents:
            raise ValueError("generated theme artifacts must be outside source checkouts")
    args.output_dir.mkdir(parents=True, exist_ok=True)
    (args.output_dir / "comparison.json").unlink(missing_ok=True)
    validate(args.fixture)
    fixture = json.loads(args.fixture.read_text())
    pin(args.theme_root, fixture["theme"]["sha"])
    pin(args.liquid_root, LIQUID_SHA)
    env = os.environ.copy()
    env["BUNDLE_GEMFILE"] = str(args.liquid_root.resolve() / "Gemfile")
    args.output_dir.mkdir(parents=True, exist_ok=True)
    common = ["--theme-root", str(args.theme_root.resolve()), "--fixture", str(args.fixture.resolve()), "--scope", args.scope]
    commands = {
        "ruby": ["bundle", "exec", "ruby", str(ROOT / "compat/horizon/ruby/render.rb"), "--liquid-root", str(args.liquid_root.resolve()), *common],
        "rust": ["cargo", "run", "--quiet", "--locked", "--offline", "--example", "horizon", "--", *common],
    }
    log = {"scope": args.scope, "synthetic": True, "fixture": fingerprint(args.fixture), "theme_sha": fixture["theme"]["sha"], "ruby_liquid_sha": LIQUID_SHA}
    for engine, command in commands.items():
        for suffix in (engine, engine + "-repeat"):
            output = args.output_dir / suffix
            if output.is_symlink():
                raise ValueError(f"output engine directory must not be a symlink: {output}")
            output.mkdir(parents=True, exist_ok=True)
            for name in ("index.html", "styles.css", "report.json"):
                (output / name).unlink(missing_ok=True)
            log_path = args.output_dir / (suffix + ".log")
            if log_path.is_symlink():
                raise ValueError(f"output log must not be a symlink: {log_path}")
            with log_path.open("w") as stream:
                run([*command, "--output-dir", str(output)], cwd=ROOT, env=env, stdout=stream, stderr=subprocess.STDOUT)
        for name in ("index.html", "styles.css"):
            compare(args.output_dir / engine / name, args.output_dir / (engine + "-repeat") / name, engine + " repeat " + name)
    log["artifacts"] = {name: compare(args.output_dir / "ruby" / name, args.output_dir / "rust" / name, name) for name in ("index.html", "styles.css")}
    # Assets are shared originals. Theme assets remain linked to the external
    # pinned checkout rather than being redistributed with this harness.
    for engine in commands:
        output = args.output_dir / engine
        assets = output / "assets"
        if assets.is_symlink():
            assets.unlink()
        elif assets.exists():
            raise ValueError(f"refusing to replace a real assets path: {assets}")
        assets.symlink_to(args.theme_root.resolve() / "assets", target_is_directory=True)
        cdn = output / "cdn"
        if cdn.is_symlink() or any(path.is_symlink() for path in cdn.rglob("*")):
            raise ValueError(f"mock asset output must not contain symlinks: {cdn}")
        shutil.copytree(ROOT / "compat/horizon/mock-assets/cdn", cdn, dirs_exist_ok=True)
    if fingerprint(args.fixture) != log["fixture"]:
        raise ValueError("fixture changed during comparison")
    pin(args.theme_root, fixture["theme"]["sha"])
    pin(args.liquid_root, LIQUID_SHA)
    log["core_head"] = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    log["core_dirty"] = bool(subprocess.check_output(["git", "status", "--porcelain", "--untracked-files=all"], cwd=ROOT, text=True))
    (args.output_dir / "comparison.json").write_text(json.dumps(log, indent=2) + "\n")
    print(json.dumps(log, indent=2))


if __name__ == "__main__":
    try:
        main()
    except (ValueError, subprocess.CalledProcessError) as error:
        sys.exit(str(error))
