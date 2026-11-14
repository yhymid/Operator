# Deploy na VPS

## 1. Zbuduj binarke

Na VPS (`curl https://sh.rustup.rs -sSf | sh`, potem):

```bash
git clone -b discord https://github.com/yhymid/Operator.git && cd Operator
cargo build --release -p operator-discord
```

Albo zbuduj lokalnie i skopiuj `target/release/operator-discord` (ta sama architektura CPU).

## 2. Katalogi i pliki

```bash
sudo mkdir -p /srv/operator/app /srv/operator/data /srv/operator/logs
sudo chown -R ubuntu:ubuntu /srv/operator
cp target/release/operator-discord /srv/operator/app/
cp discord/deploy/.env.example /srv/operator/app/.env            # uzupelnij
cp discord/deploy/config.example.json /srv/operator/app/config.json
```

W `.env` warto ustawic:

```bash
OPERATOR_CONFIG_PATH=/srv/operator/app/config.json
OPERATOR_DATA_DIR=/srv/operator/data
OPERATOR_LOG_DIR=/srv/operator/logs
OPERATOR_SYNC_COMMANDS_ON_STARTUP=0
```

Przenosisz bota z wersji JS? Skopiuj stary `.env`, `config.json` oraz katalogi `cache/` i `logs/` -
format jest ten sam.

## 3. Sprawdzenie i komendy

```bash
cd /srv/operator/app
./operator-discord doctor
./operator-discord commands sync
```

## 4. systemd

```bash
sudo cp discord/deploy/operator-discord.service /etc/systemd/system/operator.service
sudo systemctl daemon-reload
sudo systemctl enable --now operator
journalctl -u operator -f
```

Aktualizacja: zbuduj nowa binarke, podmien `/srv/operator/app/operator-discord`,
`sudo systemctl restart operator`.

Nie uruchamiaj produkcji przez `nohup` z sesji SSH - proces potrafi zginac po wylogowaniu.
