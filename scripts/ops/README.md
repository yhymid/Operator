# Operacje na VPS

| Plik | Co robi |
| --- | --- |
| `operator-backup.sh` + `.service` + `.timer` | codziennie o 4:30 pakuje `.env`, `config.json`, `cache/`, `logs/` i `application.yml` do `~/backups/operator` (chmod 600, 14 dni). `BACKUP_COPY_CMD` moze wyslac kopie poza VPS. |
| `operator-notify.sh` + `operator-notify@.service` | gdy unit bota padnie, wysyla na kanal Discorda (webhook) nazwe unitu i ostatnie linie logu |
| `journald-operator.conf` | limit dziennika systemd do 1 GB / 1 miesiaca |

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
