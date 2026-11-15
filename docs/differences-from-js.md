# Operator Discord - roznice wzgledem wersji JS

Port `main.js` + `src/` (wersja JS, dostepna w historii git pod tagiem `js-final`) na Rust (`discord/`). Te same komendy, teksty odpowiedzi,
pliki stanu (`cache/server/<guildId>.json`, `logs/server/...`, `cache/music-queue/...`), `config.json`
i zmienne srodowiskowe co wersja JS - mozna przelaczac sie miedzy wersjami bez migracji.

## Uruchamianie

```bash
cargo build --release -p operator-discord
./target/release/operator-discord doctor          # npm run doctor
./target/release/operator-discord commands sync   # npm run commands:sync
./target/release/operator-discord commands sync --guild=123,456
./target/release/operator-discord commands sync --global
./target/release/operator-discord commands sync --clear-global
./target/release/operator-discord                 # npm start
```

Binarke uruchamiaj z katalogu projektu (jak `node main.js`) albo ustaw `OPERATOR_CONFIG_PATH`,
`OPERATOR_DATA_DIR`, `OPERATOR_LOG_DIR`. W `operator.service` wystarczy podmienic `ExecStart`
na sciezke do binarki. Poziom logow: `RUST_LOG` (np. `RUST_LOG=debug`).

## Stos

- `twilight` 0.18 (gateway, http, in-memory cache z kalkulatorem uprawnien) zamiast `discord.js`
- wlasny, maly klient Lavalink v4 (REST + WebSocket) zamiast `lavalink-client`
- `tokio`, `rustls` (bez OpenSSL)

## Mapowanie plikow

| JS | Rust |
| --- | --- |
| `main.js` | `src/main.rs`, `src/commands/mod.rs` (dispatch + sync) |
| `src/cmds/*.js` | `src/commands/*.rs` |
| `src/core/command-manifest.js` | `commands.json` (zrzut payloadu z JS) |
| `src/core/music.js` | `src/modules/music.rs`, `src/lavalink.rs` |
| `src/core/verification.js` + verify z `main.js` | `src/modules/verification.rs` |
| `src/events/*.js` | `src/modules/{autoclean,automod,fulllogs}.rs` |
| `src/core/{config,owners,paths,state}.js` | `src/{config,owners,paths,state}.rs` |

## Swiadome roznice wzgledem JS

- `/health`, `/debug`: linia runtime pokazuje wersje Rust/twilight zamiast Node.js/discord.js;
  zniknelo ostrzezenie o wersji Node.
- Reconnect muzyki po restarcie Lavalinka przywraca utwor, pozycje, glosnosc i filtry.
- `commands sync` z CLI uzywa samego REST (bez logowania do gateway).
- Nieuzywany w JS `enqueueSpotifyPlaylistProgressive` nie zostal przeniesiony.

## /ytdlp (z wersji JS na VPS)

- Format sort bez zmian co do idei, ale z preferencja SDR i H.264 przy remisie oraz `--merge-output-format mp4`.
- Konwersja do AV1/AAC (domyslnie; albo H.264) przed wysylka; w JS HEVC wyswietlal sie w Discordzie jako zamrozony obraz.
- Gdy Discord odrzuci zalacznik jako za duzy, bot publikuje player zamiast zwracac blad.
- `YTDLP_PATH` jest konfigurowalny (w JS na sztywno `/usr/local/bin/yt-dlp`, to nadal domyslna wartosc).
