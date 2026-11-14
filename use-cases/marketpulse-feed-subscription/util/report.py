from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path


@dataclass
class E2EReportHelper:
    @dataclass(frozen=True)
    class ScenarioEvidence:
        module: str
        filename: str
        scenario: str
        round_number: int
        output: str

    title: str
    root: Path
    run_id: str
    entries: list[E2EReportHelper.ScenarioEvidence] = field(default_factory=list)

    def record(
        self, module: str, filename: str, scenario: str, round_number: int, output: str
    ) -> Path:
        if (
            Path(module).name != module
            or Path(filename).name != filename
            or not filename.endswith(".md")
        ):
            raise ValueError("report module and filename must be safe relative names")
        evidence = E2EReportHelper.ScenarioEvidence(
            module, filename, scenario, round_number, output.strip()
        )
        self.entries.append(evidence)
        module_root = self.root / module
        module_root.mkdir(parents=True, exist_ok=True)
        path = module_root / filename
        same_report = [
            entry
            for entry in self.entries
            if entry.module == module and entry.filename == filename
        ]
        lines = [
            f"# {scenario}",
            "",
            f"Run: `{self.run_id}`",
            f"Generated: {datetime.now(timezone.utc).isoformat()}",
            "",
        ]
        for entry in same_report:
            lines.extend(
                (
                    f"## Round {entry.round_number}",
                    "",
                    "```text",
                    entry.output or "ok",
                    "```",
                    "",
                )
            )
        path.write_text("\n".join(lines), encoding="utf-8")
        return path

    def write_summary(self) -> Path:
        self.root.mkdir(parents=True, exist_ok=True)
        lines = [
            f"# {self.title}",
            "",
            f"Run: `{self.run_id}`",
            f"Generated: {datetime.now(timezone.utc).isoformat()}",
            "",
            "| Scenario | Round | Evidence |",
            "| --- | ---: | --- |",
        ]
        for entry in self.entries:
            relative = f"{entry.module}/{entry.filename}"
            lines.append(
                f"| {entry.scenario} | {entry.round_number} | [{relative}]({relative}) |"
            )
        lines.append("")
        path = self.root / "summary.md"
        path.write_text("\n".join(lines), encoding="utf-8")
        return path

    @staticmethod
    def create_run_root(root: Path) -> tuple[str, Path]:
        from datetime import timedelta

        instant = datetime.now(timezone.utc)
        while True:
            run_id = instant.strftime("%Y%m%d%H%M%S")
            path = root / run_id
            try:
                path.mkdir(parents=True)
                return run_id, path
            except FileExistsError:
                instant += timedelta(seconds=1)

    @staticmethod
    def archive(
        root: Path, keep: int = 20, protected: tuple[str, ...] = ()
    ) -> list[Path]:
        import shutil

        if keep < 1:
            raise ValueError("report retention must keep at least one completed run")
        runs = sorted(
            (
                p
                for p in root.iterdir()
                if p.is_dir()
                and len(p.name) == 14
                and p.name.isdigit()
                and (p / "summary.md").is_file()
            ),
            reverse=True,
        )
        archived = []
        for path in runs[keep:]:
            if path.name in protected:
                continue
            archive = root / "archive"
            archive.mkdir(exist_ok=True)
            destination = archive / path.name
            if destination.exists():
                raise RuntimeError(f"archive already exists: {destination}")
            shutil.move(str(path), destination)
            archived.append(destination)
        return archived

    @staticmethod
    def source_identity(repository: Path) -> dict[str, str]:
        import hashlib
        import subprocess

        revision = subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=repository, text=True
        ).strip()
        names = (
            subprocess.check_output(
                [
                    "git",
                    "ls-files",
                    "-z",
                    "--cached",
                    "--others",
                    "--exclude-standard",
                    "--",
                    "src",
                    "tests",
                    "migrations",
                    "etc",
                    "deploy",
                    "use-cases",
                    "web",
                    "docs",
                    "Cargo.toml",
                    "Cargo.lock",
                    "build.rs",
                    "Makefile",
                    ".dockerignore",
                    ".gitignore",
                    ".github/workflows",
                ],
                cwd=repository,
            )
            .decode()
            .split("\0")
        )
        digest = hashlib.sha256()
        for name in sorted(set(filter(None, names))):
            path = repository / name
            if "reports" in Path(name).parts or not path.is_file():
                continue
            digest.update(
                name.encode() + b"\0" + hashlib.sha256(path.read_bytes()).digest()
            )
        return {
            "revision": revision,
            "working_tree_sha256": digest.hexdigest(),
            "scope_version": "2",
        }

    @staticmethod
    def seal(directory: Path, metadata: dict) -> Path:
        import hashlib
        import json

        artifacts = {}
        for path in sorted(directory.rglob("*")):
            if path.is_file() and path.name != "manifest.json":
                artifacts[path.relative_to(directory).as_posix()] = hashlib.sha256(
                    path.read_bytes()
                ).hexdigest()
        if not artifacts:
            raise RuntimeError("cannot seal empty evidence")
        target = directory / "manifest.json"
        target.write_text(
            json.dumps(
                {"version": 1, "metadata": metadata, "artifacts": artifacts},
                sort_keys=True,
                indent=2,
            )
            + "\n"
        )
        return target

    @staticmethod
    def verify_manifest(directory: Path) -> dict:
        import hashlib
        import json

        payload = json.loads((directory / "manifest.json").read_text())
        if payload.get("version") != 1 or not payload.get("artifacts"):
            raise RuntimeError("unsupported or empty evidence manifest")
        for name, digest in payload["artifacts"].items():
            path = directory / name
            if (
                not path.resolve().is_relative_to(directory.resolve())
                or not path.is_file()
                or hashlib.sha256(path.read_bytes()).hexdigest() != digest
            ):
                raise RuntimeError(f"evidence content changed or is missing: {name}")
        return payload
