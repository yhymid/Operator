# SQLite State Migration Plan

## Cel

Przenieść operacyjny stan bota z lokalnych plików JSON do jednego, stabilniejszego backendu SQLite, ale bez ryzykownego "big bang" switcha.

Priorytety:
- zachować aktualne API `stateStore`,
- ograniczyć ryzyko utraty danych,
- mieć prosty rollback,
- nie ruszać od razu wszystkiego, co nie daje realnego zysku.

## Co migrować w pierwszej kolejności

### Tak

1. `cache/server/*.json`
   - główny stan per guild:
   - `presence`
   - `runtime`
   - `autoclean`
   - `verification`
   - `announcements`
   - `logs`

2. `cache/music-queue/*.json`
   - zapisane kolejki muzyki per guild

3. `cache/user/*/commands.json`
   - tylko jeśli ten feature zostaje
   - dziś wygląda na mało używany, ale można go objąć migracją prawie za darmo

### Nie na etap 1

1. `config.json`
   - to jest konfiguracja, nie runtime state
   - lepiej zostawić osobno, z walidacją i atomowym zapisem

2. `logs/server/**/*.jsonl`
   - to są logi archiwalne, nie aktywny stan
   - plikowy append-only format jest tu nadal sensowny
   - ewentualnie później można dodać indeks SQLite albo metadane, ale nie mieszać tego z migracją state

## Rekomendowany model danych na etap 1

Nie zaczynać od pełnej normalizacji wszystkich sekcji.

Najbezpieczniejszy etap pośredni:
- SQLite jako trwały magazyn dokumentów JSON,
- jeden rekord na guild dla głównego state,
- osobne rekordy dla saved music queue i user command cache.

To daje:
- jeden plik DB zamiast wielu plików JSON,
- transakcyjność,
- prostszy backup,
- minimalne ryzyko regresji, bo kształt payloadu zostaje praktycznie ten sam.

### Proponowane tabele

1. `guild_state_documents`
   - `guild_id TEXT PRIMARY KEY`
   - `guild_name TEXT`
   - `version INTEGER`
   - `payload_json TEXT NOT NULL`
   - `updated_at TEXT NOT NULL`

2. `music_queue_documents`
   - `guild_id TEXT PRIMARY KEY`
   - `payload_json TEXT NOT NULL`
   - `updated_at TEXT NOT NULL`

3. `user_command_documents`
   - `user_id TEXT PRIMARY KEY`
   - `payload_json TEXT NOT NULL`
   - `updated_at TEXT NOT NULL`

4. `migration_meta`
   - `key TEXT PRIMARY KEY`
   - `value_json TEXT NOT NULL`
   - `updated_at TEXT NOT NULL`

## Dlaczego nie pełna normalizacja od razu

Pełna normalizacja brzmi ładnie, ale dziś zwiększa ryzyko:
- obecny kod już operuje na całych dokumentach per guild,
- dużo logiki bazuje na merge payloadu,
- verification i announcements mają sporo małych map i list,
- łatwo zepsuć kompatybilność przy częściowej przebudowie.

Lepszy plan:
- etap 1: SQLite jako document store,
- etap 2: jeśli będzie potrzeba, znormalizować tylko wybrane "hot paths".

Najbardziej sensowne kandydaty do normalizacji w etapie 2:
- verification exceptions / expirations / challenges / lockouts,
- announcement items,
- queued autoclean channels.

## Kompatybilność runtime

Docelowo warto wprowadzić fabrykę backendu, np.:

```txt
createServerStateStore({
  backend: 'json' | 'sqlite',
  jsonCacheDir,
  sqlitePath,
  dualWrite,
  readFallback,
})
```

### Zalecane flagi rolloutu

1. `STATE_BACKEND=json`
   - domyślnie

2. `STATE_SQLITE_PATH=cache/state/operator-state.sqlite`

3. `STATE_DUAL_WRITE=0|1`
   - zapis do JSON i SQLite równocześnie

