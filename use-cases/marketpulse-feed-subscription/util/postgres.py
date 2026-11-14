from pathlib import Path
from typing import Callable


class E2EPostgresImporter:
    def __init__(self, sql: Callable[[str], str]) -> None:
        self.sql = sql

    def import_file(self, path: Path) -> str:
        return self.sql("BEGIN;\n" + path.read_text(encoding="utf-8") + "\nCOMMIT;")


class E2EPostgresChecker:
    def __init__(self, sql: Callable[[str], str]) -> None:
        self.sql = sql

    def query(self, statement: str) -> str:
        return self.sql(statement)

    def require_scalar(self, statement: str, expected: str) -> str:
        value = self.query(statement).strip()
        if value != expected:
            raise RuntimeError(
                f"PostgreSQL evidence mismatch: expected {expected!r}, got {value!r}"
            )
        return value
