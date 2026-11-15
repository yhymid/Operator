# Operacje na VPS

| Plik | Co robi |
| --- | --- |
| `operator-backup.sh` + `.service` + `.timer` | codziennie o 4:30 pakuje `.env`, `config.json`, `cache/`, `logs/` i `application.yml` do `~/backups/operator` (chmod 600, 14 dni). `BACKUP_COPY_CMD` moze wyslac kopie poza VPS. |
| `operator-notify.sh` + `operator-notify@.service` | gdy unit bota padnie, wysyla na kanal Discorda (webhook) nazwe unitu i ostatnie linie logu |
| `journald-operator.conf` | limit dziennika systemd do 1 GB / 1 miesiaca |
| `operator-update.sh` + `operator-update@.service` + `50-operator-update.rules` | `/update target:ytdlp|lavalink|server` z Discorda: aktualizacja yt-dlp, pluginu YouTube zbudowanego ze zrodel (`youtube-plugin-update.sh` z `SOURCE=1`) albo bota z wydania na GitHubie (`deploy.sh`). Regula polkit pozwala uzytkownikowi bota uruchomic tylko te trzy unity. |

Instalacja:

```bash
sudo install -m 755 scripts/ops/operator-backup.sh scripts/ops/operator-notify.sh /usr/local/bin/
sudo cp scripts/ops/operator-backup.{service,timer} scripts/ops/operator-notify@.service /etc/systemd/system/
sudo mkdir -p /etc/operator /etc/systemd/journald.conf.d
echo 'NOTIFY_WEBHOOK_URL=https://discord.com/api/webhooks/...' | sudo tee /etc/operator/notify.env >/dev/null
sudo chmod 600 /etc/operator/notify.env
sudo cp scripts/ops/journald-operator.conf /etc/systemd/journald.conf.d/operator.conf
sudo systemctl restart systemd-journald
# W unitach botow (sekcja [Unit]):  OnFailure=operator-notify@%n.service
sudo systemctl daemon-reload && sudo systemctl enable --now operator-backup.timer
sudo systemctl start operator-backup && ls -l ~/backups/operator
```

## /update z Discorda

Bot dziala bez roota, wiec `/update` tylko uruchamia `operator-update@<cel>.service` (root,
oneshot). Wynik trafia do `/var/lib/operator-update/<cel>.log`, ktory bot pokazuje w odpowiedzi.

```bash
# Lavalink z pluginem ze zrodel potrzebuje JDK 17-23 do kompilacji (Gradle 8.10) i gita:
sudo apt install -y git openjdk-21-jdk-headless
sudo install -m 755 scripts/ops/operator-update.sh /usr/local/bin/
sudo install -m 755 scripts/deploy.sh /usr/local/bin/operator-deploy.sh
sudo install -m 755 discord/deploy/lavalink/youtube-plugin-update.sh /usr/local/bin/
sudo cp 'scripts/ops/operator-update@.service' /etc/systemd/system/
sudo install -m 644 scripts/ops/50-operator-update.rules /etc/polkit-1/rules.d/
# Sciezki, jesli inne niz domyslne (patrz naglowki skryptow):
printf 'LAVALINK_DIR=%s\nYTDLP_PATH=%s\n' "/home/ubuntu/Main Files/Bots/operator/.local-lavalink" "$(command -v yt-dlp)" |
    sudo tee /etc/operator/update.env >/dev/null
sudo systemctl daemon-reload
# Test bez Discorda, jako uzytkownik bota (polkit nie powinien pytac o haslo):
systemctl start operator-update@ytdlp.service && cat /var/lib/operator-update/ytdlp.log
```

Pierwsze `/update target:lavalink` klonuje youtube-source i buduje plugin (ok. 2 min, Gradle
pobiera zaleznosci do `.youtube-plugin-update/gradle`). Gdy health check po restarcie nie
przejdzie, skrypt przywraca poprzedni `application.yml` i `plugins/`.
