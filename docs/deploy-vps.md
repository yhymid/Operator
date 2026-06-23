# Deploy na VPS

## Cel

Najbezpieczniejszy flow dla tego projektu:

1. zmiany robimy lokalnie,
2. testujemy lokalnie,
3. dopiero gotowy pakiet wrzucamy na VPS,
4. po deployu wykonujemy krotki smoke-check.

## Zalecany runtime

- Node.js `22.16.0`
- npm `>=10`

## Przygotowanie katalogow

Przyklad:

```bash
mkdir -p /srv/operator/app
mkdir -p /srv/operator/data
mkdir -p /srv/operator/logs
```

## Zmienne srodowiskowe

Przyklad:

```bash
export BOT_TOKEN=twoj_token
export OWNER_IDS=123,456
export OPERATOR_CONFIG_PATH=/srv/operator/app/config.json
export OPERATOR_DATA_DIR=/srv/operator/data
export OPERATOR_LOG_DIR=/srv/operator/logs
export OPERATOR_SYNC_COMMANDS_ON_STARTUP=0
```

## Kolejnosc deployu

1. Wgraj kod do `/srv/operator/app`.
2. Uruchom:

```bash
npm ci
npm run doctor
npm test
```

3. Zarejestruj komendy:

```bash
npm run commands:sync
```

4. Uruchom bota przez `systemd` albo PM2.

## Minimalny systemd service

```ini
[Unit]
Description=Operator Discord Bot
After=network.target

[Service]
Type=simple
WorkingDirectory=/srv/operator/app
Environment=BOT_TOKEN=twoj_token
Environment=OWNER_IDS=123,456
Environment=OPERATOR_CONFIG_PATH=/srv/operator/app/config.json
Environment=OPERATOR_DATA_DIR=/srv/operator/data
Environment=OPERATOR_LOG_DIR=/srv/operator/logs
Environment=OPERATOR_SYNC_COMMANDS_ON_STARTUP=0
ExecStart=/usr/bin/node /srv/operator/app/main.js
Restart=always
RestartSec=5
User=operator
Group=operator

[Install]
WantedBy=multi-user.target
```

## Checklista po deployu

1. `npm run doctor`
2. `npm run commands:sync`
3. `/health`
4. `/debug mode:music`
5. `/logs mode:status`
6. test `verify`, `automod` i jeden prosty `announce`

## Rollback

1. zatrzymaj proces,
2. przywroc poprzednia wersje kodu,
3. przywroc backup `cache/` albo katalogu z `OPERATOR_DATA_DIR`,
4. uruchom `npm run doctor`,
5. uruchom bota ponownie.
