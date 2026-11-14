# operator-telegram (Rust)

Port bota Telegram (`bot.js`, `cmd/ytdlp.js`, `lib/`) na Rust (`crates/operator-telegram`).
Bez Telegrafa: wlasny, maly klient Bot API (hyper + rustls), long polling, te same komendy,
teksty i zmienne `.env` (wzor: `crates/operator-telegram/.env.example`).

## Wymagania

- `yt-dlp` i `ffmpeg`/`ffprobe` w `PATH` (albo `YTDLP_PATH`, `FFMPEG_PATH`, `FFPROBE_PATH`)
- `ffmpeg` z `libx264` (domyslnie) albo `libsvtav1` (dla `YTDLP_VIDEO_CODEC=av1`)
- runtime JS dla yt-dlp (`YTDLP_JS_RUNTIME`, domyslnie `node`)

## Uruchamianie

```bash
cargo build --release -p operator-telegram
cp crates/operator-telegram/.env.example .env   # uzupelnij TELEGRAM_BOT_TOKEN
./target/release/operator-telegram
```

Przyklad uslugi systemd: [operator-telegram.service.example](operator-telegram.service.example).
Plik `telegram-bot-api.service` z wersji JS zawiera `--api-id`/`--api-hash` - trzymaj go poza repo.

## Komendy

- `/ytdlp <URL>` - dodaje materiał do wspólnej kolejki (mniejsze pliki maja wyzszy priorytet)
- `/ytdlp queue`, `/ytdlp status` - podglad kolejki z przyciskami
- `/ytdlp_admin pause|resume|clear|cancel <pozycja>` - tylko `ADMIN_IDS`/`ADMIN_USERNAMES`
- linki YouTube Shorts / Instagram Reels / TikTok w zwyklej wiadomosci sa pobierane automatycznie

## Jak dziala pobieranie

1. yt-dlp wybiera osobno najlepszy obraz i dzwiek: najwyzsza rozdzielczosc i FPS, a przy remisie
   SDR zamiast HDR i strumien w docelowym kodeku (`-S res,fps,hdr:sdr,vcodec:h264,acodec:aac,...`).
2. Po pobraniu `ffprobe` sprawdza kodeki. Jesli obraz nie jest 8-bitowym H.264 (np. HEVC/H.265,
   VP9, AV1, 10-bit/HDR), `ffmpeg` koduje go do H.264 High, `yuv420p`, CRF 18 (`-preset slow`);
   dzwiek inny niz AAC/MP3 (np. Opus) -> AAC 192 kb/s. Pasujace strumienie sa tylko kopiowane.
   Zawsze dodawany jest `+faststart`, wiec film startuje od razu w Discordzie i Telegramie.
3. Plik do limitu (`YTDLP_MAX_UPLOAD_BYTES`) idzie do Telegrama; wiekszy trafia na strone
   (`YTDLP_WEBSITE_ROOT`, `YTDLP_PUBLIC_BASE_URL`) z odtwarzaczem i przyciskiem usuwania.
4. `TELEGRAM_DIRECT_URL_FIRST` wysyla Telegramowi bezposredni URL tylko wtedy, gdy serwis ma
   gotowy strumien w docelowym kodeku; inaczej idzie przekonwertowany plik.

### Kodeki - co wybrac

| `YTDLP_VIDEO_CODEC` | Discord | Telegram | Rozmiar | Uwagi |
| --- | --- | --- | --- | --- |
| `h264` (domyslnie) | tak | tak | bazowy | najszersza zgodnosc |
| `av1` | niepewne | zalezy od klienta | zwykle wyraznie mniejszy przy tej samej jakosci | kodowanie dluzsze |
| `source` | HEVC sie zacina | zalezy | bez zmian | zachowanie jak w JS |

Konwersja nie podnosi jakosci ponad zrodlo - najlepsze co mozna zrobic, to nie tracic jej
(CRF 18 dla x264 jest praktycznie nieodroznialny od zrodla) i nie skalowac obrazu.

## Roznice wzgledem JS

- Sortowanie formatow: JS przekazywal `res:desc,br:desc,...`, co yt-dlp czyta jako
  "preferowana wartosc `desc`" - w praktyce sortowanie bylo ignorowane.
- Konwersja do zgodnego kodeka (wyzej); w JS HEVC z np. TikToka zacinal sie w Discordzie.
- Menu komend ustawiane przy starcie (Telegraf 4.16 robil to dopiero po zatrzymaniu pollingu).
- Rozmiar przyblizony z yt-dlp (`of ~  10.00MiB`) jest poprawnie odczytywany w pasku postepu.
- Przy SIGINT/SIGTERM bot konczy polling, potwierdza offset i czeka do 25 s na trwajace zadanie.
