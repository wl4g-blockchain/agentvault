from __future__ import annotations

import os
import subprocess
from pathlib import Path
from typing import Mapping, Sequence


def run(
    command: Sequence[str],
    cwd: Path,
    environment: Mapping[str, str] | None = None,
    timeout_seconds: float | None = None,
) -> str:
    merged = os.environ.copy()
    if environment:
        merged.update(environment)
    try:
        completed = subprocess.run(
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
    if completed.returncode != 0:
        rendered = " ".join(command)
        raise RuntimeError(
            f"command failed with exit code {completed.returncode}: {rendered}\n{completed.stdout}"
        )
    return completed.stdout
