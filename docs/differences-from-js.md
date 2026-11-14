# Operator Telegram - roznice wzgledem wersji JS

Wersja JS (`bot.js`, `cmd/ytdlp.js`, `lib/`, Telegraf 4.16) zostala zastapiona przez `telegram/`.
Te same komendy, teksty i zmienne `.env`.

- **Bez Telegrafa** - wlasny klient Bot API (hyper + rustls), long polling, ten sam limiter per czat.
- **Sortowanie formatow** - JS przekazywal `res:desc,br:desc,...`, co yt-dlp czyta jako
  "preferowana wartosc `desc`" - sortowanie bylo w praktyce ignorowane. Teraz:
  `res,fps,hdr:sdr,vcodec:h264,acodec:aac,br,asr,size`.
- **Zgodny kodek** - konwersja do H.264/AAC (albo AV1) przed wysylka; w JS HEVC np. z TikToka
  zacinal sie w Discordzie. `YTDLP_VIDEO_CODEC=source` przywraca stare zachowanie.
- **Bezposredni URL** - uzywany tylko dla strumieni juz w docelowym kodeku.
- **Menu komend** - ustawiane przy starcie (Telegraf 4.16 robil to dopiero po zatrzymaniu pollingu).
- **Pasek postepu** - rozmiar przyblizony z yt-dlp (`of ~  10.00MiB`) jest poprawnie odczytywany.
- **Zamykanie** - SIGINT/SIGTERM konczy polling, potwierdza offset i czeka do 25 s na trwajace zadanie.
