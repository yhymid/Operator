# Operator

> **Galaz `rust/telegram-rewrite`**: bot Telegram przepisany na Rust - `crates/operator-telegram`.
> Szczegoly, konfiguracja i nowa mechanika pobierania (zgodne kodeki H.264/AV1):
> [docs/rust-telegram.md](docs/rust-telegram.md). Ponizej opis wersji JS bota Discord,
> ktora zostaje w repo jako punkt odniesienia.
>
> ```bash
> cargo build --release -p operator-telegram
> ./target/release/operator-telegram
> ```

Operator to bot Discord oparty o `discord.js`, przygotowany pod lokalne rozwijanie zmian i pozniejsze spokojne wdrozenie na VPS.

## Co potrafi

- `AutoClean` do cyklicznego czyszczenia wybranych kanalow
- `AutoMod` z testowaniem regul i szybka edycja z poziomu komend
- `Verify` z panelem i captcha
- `Music` przez Lavalink
- `Full Logs` z retencja i eksportem
- `Announcements` przez webhooki
- `Health`, `Debug` i narzedzia ownera

## Wymagania

- Node.js `22.16.0`
- npm `>=10`
- bot Discord z poprawnymi intentami
- opcjonalnie Lavalink dla modulu muzyki

## Szybki start lokalnie

1. Skopiuj `.env.example` do `.env` i uzupelnij `BOT_TOKEN`.
2. Skopiuj `config.example.json` do `config.json` i dopasuj ustawienia.
3. Zainstaluj zaleznosci:

```bash
npm ci
```

4. Uruchom szybki przeglad konfiguracji:

```bash
npm run doctor
```

5. Zarejestruj komendy na serwerach, na ktorych bot juz jest:

```bash
npm run commands:sync
```

6. Uruchom testy:

```bash
npm test
```

7. Start bota:

```bash
npm start
```

## Najwazniejsze skrypty

- `npm test` - syntax + smoke tests
- `npm run doctor` - preflight configu, env i katalogow runtime
- `npm run commands:sync` - sync komend slash na wszystkich serwerach bota
- `npm run commands:sync:guild -- --guild=GUILD_ID` - sync tylko dla jednego serwera
- `npm run commands:sync:global` - globalny sync komend
- `npm run commands:clear:global` - wyczyszczenie globalnych komend
- `npm run state:migrate:dry-run` - probna migracja state do SQLite

## Katalogi runtime

Domyslnie projekt korzysta z:

- `config.json`
- `cache/`
- `logs/`

Na VPS warto ustawic osobne katalogi przez zmienne srodowiskowe:

- `OPERATOR_CONFIG_PATH`
- `OPERATOR_DATA_DIR`
- `OPERATOR_LOG_DIR`
- `OPERATOR_SYNC_COMMANDS_ON_STARTUP`

Przyklad:

```bash
export OPERATOR_CONFIG_PATH=/srv/operator/config.json
export OPERATOR_DATA_DIR=/srv/operator/data
export OPERATOR_LOG_DIR=/srv/operator/logs
export OPERATOR_SYNC_COMMANDS_ON_STARTUP=0
```

## Deploy

Krotki proces deployu jest opisany w [docs/deploy-vps.md](docs/deploy-vps.md).
