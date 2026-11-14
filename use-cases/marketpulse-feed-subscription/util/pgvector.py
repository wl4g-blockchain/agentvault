from util.postgres import E2EPostgresImporter


class E2EPgvectorImporter(E2EPostgresImporter):
    def enable(self) -> str:
        return self.sql("CREATE EXTENSION IF NOT EXISTS vector")
