# Operator - Discord

Bot Discord napisany w Rust. To jest glowna linia **Discorda**; bot Telegram zyje na galezi
[`telegram`](https://github.com/yhymid/Operator/tree/telegram). Dawna wersja JS (`discord.js`)
zostala w historii git (tag `js-final`).

## Co potrafi

- **AutoClean** - cykliczne czyszczenie kanalow (CRON + strefa czasowa)
- **AutoMod** - slowa, regexy, linki, zaproszenia, CAPS, duplikaty, wzmianki; `/automod test`
- **Verify** - panel z captcha, wygasanie roli, wyjatki
- **Music** - Lavalink v4: kolejka, panel z przyciskami, loop, autoplay, EQ/FX, zapis kolejki
- **Full Logs** - pelne logi serwera na kanal + pliki JSONL z retencja, eksport, purge
- **Announcements** - planowane ogloszenia przez webhooki, profile, edycja wyslanych
- **/ytdlp** - pobiera video (yt-dlp) i wysyla na kanal w AV1 (male pliki, odtwarza sie w Discordzie);
  za duze pliki ida na strone jako player (link wazny 6 h)
- **Health / Debug** - diagnostyka muzyki, uprawnien i logow; narzedzia ownera (`/cache`, `/dm`)

## Struktura

```
Cargo.toml                 workspace (wspolne wersje zaleznosci, profil release: LTO + strip)
common/                    wspolny crate: .env, logowanie, atomowy zapis JSON, czas, sygnaly,
                           net (blokada adresow lokalnych), media (ffprobe/ffmpeg, feature "media")
discord/
  src/main.rs              start, petla gateway, CLI (run / commands sync / doctor)
  src/commands/            komendy slash (/help, /music, /logs, ...)
  src/modules/             autoclean, automod, verification, announcements,
                           fulllogs/events/* (handler na zdarzenie), music/* (voice, search, controls, ...)
  src/lavalink.rs          klient Lavalink v4 (REST + WebSocket)
  commands.json            definicje komend slash
  deploy/                  .env.example, config.example.json, operator-discord.service,
                           lavalink/ (application.yml.example, lavalink.service)
docs/                      deploy na VPS, roznice wzgledem JS
scripts/                   deploy.sh (instalacja release'u z rollbackiem), sync-common.sh
```

`common/` jest tez na galezi `telegram` i musi byc identyczny: zmieniaj go osobnym commitem,
potem `scripts/sync-common.sh` przenosi go na druga galaz (CI ostrzega, gdy sie rozjada).

## Szybki start

Wymagania: Rust (stable, >= 1.88), opcjonalnie Lavalink v4 (Java 17+) dla muzyki,
`yt-dlp` + `ffmpeg`/`ffprobe` (z `libsvtav1`; `libx264` dla trybu H.264) dla `/ytdlp`.

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

Konfiguracja: `discord/deploy/lavalink/application.yml.example`, usluga systemd:
`discord/deploy/lavalink/lavalink.service` (Lavalink 4.2+, Java 17+). Pluginy Lavalink pobiera sam.
Polaczenie bota ustawiasz w `.env` (`LAVALINK_URL` albo `LAVALINK_HOST`/`PORT`/`PASSWORD`/`SECURE`/`NODE_ID`).
Bez Lavalinka bot dziala, a muzyka laczy sie sama, gdy node wstanie (ponawianie co 10 s).

YouTube z VPS (sprawdzone 2026-11):

- plugin youtube-source jako **snapshot** (build z mastera) - wydanie 1.18.2 nie ma poprawek
  potrzebnych na adresach serwerowni (klient TV, IOS),
- OAuth wlaczony (odtwarzanie klientem TV), szyfr przez `remoteCipher` (`cipher.kikkia.dev`),
- zrodlo `ytdlp` w LavaSrc wylaczone - rejestruje sie przed pluginem i przejmuje YouTube,
- bez `preferIPv6Addresses` - YouTube moze lokalizowac IPv6 serwera w innym kraju niz IPv4.

## /ytdlp

1. yt-dlp pobiera najlepszy obraz i dzwiek (najwyzsza rozdzielczosc i FPS; przy remisie SDR i AV1).
2. `ffprobe` sprawdza plik: obraz inny niz 8-bitowy AV1 (H.264, HEVC/H.265, VP9, 10-bit/HDR) jest
   kodowany SVT-AV1 (`-preset 10`, CRF 30, `yuv420p`; `YTDLP_AV1_PRESET`) - Discord pokazuje zamrozony obraz dla HEVC;
   Opus -> AAC; pasujace strumienie sa kopiowane. Zawsze `+faststart`. Wyjatek: 8-bitowe H.264,
   ktore miesci sie w limicie (z 10% zapasu), jest tylko przepakowywane - bez kosztownego kodowania AV1.
   `YTDLP_VIDEO_CODEC=h264` przelacza na H.264 High CRF 18 (najszersza zgodnosc, ok. 2,5x wieksze pliki).
3. W trakcie widac tylko "Operator is thinking..."; gotowy plik jest wysylany bez tekstu.
   Plik do `YTDLP_MAX_UPLOAD_BYTES` (domyslnie 25 MiB) idzie jako zalacznik. Wiekszy - albo
   odrzucony przez Discorda jako za duzy (serwer bez boostow ma nizszy limit) - trafia do
   `YTDLP_MEDIA_ROOT` jako player pod `YTDLP_MEDIA_BASE_URL`, kasowany po 6 godzinach.

Jedno zadanie naraz (kolejne czekaja), lacznie do 14 minut (token Discorda zyje 15). Filmy dluzsze
niz `YTDLP_MAX_DURATION_SECONDS` (30 min) sa odrzucane, a pobieranie ponad `YTDLP_MAX_DOWNLOAD_BYTES`
(1 GiB) przerywane. Adresy lokalne/prywatne (127.0.0.1, LAN, 169.254.169.254) sa odrzucane; na czat
trafia zrozumialy powod bledu, surowe wyjscie yt-dlp tylko do logu.

Zmienne: `YTDLP_PATH`, `YTDLP_COOKIES_PATH`, `YTDLP_JS_RUNTIME`, `YTDLP_VIDEO_CODEC=h264|av1|source`,
`YTDLP_VIDEO_CRF`, `YTDLP_AV1_PRESET`, `YTDLP_MAX_DURATION_SECONDS`, `YTDLP_MAX_DOWNLOAD_BYTES`,
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
