import sqlite3
from pathlib import Path


class E2ESQLiteImporter:
    @staticmethod
    def snapshot(source: Path, destination: Path) -> None:
        destination.parent.mkdir(parents=True, exist_ok=True)
        with sqlite3.connect(source.resolve().as_uri() + "?mode=ro", uri=True) as src:
            with sqlite3.connect(destination) as dst:
                src.backup(dst)


class E2ESQLiteChecker:
    @staticmethod
    def connect(path: Path) -> sqlite3.Connection:
        if not path.is_file():
            raise RuntimeError(f"missing SQLite evidence: {path}")
        connection = sqlite3.connect(path.resolve().as_uri() + "?mode=ro", uri=True)
        connection.execute("PRAGMA query_only=ON")
        return connection

    @staticmethod
    def one(connection: sqlite3.Connection, sql: str) -> tuple:
        row = connection.execute(sql).fetchone()
        if row is None:
            raise RuntimeError(f"no evidence for query: {sql}")
        return row
