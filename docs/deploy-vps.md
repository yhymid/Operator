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

## 5. Lavalink (muzyka)

```bash
sudo apt install openjdk-21-jre-headless      # albo nowsza Java (17+)
sudo mkdir -p /srv/operator/lavalink && sudo chown ubuntu:ubuntu /srv/operator/lavalink
cd /srv/operator/lavalink
curl -LO https://github.com/lavalink-devs/Lavalink/releases/latest/download/Lavalink.jar
mv Lavalink.jar lavalink.jar
cp /path/to/Operator/discord/deploy/lavalink/application.yml.example application.yml   # uzupelnij
sudo cp /path/to/Operator/discord/deploy/lavalink/lavalink.service /etc/systemd/system/
sudo systemctl daemon-reload && sudo systemctl enable --now lavalink
journalctl -u lavalink -f        # przy pierwszym starcie: kod OAuth YouTube do potwierdzenia
```

Po potwierdzeniu OAuth wpisz wypisany refresh token do `application.yml` (`plugins.youtube.oauth.refreshToken`)
i zrestartuj `lavalink`. Restartuj Lavalinka tylko, gdy nic nie gra (`GET /v4/stats` -> `playingPlayers`).

Nie uruchamiaj produkcji przez `nohup` z sesji SSH - proces potrafi zginac po wylogowaniu.

### Automatyczna aktualizacja pluginu youtube-source (prototyp)

`discord/deploy/lavalink/youtube-plugin-update.{sh,service,timer}` co ~3 h sprawdza najnowszy snapshot
w `maven-metadata.xml`. Podmienia wersje w `application.yml` i restartuje Lavalinka tylko, gdy
`/v4/stats` zwraca `playingPlayers == 0`; potem sprawdza `/v4/loadtracks` i `/youtube/stream/{id}`.
Gdy test nie przejdzie - przywraca kopie `application.yml` i `plugins/`, a wersje dopisuje do
`.youtube-plugin-update/bad-versions` (nie bedzie ponawiana).

```bash
sudo install -m 755 discord/deploy/lavalink/youtube-plugin-update.sh /usr/local/bin/
sudo cp discord/deploy/lavalink/youtube-plugin-update.{service,timer} /etc/systemd/system/
sudo mkdir -p /etc/operator   # opcjonalnie youtube-plugin-update.env (chmod 600), np. LAVALINK_PASSWORD=...
sudo DRY_RUN=1 /usr/local/bin/youtube-plugin-update.sh   # najpierw na sucho
sudo systemctl daemon-reload && sudo systemctl enable --now youtube-plugin-update.timer
journalctl -u youtube-plugin-update -n 50
```
