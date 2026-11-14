# Przepisanie Operatora na Rust

Cel: ta sama funkcjonalnosc co wersje JS, mniejsze zuzycie RAM, szybszy start, jeden statyczny plik binarny.

## Galezie

- `rust/discord-rewrite` - bot Discord (`crates/operator-discord`)
- `rust/telegram-rewrite` - bot Telegram (`crates/operator-telegram`)

Obie galezie zaczynaja sie od tej samej podstawy (workspace + `crates/operator-common`).

## Uklad

```
Cargo.toml                  workspace, wspolne wersje zaleznosci, profil release (LTO, strip)
crates/operator-common      env/.env, logowanie, atomowy zapis JSON, czas ISO jak w JS, sygnaly
crates/operator-discord     bot Discord (galaz rust/discord-rewrite)
crates/operator-telegram    bot Telegram (galaz rust/telegram-rewrite)
```

## Zasady zgodnosci

- Te same zmienne srodowiskowe i pliki `.env` co w wersjach JS.
- Te same pliki stanu na dysku (`cache/server/<guildId>.json`, `config.json` itd.) - mozna przelaczac sie
  miedzy wersja JS i Rust bez migracji.
- Daty zapisywane w formacie `Date#toISOString()` (`2026-01-31T12:00:00.000Z`).
- Kod JS zostaje w repo jako punkt odniesienia, dopoki wersja Rust nie osiagnie pelnej zgodnosci.

## Budowanie

```bash
cargo build --release
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Status

- Discord: `crates/operator-discord`, opis i roznice w [rust-discord.md](rust-discord.md).
