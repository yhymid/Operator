# Operator - Telegram

Bot Telegram napisany w Rust: pobieranie wideo przez `yt-dlp` z kolejka, paskiem postepu
i wysylka w kodeku, ktory odtwarza sie w Telegramie i Discordzie. To jest glowna linia
**Telegrama**; bot Discord zyje na galezi [`discord`](https://github.com/yhymid/Operator/tree/discord).

## Komendy

- `/ytdlp <URL>` - dodaje material do wspolnej kolejki (mniejsze pliki maja wyzszy priorytet)
- `/ytdlp queue`, `/ytdlp status` - podglad kolejki z przyciskami
- `/ytdlp_admin pause | resume | clear | cancel <pozycja>` - tylko `ADMIN_IDS`
- linki YouTube Shorts / Instagram Reels / TikTok wyslane zwykla wiadomoscia sa pobierane automatycznie

Ograniczenia: jeden uzytkownik moze miec `YTDLP_QUEUE_MAX_PER_USER` zadan (domyslnie 3), adresy
lokalne/prywatne sa odrzucane, a `ALLOWED_CHAT_IDS` (puste = wszystkie) zaweza czaty, w ktorych
bot odpowiada (administratorzy z `ADMIN_IDS` sa obslugiwani wszedzie). Admini tylko po ID -
`ADMIN_USERNAMES` nie jest juz obslugiwane.

## Struktura

```
Cargo.toml              workspace (wspolne wersje zaleznosci, profil release: LTO + strip)
common/                 wspolny crate: .env, czas, sygnaly, atomowy zapis JSON,
                        net (blokada adresow lokalnych), media (ffprobe/ffmpeg)
telegram/
  src/main.rs           start, long polling, zamykanie
  src/api.rs            klient Bot API (JSON + strumieniowy upload multipart)
  src/limiter.rs        limiter per czat (priorytety, retry_after, scalanie edycji)
  src/queue.rs          kolejka zadan (pauza/wznow/anuluj)
  src/ytdlp.rs          /ytdlp: wspolne helpery; ytdlp/{format,download,website,replies,
                        queue_view,handlers,jobs}.rs
  src/bot.rs            routing aktualizacji i komend
  deploy/               .env.example, operator-telegram.service
docs/                   roznice wzgledem wersji JS
scripts/                deploy.sh (instalacja release'u z rollbackiem), sync-common.sh
```

`common/` jest tez na galezi `discord` i musi byc identyczny: zmieniaj go osobnym commitem,
potem `scripts/sync-common.sh` przenosi go na druga galaz (CI ostrzega, gdy sie rozjada).

## Wymagania

- `yt-dlp` oraz `ffmpeg`/`ffprobe` z `libx264` (dla trybu AV1 takze `libsvtav1`)
- runtime JS dla yt-dlp (`YTDLP_JS_RUNTIME`, domyslnie `node`)
- opcjonalnie Local Bot API Server (`TELEGRAM_API_ROOT`) dla plikow powyzej 50 MB

## Szybki start

```bash
cargo build --release -p operator-telegram
mkdir -p run && cd run
cp ../telegram/deploy/.env.example .env      # uzupelnij TELEGRAM_BOT_TOKEN i ADMIN_IDS
../target/release/operator-telegram
```

Bot czyta `.env` (i `cookies.txt` dla YouTube) z katalogu, w ktorym go uruchomisz.
Zmienne sa te same co w wersji JS - stary `.env` dziala bez zmian.

## Deploy (systemd)

Z release'u: `git tag telegram-v1.0.0 telegram && git push github telegram-v1.0.0` buduje
binarke w GitHub Actions, a na serwerze `scripts/deploy.sh telegram telegram-v1.0.0` ja instaluje
(SHA256, czeka na pusta kolejke, restart, rollback gdy unit nie wstanie). Recznie:

```bash
cp target/release/operator-telegram /home/ubuntu/yhymfiles/operator-tg/
sudo cp telegram/deploy/operator-telegram.service /etc/systemd/system/operator-tg.service
sudo systemctl daemon-reload && sudo systemctl enable --now operator-tg
journalctl -u operator-tg -f
```

Dopasuj `WorkingDirectory`/`ExecStart` w pliku uslugi. Plik uslugi Local Bot API Server
zawiera `--api-id`/`--api-hash` - trzymaj go poza repozytorium.

## Jak dziala pobieranie

1. **Wybor formatu** - yt-dlp bierze osobno najlepszy obraz i dzwiek: najwyzsza rozdzielczosc
   i FPS, a przy remisie SDR zamiast HDR i strumien w docelowym kodeku
   (`-S res,fps,hdr:sdr,vcodec:h264,acodec:aac,br,asr,size`).
2. **Zgodnosc** - `ffprobe` sprawdza pobrany plik. Obraz inny niz 8-bitowy H.264 (HEVC/H.265, VP9,
   AV1, 10-bit/HDR) jest kodowany do H.264 High `yuv420p`, CRF 18, `-preset slow`; dzwiek inny niz
   AAC/MP3 (np. Opus) -> AAC 192 kb/s. Pasujace strumienie sa tylko kopiowane. Zawsze `+faststart`,
   wiec film startuje od razu.
3. **Wysylka** - plik do `YTDLP_MAX_UPLOAD_BYTES` idzie do Telegrama; wiekszy trafia na strone
   (`YTDLP_WEBSITE_ROOT`, `YTDLP_PUBLIC_BASE_URL`) z odtwarzaczem i przyciskiem usuwania;
   strona jest kasowana po `YTDLP_WEBSITE_TTL_HOURS` (24 h).
   `TELEGRAM_DIRECT_URL_FIRST` wysyla Telegramowi bezposredni URL tylko, gdy serwis ma gotowy
   strumien w docelowym kodeku.

### Kodek (`YTDLP_VIDEO_CODEC`)

| Wartosc | Discord | Telegram | Rozmiar | Uwagi |
| --- | --- | --- | --- | --- |
| `h264` (domyslnie) | tak | tak | bazowy | najszersza zgodnosc |
| `av1` | tak (bot Discord uzywa go domyslnie) | zalezy od klienta | zwykle wyraznie mniejszy | dluzsze kodowanie (SVT-AV1 preset 6) |
| `source` | HEVC sie zacina | zalezy | bez zmian | bez konwersji, jak w JS |

`YTDLP_VIDEO_CRF` zmienia jakosc konwersji (mniej = lepiej i wiekszy plik). Konwersja nie
podnosi jakosci ponad zrodlo - cel to jej nie tracic i nie skalowac obrazu.

## Najwazniejsze zmienne

Pelna lista z opisami: `telegram/deploy/.env.example`. `DEVELOPER_MODE=true` wlacza szczegolowe
logi aktualizacji, yt-dlp i uploadu.

## Rozwoj

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

To samo sprawdza CI (`.github/workflows/rust.yml`).
