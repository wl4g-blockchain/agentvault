from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path


@dataclass(frozen=True)
class ScenarioEvidence:
    module: str
    filename: str
    scenario: str
    round_number: int
    output: str


@dataclass
class ReportBook:
    title: str
    root: Path
    run_id: str
    entries: list[ScenarioEvidence] = field(default_factory=list)

    def record(self, module: str, filename: str, scenario: str, round_number: int, output: str) -> Path:
        if Path(module).name != module or Path(filename).name != filename or not filename.endswith(".md"):
            raise ValueError("report module and filename must be safe relative names")
        evidence = ScenarioEvidence(module, filename, scenario, round_number, output.strip())
        self.entries.append(evidence)
        module_root = self.root / module
        module_root.mkdir(parents=True, exist_ok=True)
        path = module_root / filename
        same_report = [entry for entry in self.entries if entry.module == module and entry.filename == filename]
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
            lines.append(f"| {entry.scenario} | {entry.round_number} | [{relative}]({relative}) |")
        lines.append("")
        path = self.root / "summary.md"
        path.write_text("\n".join(lines), encoding="utf-8")
        return path
