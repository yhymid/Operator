# Operator - Discord

Bot Discord napisany w Rust. To jest glowna linia **Discorda**; bot Telegram zyje na galezi
[`telegram`](https://github.com/yhymid/Operator/tree/telegram). Dawna wersja JS (`discord.js`)
zostala w historii i na `main`.

## Co potrafi

- **AutoClean** - cykliczne czyszczenie kanalow (CRON + strefa czasowa)
- **AutoMod** - slowa, regexy, linki, zaproszenia, CAPS, duplikaty, wzmianki; `/automod test`
- **Verify** - panel z captcha, wygasanie roli, wyjatki
- **Music** - Lavalink v4: kolejka, panel z przyciskami, loop, autoplay, EQ/FX, zapis kolejki
- **Full Logs** - pelne logi serwera na kanal + pliki JSONL z retencja, eksport, purge
- **Announcements** - planowane ogloszenia przez webhooki, profile, edycja wyslanych
- **/ytdlp** - pobiera video (yt-dlp) i wysyla na kanal w H.264, wiec odtwarza sie w Discordzie;
  za duze pliki ida na strone jako player (link wazny 6 h)
- **Health / Debug** - diagnostyka muzyki, uprawnien i logow; narzedzia ownera (`/cache`, `/dm`)

## Struktura

```
Cargo.toml                 workspace (wspolne wersje zaleznosci, profil release: LTO + strip)
common/                    wspolny crate: .env, logowanie, atomowy zapis JSON, czas, sygnaly
discord/
  src/main.rs              start, petla gateway, CLI (run / commands sync / doctor)
  src/commands/            komendy slash (/help, /music, /logs, ...)
  src/modules/             autoclean, automod, fulllogs, music, verification, announcements
  src/lavalink.rs          klient Lavalink v4 (REST + WebSocket)
  commands.json            definicje komend slash
  deploy/                  .env.example, config.example.json, operator-discord.service,
                           lavalink/ (application.yml.example + pluginy)
docs/                      deploy na VPS, roznice wzgledem JS
```

## Szybki start

Wymagania: Rust (stable, >= 1.88), opcjonalnie Lavalink v4 (Java 17+) dla muzyki,
`yt-dlp` + `ffmpeg`/`ffprobe` (z `libx264`) dla `/ytdlp`.

```bash
cargo build --release -p operator-discord
mkdir -p run && cd run
cp ../discord/deploy/.env.example .env              # uzupelnij BOT_TOKEN i OWNER_IDS
cp ../discord/deploy/config.example.json config.json
../target/release/operator-discord doctor           # sprawdza token, config i katalogi
../target/release/operator-discord commands sync    # rejestruje komendy slash
../target/release/operator-discord                  # start bota
```

Bot czyta `.env`, `config.json`, `cache/` i `logs/` z katalogu, w ktorym go uruchomisz
(albo ze sciezek `OPERATOR_CONFIG_PATH`, `OPERATOR_DATA_DIR`, `OPERATOR_LOG_DIR`).
Format tych plikow jest taki sam jak w wersji JS - istniejacy config i stan dzialaja bez zmian.

## CLI

| Polecenie | Opis |
| --- | --- |
| `operator-discord` | start bota |
| `operator-discord doctor` | preflight: token, config, katalogi, owner IDs |
| `operator-discord commands sync` | sync komend na wszystkich serwerach bota |
| `operator-discord commands sync --guild=ID[,ID]` | sync dla wybranych serwerow |
| `operator-discord commands sync --global` / `--clear-global` | komendy globalne |

Logi: `RUST_LOG=debug` dla szczegolow. Pod systemd znaczniki czasu dodaje journald.

## Lavalink

Przyklad konfiguracji: `discord/deploy/lavalink/application.yml.example`. Polaczenie ustawiasz
w `.env` (`LAVALINK_URL` albo `LAVALINK_HOST`/`PORT`/`PASSWORD`/`SECURE`/`NODE_ID`).
Bez Lavalinka bot dziala, a muzyka laczy sie sama, gdy node wstanie (ponawianie co 10 s).

## /ytdlp

1. yt-dlp pobiera najlepszy obraz i dzwiek (najwyzsza rozdzielczosc i FPS; przy remisie SDR i H.264).
2. `ffprobe` sprawdza plik: HEVC/H.265, VP9, AV1 lub 10-bit/HDR jest kodowany do H.264 High
   `yuv420p` CRF 18 (Discord pokazuje zamrozony obraz dla HEVC), Opus -> AAC; reszta jest kopiowana.
   Zawsze `+faststart`.
3. Plik do `YTDLP_MAX_UPLOAD_BYTES` (domyslnie 25 MiB) idzie jako zalacznik. Wiekszy - albo
   odrzucony przez Discorda jako za duzy (serwer bez boostow ma nizszy limit) - trafia do
   `YTDLP_MEDIA_ROOT` jako player pod `YTDLP_MEDIA_BASE_URL`, kasowany po 6 godzinach.

Zmienne: `YTDLP_PATH`, `YTDLP_COOKIES_PATH`, `YTDLP_VIDEO_CODEC=h264|av1|source`, `YTDLP_VIDEO_CRF`,
`FFMPEG_PATH`, `FFPROBE_PATH` - opisane w `discord/deploy/.env.example`.

## Rozwoj

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

To samo sprawdza CI (`.github/workflows/rust.yml`).

## Dokumentacja

- [docs/deploy-vps.md](docs/deploy-vps.md) - deploy na VPS z systemd
- [docs/differences-from-js.md](docs/differences-from-js.md) - co sie zmienilo wzgledem wersji JS
