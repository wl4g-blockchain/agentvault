import logging

from app.server import MarketPulseServer


def main() -> None:
    logging.basicConfig(level=logging.INFO, format="%(message)s")
    MarketPulseServer.from_environment().run()


if __name__ == "__main__":
    main()