4. `STATE_READ_FALLBACK=0|1`
   - jeśli rekord nie istnieje w SQLite, odczytaj JSON

## Etapy wdrożenia

### Etap 0: przygotowanie

Status:
- bez przełączania runtime
- tylko backup, dry-run, porównanie danych

Zakres:
- zrobić snapshot JSON,
- zapisać go do SQLite lab DB,
- odczytać z SQLite,
- porównać round-trip 1:1,
- przetestować restore z backupu.

### Etap 1: shadow backend

Status:
- runtime nadal czyta z JSON
- opcjonalnie zapisuje też do SQLite

Zakres:
- dodać `SQLiteStateStore`,
- nie używać go jako primary read path,
- włączyć `dualWrite` tylko na testowym środowisku.

Warunek wyjścia:
- kilka restartów bez rozjazdów,
- brak różnic w porównaniu JSON vs SQLite snapshot.

### Etap 2: read prefer SQLite

Status:
- odczyt preferuje SQLite,
- JSON zostaje jako fallback i dodatkowy safety net

Zakres:
- nowe dane idą do SQLite,
- JSON może być jeszcze utrzymywany przez jakiś czas przez `dualWrite=1`.

Warunek wyjścia:
- brak błędów po restarcie,
- brak brakujących rekordów,
- narzędzia admina zwracają ten sam stan co wcześniej.

### Etap 3: JSON jako backup-only

Status:
- primary backend to SQLite
- JSON nie jest już źródłem prawdy

Zakres:
- wyłączyć `readFallback`,
- opcjonalnie wyłączyć `dualWrite`,
- zostawić eksport snapshotu do JSON jako backup operacyjny.

## Backup i ograniczanie ryzyka

Przed każdą realną migracją:

1. Snapshot źródeł:
   - `cache/server`
   - `cache/music-queue`
   - `cache/user`

2. Manifest backupu:
   - ścieżka pliku,
   - rozmiar,
   - hash SHA-256

3. SQLite w osobnym pliku:
   - nie nadpisywać starych DB przy dry-run

4. Dopiero po zgodności:
   - rozważyć wpięcie do runtime

## Rollback

Rollback ma być trywialny:

1. ustawić backend z powrotem na `json`,
2. zatrzymać bota,
3. przywrócić backup katalogów `cache/*`,
4. uruchomić bota ponownie,
5. porównać snapshot po starcie.

Jeśli etap 1 działa w `dualWrite`, rollback nie wymaga nawet odzyskiwania SQLite. Wystarczy wyłączyć SQLite i wrócić do JSON.

## Test plan

Minimum przed przełączeniem runtime:

1. Odczyt i zapis stanu
   - zapis przykładowego guild state do SQLite
   - odczyt i porównanie z JSON snapshot

2. Restart
   - zamknąć połączenie
   - otworzyć DB ponownie
   - potwierdzić identyczny odczyt

3. Zgodność po migracji próbnej
   - JSON snapshot -> SQLite -> snapshot
   - porównanie 1:1

4. Rollback
   - zmienić pliki źródłowe
   - przywrócić backup
   - potwierdzić zgodność z oryginalnym snapshotem

## Narzędzia przygotowane w repo

1. `src/core/state-migration.js`
   - budowa snapshotu
   - backup
   - restore
   - zapis/odczyt lab DB SQLite
   - porównanie round-trip

2. `scripts/state-migration-dry-run.js`
   - wykonuje bezpieczny dry-run:
   - backup JSON
   - import do SQLite
   - odczyt z SQLite
   - porównanie zgodności

## Decyzja techniczna

Na teraz jest jeszcze za wcześnie na ślepe przełączenie produkcji na SQLite.

Rozsądny następny krok:
- uruchamiać dry-run i backup,
- dodać później adapter `SQLiteStateStore`,
- dopiero potem robić controlled rollout z `dualWrite`.
