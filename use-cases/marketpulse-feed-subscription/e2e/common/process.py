from __future__ import annotations

import os
import subprocess
from pathlib import Path
from typing import Mapping, Sequence


def execute(
    command: Sequence[str],
    cwd: Path,
    environment: Mapping[str, str] | None = None,
    timeout_seconds: float | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run one bounded child process and retain its combined diagnostic output."""
    merged = os.environ.copy()
    if environment:
        merged.update(environment)
    try:
        return subprocess.run(
            command,
            cwd=cwd,
            env=merged,
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            timeout=timeout_seconds,
        )
    except subprocess.TimeoutExpired as error:
        rendered = " ".join(command)
        output = error.stdout or ""
        raise RuntimeError(f"command timed out after {timeout_seconds}s: {rendered}\n{output}") from error


def run(
    command: Sequence[str],
    cwd: Path,
    environment: Mapping[str, str] | None = None,
    timeout_seconds: float | None = None,
) -> str:
    completed = execute(command, cwd, environment, timeout_seconds)
    if completed.returncode != 0:
        rendered = " ".join(command)
        raise RuntimeError(
            f"command failed with exit code {completed.returncode}: {rendered}\n{completed.stdout}"
        )
    return completed.stdout
