from app.server import MarketPulseServer


def main() -> None:
    MarketPulseServer.from_environment().run()


if __name__ == "__main__":
    main()
