from __future__ import annotations

import os
import signal
import subprocess
from contextlib import contextmanager
from pathlib import Path
from typing import Mapping, Sequence


class E2EProcessHelper:
    @staticmethod
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
            raise RuntimeError(
                f"command timed out after {timeout_seconds}s: {rendered}\n{output}"
            ) from error

    @staticmethod
    def run(
        command: Sequence[str],
        cwd: Path,
        environment: Mapping[str, str] | None = None,
        timeout_seconds: float | None = None,
    ) -> str:
        completed = E2EProcessHelper.execute(command, cwd, environment, timeout_seconds)
        if completed.returncode != 0:
            rendered = " ".join(command)
            raise RuntimeError(
                f"command failed with exit code {completed.returncode}: {rendered}\n{completed.stdout}"
            )
        return completed.stdout

    @staticmethod
    @contextmanager
    def background(
        command: Sequence[str],
        cwd: Path,
        log_path: Path,
        environment: Mapping[str, str] | None = None,
    ):
        """Own one background process and release its log and process on every exit."""
        log_path.parent.mkdir(parents=True, exist_ok=True)
        with log_path.open("w", encoding="utf-8") as output:
            process = subprocess.Popen(
                command,
                cwd=cwd,
                env={**os.environ, **(environment or {})},
                stdout=output,
                stderr=subprocess.STDOUT,
                text=True,
                start_new_session=os.name == "posix",
            )
            try:
                yield process
            finally:
                if os.name == "posix":
                    try:
                        os.killpg(process.pid, signal.SIGTERM)
                    except ProcessLookupError:
                        pass
                elif process.poll() is None:
                    process.terminate()
                if process.poll() is None:
                    try:
                        process.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        if os.name == "posix":
                            os.killpg(process.pid, signal.SIGKILL)
                        else:
                            process.kill()
                        process.wait(timeout=5)
