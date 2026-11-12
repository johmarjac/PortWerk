# PortWerk

Ein Terminal für Entwickler, das serielle Schnittstellen, TCP-Client, TCP-Server und UDP in einem Fenster vereint. Geschrieben in Rust mit [egui](https://github.com/emilk/egui).

## Funktionen

- **Seriell:** Port, Baudrate, Datenbits, Parität, Stoppbits und Handshake einstellen. DTR, RTS und BREAK schalten, CTS, DSR, RI und CD beobachten.
- **TCP-Client:** mit Geräten oder Diensten verbinden.
- **TCP-Server:** mehrere Clients annehmen, an alle oder an einzelne senden, Clients trennen.
- **UDP:** senden und empfangen, auch Broadcast und Antwort an den letzten Absender.
- **Anzeige:** Text, Hex, Hex + Text oder Dezimal, mit Uhrzeit und Steuerzeichen. Dazu Echo, Pause, Speichern und laufende Mitschrift in eine Datei.
- **Senden:**
  - *Zeilen:* beliebig viele Sendezeilen, als Text mit `$0D`/`#13`-Escapes oder als Hex, mit automatischer Wiederholung.
  - *Schleife:* mehrere Zeilen nacheinander, jede mit eigener Pause, x-mal oder endlos.
  - *Skript:* Lua-5.4-Skripte für Protokolle, also senden, warten, auf Antworten reagieren und Prüfsummen berechnen. Beispiele sind enthalten.
- **ASCII-Tabelle:** Dezimal, Hex, Binär und Zeichen. Ein Klick fügt das Zeichen in die Sendezeile ein.
- **Werkzeuge:** Umrechner für Text, Hex, Dezimal und Binär, Prüfsummen (Summe, XOR, CRC-8, CRC-16 Modbus/CCITT, CRC-32) und ein Zahlenumrechner.
- Alle Einstellungen werden beim Beenden gespeichert.

## Skript-Befehle (Lua)

| Befehl | Bedeutung |
|---|---|
| `send("Text\r\n")` | Text senden (`$XX` und `#DDD` sind erlaubt) |
| `send_hex("01 02 FF")` | Bytes als Hex senden |
| `send_bytes(daten)` | Bytes roh senden (Lua-String) |
| `wait(ms)` | warten |
| `recv(ms)` / `recv_hex(ms)` | Empfangenes holen |
| `wait_for("OK", ms)` | `true`, wenn „OK“ rechtzeitig kam |
| `clear_rx()` | Empfangspuffer leeren |
| `log(...)` / `print(...)` | Meldung ins Fenster schreiben |
| `from_hex`, `to_hex`, `crc16_modbus`, `crc16_ccitt`, `crc32`, `crc8`, `sum8`, `xor8` | Helfer |

## Bauen

```
cargo build --release
```

Das fertige Programm liegt dann unter `target/release/`.

Tests laufen mit `cargo test`.
