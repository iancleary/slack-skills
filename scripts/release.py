#!/usr/bin/env -S uv run --script
# Source: https://github.com/iancleary/release-skills
# Keep this runner unchanged; record its revision and checksum in release.toml [runner_source].
# /// script
# requires-python = ">=3.11"
# ///
"""Execute a checked-in release.toml contract."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import shlex
import subprocess
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Sequence
from urllib.parse import quote


class ReleaseError(RuntimeError):
    """A release contract or command failed."""


@dataclass
class CommandResult:
    command: list[str]
    ok: bool
    exit_status: int | None
    stdout: str
    stderr: str


def command(value: Any, label: str) -> list[str]:
    if not isinstance(value, list) or not value:
        raise ReleaseError(f"{label} must be a non-empty string array")
    if not all(isinstance(part, str) for part in value) or not value[0].strip():
        raise ReleaseError(f"{label} must be a non-empty string array")
    if value[0].startswith("builtin:"):
        raise ReleaseError(f"{label} uses unsupported legacy command {value[0]}")
    return list(value)


def repo_file(repo: Path, value: str | Path, label: str) -> Path:
    candidate = Path(value)
    resolved = candidate.resolve() if candidate.is_absolute() else (repo / candidate).resolve()
    try:
        resolved.relative_to(repo)
    except ValueError as exc:
        raise ReleaseError(f"{label} must stay inside the repository: {value}") from exc
    return resolved


def load_config(repo: Path, config_name: str) -> tuple[Path, dict[str, Any]]:
    config_path = repo_file(repo, config_name, "config path")
    try:
        with config_path.open("rb") as stream:
            config = tomllib.load(stream)
    except OSError as exc:
        raise ReleaseError(f"failed to read {config_path}: {exc}") from exc
    except tomllib.TOMLDecodeError as exc:
        raise ReleaseError(f"failed to parse {config_path}: {exc}") from exc

    release = config.get("release")
    source = config.get("runner_source")
    if source is not None:
        if not isinstance(source, dict) or not all(isinstance(source.get(key), str) for key in ("repository", "revision", "sha256")):
            raise ReleaseError("runner_source requires repository, revision, and sha256 strings")
        if not re.fullmatch(r"[0-9a-f]{40}", source["revision"]):
            raise ReleaseError("runner_source.revision must be a full Git commit")
        if source["sha256"] != hashlib.sha256(Path(__file__).read_bytes()).hexdigest():
            raise ReleaseError("vendored runner checksum mismatch; review and update runner_source")
    checks = config.get("checks", {})
    if not isinstance(release, dict):
        raise ReleaseError("release.toml requires [release]")
    if not isinstance(checks, dict):
        raise ReleaseError("release.toml [checks] must be a table")
    if not isinstance(release.get("name"), str) or not release["name"].strip():
        raise ReleaseError("release.toml requires release.name")
    command(release.get("runner"), "release.runner")
    for key in ("current_version_command", "next_version_command", "validate_version_command"):
        if key in release:
            command(release[key], f"release.{key}")
    if release.get("runner_protocol", "command") not in ("command", "prepared-v1"):
        raise ReleaseError("unsupported release.runner_protocol")
    if "publish" in release and not isinstance(release["publish"], bool):
        raise ReleaseError("release.publish must be a boolean")
    dry_run_args = release.get("dry_run_args", [])
    if not isinstance(dry_run_args, list) or not all(
        isinstance(part, str) and part.strip() for part in dry_run_args
    ):
        raise ReleaseError("release.dry_run_args must be a string array")
    configured_checks = checks.get("commands", [])
    if not isinstance(configured_checks, list):
        raise ReleaseError("checks.commands must be an array of command arrays")
    for index, item in enumerate(configured_checks):
        command(item, f"checks.commands[{index}]")
    return config_path, config


def run_command(repo: Path, argv: Sequence[str]) -> CommandResult:
    try:
        completed = subprocess.run(
            list(argv), cwd=repo, check=False, text=True, capture_output=True
        )
    except OSError as exc:
        raise ReleaseError(f"failed to run {' '.join(argv)}: {exc}") from exc
    return CommandResult(
        command=list(argv),
        ok=completed.returncode == 0,
        exit_status=completed.returncode,
        stdout=completed.stdout[-8000:],
        stderr=completed.stderr[-8000:],
    )


def require_success(result: CommandResult) -> str:
    if not result.ok:
        detail = result.stderr.strip() or result.stdout.strip() or "unknown failure"
        raise ReleaseError(f"command failed ({' '.join(result.command)}): {detail}")
    return result.stdout.strip()


def base_result(
    action: str,
    mode: str,
    repo: Path,
    config_path: Path,
    config: dict[str, Any],
) -> dict[str, Any]:
    release = config["release"]
    return {
        "command": action,
        "mode": mode,
        "config_path": str(config_path),
        "repo_path": str(repo),
        "name": release["name"],
        "version_source": release.get("version_source"),
        "current_version": None,
        "next_version": None,
        "publish": release.get("publish", True),
        "changelog": release.get("changelog"),
        "notes_file": release.get("notes_file"),
        "runner": list(release["runner"]),
        "checks": [],
        "ready": True,
        "checks_verified": False,
        "executed": False,
        "exit_status": None,
        "runner_stdout": None,
        "runner_stderr": None,
    }


def execute_check(repo: Path, config_path: Path, config: dict[str, Any]) -> dict[str, Any]:
    result = base_result("check", "check", repo, config_path, config)
    checks = [
        run_command(repo, command(item, f"checks.commands[{index}]"))
        for index, item in enumerate(config.get("checks", {}).get("commands", []))
    ]
    result["checks"] = [item.__dict__ for item in checks]
    result["ready"] = all(item.ok for item in checks)
    result["checks_verified"] = True
    return result


def optional_query(repo: Path, release: dict[str, Any], key: str) -> str | None:
    value = release.get(key)
    if value is None:
        return None
    return require_success(run_command(repo, command(value, f"release.{key}")))


def execute_plan(repo: Path, config_path: Path, config: dict[str, Any], args=None) -> dict[str, Any]:
    result = base_result("plan", "plan", repo, config_path, config)
    result["current_version"] = optional_query(
        repo, config["release"], "current_version_command"
    )
    result["next_version"] = optional_query(
        repo, config["release"], "next_version_command"
    )
    result["ready"] = None
    result["target_commit"] = git(repo, "rev-parse", "HEAD")
    result["config_sha256"] = hashlib.sha256(config_path.read_bytes()).hexdigest()
    explicit_version = getattr(args, "version", None)
    result["version"] = explicit_version if explicit_version is not None else result["next_version"]
    if getattr(args, "bump", None):
        if not result["current_version"]:
            raise ReleaseError("--bump planning requires current_version_command")
        result["version"] = bump_semver(result["current_version"], args.bump)
    if result["version"] is not None:
        validate_consumer_version(repo, config, result["version"])
    result["tag"] = None
    if (config["release"].get("runner_protocol") == "prepared-v1" and result["version"]
            and any(action in config["release"]["runner"] for action in
                    ("tag-release", "cargo-release", "version-file-release"))):
        runner = config["release"]["runner"]
        prefix = "" if "tag-release" in runner else "v"
        if "--tag-prefix" in runner and runner.index("--tag-prefix") + 1 < len(runner):
            prefix = runner[runner.index("--tag-prefix") + 1]
        result["tag"] = release_tag(repo, result["version"], prefix)
    return result


def validate_consumer_version(repo: Path, config: dict[str, Any], version: str) -> None:
    if not version or version != version.strip() or "\x00" in version:
        raise ReleaseError("version must be non-empty without surrounding whitespace or NUL")
    validator = config["release"].get("validate_version_command")
    if validator:
        require_success(run_command(repo, [*command(validator, "validate_version_command"), version]))


def execute_run(
    repo: Path,
    config_path: Path,
    config: dict[str, Any],
    args: argparse.Namespace,
) -> dict[str, Any]:
    release = config["release"]
    digest = getattr(args, "expected_config", None)
    if digest and hashlib.sha256(config_path.read_bytes()).hexdigest() != digest:
        raise ReleaseError("configuration differs from --expected-config; create a new plan")
    if args.apply and not release.get("publish", True):
        raise ReleaseError("release.publish is false; apply is disabled")
    expected = getattr(args, "expected_head", None)
    if expected and git(repo, "rev-parse", "HEAD") != expected:
        raise ReleaseError("HEAD differs from --expected-head; create a new plan")
    prepared = release.get("runner_protocol") == "prepared-v1"
    if prepared and not expected:
        expected = git(repo, "rev-parse", "HEAD")
    if getattr(args, "resume", False) and not prepared:
        raise ReleaseError("resume requires a prepared-v1 runner")
    if getattr(args, "resume", False) and (not args.version or not getattr(args, "expected_head", None) or args.bump):
        raise ReleaseError("resume requires an exact --version and --expected-head without --bump")
    if args.version is not None:
        validate_consumer_version(repo, config, args.version)
    runner = command(release["runner"], "release.runner")
    if prepared:
        runner.extend(["--contract", str(config_path)])
    if expected and prepared:
        runner.extend(["--expected-head", expected])
    if getattr(args, "resume", False):
        runner.append("--resume")
    if not args.apply:
        dry_run_args = release.get("dry_run_args", [])
        if not dry_run_args:
            raise ReleaseError("release.dry_run_args is required for dry-run mode")
        runner.extend(dry_run_args)
    if args.version:
        runner.extend(["--version", args.version])
    if args.bump:
        runner.extend(["--bump", args.bump])
    if args.notes_file:
        runner.extend(["--notes-file", args.notes_file])
    elif release.get("notes_file"):
        runner.extend(["--notes-file", str(release["notes_file"])])
    if args.not_latest:
        runner.append("--not-latest")

    checks = None if prepared else execute_check(repo, config_path, config)
    if checks is not None and not checks["ready"]:
        checks.update(command="run", mode="apply" if args.apply else "dry_run", executed=False)
        return checks
    if expected and git(repo, "rev-parse", "HEAD") != expected:
        raise ReleaseError("HEAD changed during checks")
    command_result = run_command(repo, runner)
    require_success(command_result)
    result = base_result(
        "run", "apply" if args.apply else "dry_run", repo, config_path, config
    )
    result["runner"] = runner
    result["executed"] = True
    result["exit_status"] = command_result.exit_status
    result["runner_stdout"] = command_result.stdout
    result["runner_stderr"] = command_result.stderr
    result["checks_verified"] = True
    if checks:
        result["checks"] = checks["checks"]
    return result


SEMVER = re.compile(
    r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
    r"(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$"
)


def validate_semver(version: str) -> None:
    if not SEMVER.fullmatch(version):
        raise ReleaseError("--version must be a SemVer value like 1.2.3")


def release_tag(repo: Path, version: str, tag_prefix: str) -> str:
    if not version or version != version.strip() or "\x00" in version:
        raise ReleaseError("--version must be a non-empty value without surrounding whitespace")
    tag = f"{tag_prefix}{version}"
    if tag.startswith("-"):
        raise ReleaseError("Git tag must not start with '-'")
    result = run_command(repo, ["git", "check-ref-format", f"refs/tags/{tag}"])
    if not result.ok:
        raise ReleaseError(f"version and tag prefix produce an invalid Git tag: {tag}")
    return tag


def bump_semver(version: str, bump: str) -> str:
    if "-" in version or "+" in version:
        raise ReleaseError(
            "--bump requires a stable current version; pass --version for a prerelease"
        )
    validate_semver(version)
    major, minor, patch = (int(part) for part in version.split("."))
    if bump == "major":
        return f"{major + 1}.0.0"
    if bump == "minor":
        return f"{major}.{minor + 1}.0"
    return f"{major}.{minor}.{patch + 1}"


def cargo_package(repo: Path, source: str) -> tuple[str, str]:
    path = repo_file(repo, source, "version source")
    try:
        with path.open("rb") as stream:
            package = tomllib.load(stream)["package"]
        name, version = package["name"], package["version"]
    except (OSError, KeyError, TypeError, tomllib.TOMLDecodeError) as exc:
        raise ReleaseError(f"failed to read package name and version from {path}") from exc
    if not isinstance(name, str) or not isinstance(version, str):
        raise ReleaseError(f"{path} package name and version must be strings")
    return name, version


def rewrite_manifest(path: Path, version: str) -> None:
    body = path.read_text()
    section = None
    changed = False
    output = []
    for line in body.splitlines(keepends=True):
        match = re.match(r"\s*\[([^]]+)]\s*$", line)
        if match:
            section = match.group(1)
        if section == "package" and re.match(r"\s*version\s*=", line) and not changed:
            ending = "\n" if line.endswith("\n") else ""
            indent = line[: len(line) - len(line.lstrip())]
            line = f'{indent}version = "{version}"{ending}'
            changed = True
        output.append(line)
    if not changed:
        raise ReleaseError(f"{path} does not declare package.version")
    path.write_text("".join(output))


def rewrite_lock(path: Path, package_name: str, version: str) -> None:
    body = path.read_text()
    blocks = body.split("[[package]]")
    changed = False
    for index in range(1, len(blocks)):
        block = blocks[index]
        if re.search(rf'^name = "{re.escape(package_name)}"$', block, re.MULTILINE):
            block, count = re.subn(
                r'^version = "[^"]+"$',
                f'version = "{version}"',
                block,
                count=1,
                flags=re.MULTILINE,
            )
            if count:
                blocks[index] = block
                changed = True
                break
    if not changed:
        raise ReleaseError(f"package {package_name} not found in {path}")
    path.write_text("[[package]]".join(blocks))


def git(repo: Path, *args: str) -> str:
    return require_success(run_command(repo, ["git", *args]))


def ensure_tag_absent(repo: Path, tag: str) -> None:
    if git(repo, "tag", "--list", tag):
        raise ReleaseError(f"local tag already exists: {tag}")
    if git(repo, "ls-remote", "--tags", "origin", f"refs/tags/{tag}"):
        raise ReleaseError(f"remote tag already exists: {tag}")


def cargo_current_version(repo: Path, args: argparse.Namespace) -> int:
    _, version = cargo_package(repo, args.version_source)
    print(version)
    return 0


def read_version_file(repo: Path, value: str) -> tuple[Path, str]:
    path = repo_file(repo, value, "version file")
    try:
        version = path.read_text().strip()
    except OSError as exc:
        raise ReleaseError(f"failed to read {path}: {exc}") from exc
    validate_semver(version)
    return path, version


def version_file_current(repo: Path, args: argparse.Namespace) -> int:
    _, version = read_version_file(repo, args.version_file)
    print(version)
    return 0


def prepare_release(
    repo: Path, current: str, args: argparse.Namespace
) -> tuple[str, str, Path | None]:
    if bool(args.version) == bool(args.bump):
        raise ReleaseError("exactly one of --version or --bump is required")
    version = args.version or bump_semver(current, args.bump)
    validate_semver(version)
    if version == current:
        raise ReleaseError(f"target version matches current version ({version})")
    tag, notes_path = prepare_tag(repo, version, args)
    return version, tag, notes_path


def prepare_tag(
    repo: Path, version: str, args: argparse.Namespace
) -> tuple[str, Path | None]:
    tag = release_tag(repo, version, args.tag_prefix)
    if getattr(args, "contract", None):
        _, config = load_config(repo, args.contract)
        validate_consumer_version(repo, config, version)
        if not args.dry_run and not config["release"].get("publish", True):
            raise ReleaseError("release.publish is false; apply is disabled")
    if args.provider == "gitea" and args.not_latest:
        raise ReleaseError("--not-latest is not supported by the Gitea provider")
    expected = getattr(args, "expected_head", None)
    if expected and git(repo, "rev-parse", "HEAD") != expected:
        raise ReleaseError("HEAD differs from --expected-head")
    if not expected and not getattr(args, "resume", False):
        args.expected_head = git(repo, "rev-parse", "HEAD")
    notes_path = repo_file(repo, args.notes_file, "notes file") if args.notes_file else None
    if notes_path and not notes_path.is_file():
        raise ReleaseError(f"notes file not found: {args.notes_file}")
    if args.notes_required and not args.dry_run and notes_path is None:
        raise ReleaseError("--notes-file is required for a real release")
    if git(repo, "status", "--short"):
        raise ReleaseError("working tree must be clean before cutting a release")
    if not args.dry_run:
        branch = git(repo, "branch", "--show-current")
        if branch != args.branch:
            raise ReleaseError(
                f"real releases must run from {args.branch}; current branch is {branch}"
            )
        auth = ["gh", "auth", "status"] if args.provider == "github" else ["tea", "login", "list"]
        require_success(run_command(repo, auth))
    if getattr(args, "resume", False):
        if args.provider != "github" or not expected:
            raise ReleaseError("resume requires GitHub and --expected-head")
        verify_resume_tag(repo, tag, expected)
    else:
        ensure_tag_absent(repo, tag)
    return tag, notes_path


def verify_resume_tag(repo: Path, tag: str, expected: str) -> None:
    refs = dict(line.split()[::-1] for line in git(
        repo, "ls-remote", "--tags", "origin", f"refs/tags/{tag}", f"refs/tags/{tag}^{{}}"
    ).splitlines())
    target = refs.get(f"refs/tags/{tag}^{{}}", refs.get(f"refs/tags/{tag}"))
    if target != expected:
        raise ReleaseError("remote release tag does not match --expected-head")
    local = run_command(repo, ["git", "rev-parse", "--verify", f"refs/tags/{tag}^{{commit}}"])
    local_ref = run_command(repo, ["git", "show-ref", "--verify", "--quiet", f"refs/tags/{tag}"])
    if local_ref.ok and (not local.ok or local.stdout.strip() != expected):
        raise ReleaseError("local release tag does not match --expected-head")


def prepared_checks(repo: Path, args: argparse.Namespace) -> None:
    if getattr(args, "contract", None):
        path, config = load_config(repo, args.contract)
        result = execute_check(repo, path, config)
        if not result["ready"]:
            failed = next(item for item in result["checks"] if not item["ok"])
            raise ReleaseError(f"release check failed: {failed['command']}: {failed['stderr'] or failed['stdout']}")
    for check_text in args.check:
        require_success(run_command(repo, shlex.split(check_text)))
    expected = getattr(args, "expected_head", None)
    if expected and git(repo, "rev-parse", "HEAD") != expected:
        raise ReleaseError("HEAD changed during checks")


def ensure_only_version_changes(repo: Path, paths: Sequence[Path]) -> None:
    allowed = {str(path.relative_to(repo)) for path in paths}
    changed = set()
    for arguments in (("diff", "--name-only", "-z"), ("diff", "--cached", "--name-only", "-z"),
                      ("ls-files", "--others", "--exclude-standard", "-z")):
        changed.update(part for part in git(repo, *arguments).split("\x00") if part)
    if changed - allowed:
        raise ReleaseError(f"checks modified files outside the version targets: {sorted(changed - allowed)}")


def publish_release(
    repo: Path, args: argparse.Namespace, tag: str, notes_path: Path | None
) -> None:
    if args.provider == "github":
        if getattr(args, "resume", False):
            endpoint = "repos/{owner}/{repo}/releases/tags/" + quote(tag, safe="")
            existing = run_command(repo, ["gh", "api", endpoint, "--include", "--jq", "{tag_name, draft}"])
            if existing.ok:
                body = existing.stdout.replace("\r\n", "\n").split("\n\n", 1)[-1]
                try:
                    data = json.loads(body)
                except ValueError as exc:
                    raise ReleaseError("invalid release response during resume") from exc
                if not isinstance(data, dict) or data.get("tag_name") != tag or data.get("draft") is not False:
                    raise ReleaseError("existing release requires manual inspection")
                return
            if not re.search(r"^HTTP/\S+ 404(?:\s|$)", existing.stdout, re.MULTILINE):
                require_success(existing)
        publish = ["gh", "release", "create", tag, "--verify-tag"]
        publish += ["--notes-file", str(notes_path)] if notes_path else ["--generate-notes"]
        if args.not_latest:
            publish.append("--latest=false")
    else:
        if args.not_latest:
            raise ReleaseError("--not-latest is not supported by the Gitea provider")
        publish = ["tea", "releases", "create", "--tag", tag, "--title", tag]
        publish += ["--note-file", str(notes_path)] if notes_path else ["--note", f"Release {tag}"]
    require_success(run_command(repo, publish))


def commit_tag_push(repo: Path, paths: Sequence[Path], tag: str) -> None:
    git(repo, "add", *(str(path.relative_to(repo)) for path in paths))
    git(repo, "commit", "-m", f"chore: release {tag}")
    git(repo, "tag", "-a", tag, "-m", f"Release {tag}")
    git(repo, "push", "origin", "HEAD")
    git(repo, "push", "origin", tag)


def version_file_release(repo: Path, args: argparse.Namespace) -> int:
    if getattr(args, "resume", False):
        return tag_release(repo, args)
    version_path, current = read_version_file(repo, args.version_file)
    version, tag, notes_path = prepare_release(repo, current, args)
    original = version_path.read_bytes()
    committing = False
    try:
        version_path.write_text(f"{version}\n")
        prepared_checks(repo, args)
        ensure_only_version_changes(repo, [version_path])
        if args.dry_run:
            print(f"Dry run succeeded for {tag}.")
            return 0

        committing = True
        commit_tag_push(repo, [version_path], tag)
        publish_release(repo, args, tag, notes_path)
        print(f"release ready: {tag}")
        return 0
    finally:
        if args.dry_run or not committing:
            version_path.write_bytes(original)


def tag_release(repo: Path, args: argparse.Namespace) -> int:
    if not args.version:
        raise ReleaseError("--version is required for a tag-only release")
    tag, notes_path = prepare_tag(repo, args.version, args)

    prepared_checks(repo, args)
    if git(repo, "status", "--short"):
        raise ReleaseError("checks modified the working tree")
    if args.dry_run:
        print(f"Dry run succeeded for {tag}.")
        return 0

    if not getattr(args, "resume", False):
        git(repo, "tag", "-a", tag, "-m", f"Release {tag}")
        git(repo, "push", "origin", f"refs/tags/{tag}:refs/tags/{tag}")
    else:
        verify_resume_tag(repo, tag, args.expected_head)
    publish_release(repo, args, tag, notes_path)
    print(f"release ready: {tag}")
    return 0


def cargo_release(repo: Path, args: argparse.Namespace) -> int:
    if getattr(args, "resume", False):
        return tag_release(repo, args)
    package_name, current = cargo_package(repo, args.version_source)
    version, tag, notes_path = prepare_release(repo, current, args)

    targets = [repo_file(repo, item, "version target") for item in args.version_target]
    lockfiles = [repo_file(repo, item, "lockfile") for item in args.lockfile]
    backups = {path: path.read_bytes() for path in {*targets, *lockfiles}}
    committing = False
    try:
        for target in targets:
            rewrite_manifest(target, version)
        for lockfile in lockfiles:
            rewrite_lock(lockfile, package_name, version)
        prepared_checks(repo, args)
        ensure_only_version_changes(repo, list(backups))
        if args.dry_run:
            print(f"Dry run succeeded for {package_name} {tag}.")
            return 0

        committing = True
        commit_tag_push(repo, list(backups), tag)
        publish_release(repo, args, tag, notes_path)
        print(f"release ready: {tag}")
        return 0
    finally:
        if args.dry_run or not committing:
            for path, body in backups.items():
                path.write_bytes(body)


def add_common(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--repo-path", default=".")
    parser.add_argument("--config", default="release.toml")
    parser.add_argument("--json", action="store_true")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="action", required=True)
    for action in ("check", "plan"):
        sub = subparsers.add_parser(action)
        add_common(sub)
        if action == "plan":
            intent = sub.add_mutually_exclusive_group()
            intent.add_argument("--version")
            intent.add_argument("--bump", choices=("major", "minor", "patch"))
    run_parser = subparsers.add_parser("run")
    add_common(run_parser)
    mode = run_parser.add_mutually_exclusive_group()
    mode.add_argument("--dry-run", action="store_true")
    mode.add_argument("--apply", action="store_true")
    intent = run_parser.add_mutually_exclusive_group()
    intent.add_argument("--version")
    intent.add_argument("--bump", choices=("major", "minor", "patch"))
    run_parser.add_argument("--notes-file")
    run_parser.add_argument("--not-latest", action="store_true")
    run_parser.add_argument("--expected-head")
    run_parser.add_argument("--expected-config")
    run_parser.add_argument("--resume", action="store_true")

    current = subparsers.add_parser("cargo-current-version")
    current.add_argument("--repo-path", default=".")
    current.add_argument("--version-source", default="Cargo.toml")

    cargo = subparsers.add_parser("cargo-release")
    cargo.add_argument("--repo-path", default=".")
    cargo.add_argument("--provider", choices=("github", "gitea"), default="github")
    cargo.add_argument("--version-source", default="Cargo.toml")
    cargo.add_argument("--version-target", action="append", default=[])
    cargo.add_argument("--lockfile", action="append", default=[])
    cargo.add_argument("--tag-prefix", default="v")
    cargo.add_argument("--branch", default="main")
    cargo.add_argument("--check", action="append", default=[])
    cargo.add_argument("--notes-required", action="store_true")
    cargo.add_argument("--version")
    cargo.add_argument("--bump", choices=("major", "minor", "patch"))
    cargo.add_argument("--notes-file")
    cargo.add_argument("--not-latest", action="store_true")
    cargo.add_argument("--dry-run", action="store_true")

    version_current = subparsers.add_parser("version-file-current")
    version_current.add_argument("--repo-path", default=".")
    version_current.add_argument("--version-file", default="VERSION")

    version_release = subparsers.add_parser("version-file-release")
    version_release.add_argument("--repo-path", default=".")
    version_release.add_argument("--provider", choices=("github", "gitea"), default="github")
    version_release.add_argument("--version-file", default="VERSION")
    version_release.add_argument("--tag-prefix", default="v")
    version_release.add_argument("--branch", default="main")
    version_release.add_argument("--check", action="append", default=[])
    version_release.add_argument("--notes-required", action="store_true")
    version_release.add_argument("--version")
    version_release.add_argument("--bump", choices=("major", "minor", "patch"))
    version_release.add_argument("--notes-file")
    version_release.add_argument("--not-latest", action="store_true")
    version_release.add_argument("--dry-run", action="store_true")

    tag_only = subparsers.add_parser("tag-release")
    tag_only.add_argument("--repo-path", default=".")
    tag_only.add_argument("--provider", choices=("github", "gitea"), default="github")
    tag_only.add_argument("--tag-prefix", default="")
    tag_only.add_argument("--branch", default="main")
    tag_only.add_argument("--check", action="append", default=[])
    tag_only.add_argument("--notes-required", action="store_true")
    tag_only.add_argument("--version")
    tag_only.add_argument("--notes-file")
    tag_only.add_argument("--not-latest", action="store_true")
    tag_only.add_argument("--dry-run", action="store_true")
    for sub in (cargo, version_release, tag_only):
        sub.add_argument("--resume", action="store_true")
        sub.add_argument("--contract")
        sub.add_argument("--expected-head")
    return parser.parse_args()


def print_result(result: dict[str, Any], as_json: bool) -> None:
    if as_json:
        print(json.dumps(result, sort_keys=True))
    else:
        print(f"{result['name']}: {result['mode']} ready={str(result['ready']).lower()}")


def main() -> int:
    args = parse_args()
    repo = Path(args.repo_path).resolve()
    try:
        if args.action == "cargo-current-version":
            return cargo_current_version(repo, args)
        if args.action == "version-file-current":
            return version_file_current(repo, args)
        if args.action == "version-file-release":
            return version_file_release(repo, args)
        if args.action == "tag-release":
            return tag_release(repo, args)
        if args.action == "cargo-release":
            if not args.version_target:
                args.version_target = [args.version_source]
            return cargo_release(repo, args)
        config_path, config = load_config(repo, args.config)
        if args.action == "check":
            result = execute_check(repo, config_path, config)
        elif args.action == "plan":
            result = execute_plan(repo, config_path, config, args)
        else:
            result = execute_run(repo, config_path, config, args)
        print_result(result, args.json)
        return 1 if result["ready"] is False else 0
    except ReleaseError as exc:
        if getattr(args, "json", False):
            print(json.dumps({"ok": False, "error": str(exc)}, sort_keys=True))
        else:
            print(f"release failed: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
