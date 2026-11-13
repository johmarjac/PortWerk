# PortWerk

Ein Terminal für Entwickler, das serielle Schnittstellen, TCP-Client, TCP-Server, UDP, Modbus, MQTT und REST in einem Fenster vereint. Geschrieben in Rust mit [egui](https://github.com/emilk/egui).

## Funktionen

- **Mehrere Sitzungen:** Jede Verbindungsart kann beliebig viele Sitzungen als Reiter haben. Sitzungen lassen sich öffnen, per Doppelklick umbenennen, duplizieren und schließen, und alle laufen gleichzeitig.
- **Seriell:** Port, Baudrate, Datenbits, Parität, Stoppbits und Handshake einstellen. DTR, RTS und BREAK schalten, CTS, DSR, RI und CD beobachten.
- **TCP-Client:** mit Geräten oder Diensten verbinden.
- **TCP-Server:** mehrere Clients annehmen, an alle oder an einzelne senden, Clients trennen.
- **UDP:** senden und empfangen, auch Broadcast und Antwort an den letzten Absender.
- **Modbus-Client RTU und TCP:** Coils, diskrete Eingänge, Holding- und Input-Register lesen (Funktionen 01–04) und schreiben (05, 06, 15, 16). Zyklische Abfrage, Anzeige als 16 oder 32 Bit, Float oder Text in beiden Wortreihenfolgen, Datenverkehr als Hex und Klartext-Meldungen bei Ausnahmen.
- **Modbus-TCP-Server (Geräte-Simulator):** Coils, diskrete Eingänge, Holding- und Input-Register in einer Tabelle bearbeiten, mehrere Clients gleichzeitig bedienen, auf Wunsch nur eine Geräteadresse beantworten, Werte automatisch hochzählen oder zufällig ändern. Alle Anfragen werden im Klartext und als Hex protokolliert. Die Werte werden im Projekt mitgespeichert.
- **MQTT-Client (3.1.1):** mehrere Broker gleichzeitig, wahlweise mit TLS (auch eigene CA-Datei oder ohne Zertifikatsprüfung), Benutzer und Passwort, Keep-Alive, Clean Session und automatischem Neuverbinden. Abos mit `+` und `#` und QoS 0–2, eine Nachrichtenliste als Text, Hex oder JSON mit Filter, Pause, Speichern und Mitschrift, eine Themenübersicht mit letztem Wert sowie Veröffentlichen mit QoS, Retain und automatischer Wiederholung.
- **REST-Client:** Anfragen mit GET, POST, PUT, PATCH, DELETE, HEAD und OPTIONS, mit Parametern, Headern, Body (JSON, Text, Formular) und Anmeldung (Bearer-Token, Benutzer + Passwort, API-Schlüssel). Dazu gespeicherte Anfragen, Variablen wie `{{baseUrl}}`, Verlauf, formatierte JSON-Antworten und Export als cURL-Befehl.
- **Anzeige:** Text, Hex, Hex + Text oder Dezimal, mit Uhrzeit und Steuerzeichen. Dazu Echo, Pause, Speichern und laufende Mitschrift in eine Datei.
- **Senden:**
  - *Zeilen:* beliebig viele Sendezeilen, als Text mit `$0D`/`#13`-Escapes oder als Hex, mit automatischer Wiederholung.
  - *Schleife:* mehrere Zeilen nacheinander, jede mit eigener Pause, x-mal oder endlos.
  - *Skript:* Lua-5.4-Skripte für Protokolle, also senden, warten, auf Antworten reagieren und Prüfsummen berechnen. Beispiele sind enthalten.
- **ASCII-Tabelle:** Dezimal, Hex, Binär und Zeichen. Ein Klick fügt das Zeichen in die Sendezeile ein.
- **Werkzeuge:** Umrechner für Text, Hex, Dezimal und Binär, Prüfsummen (Summe, XOR, CRC-8, CRC-16 Modbus/CCITT, CRC-32) und ein Zahlenumrechner.
- **Projektdateien (`.portwerk`):** Alle Sitzungen, Sendezeilen, Skripte, Modbus- und REST-Einstellungen lassen sich in einer Datei speichern und weitergeben. Ein Doppelklick auf die Datei öffnet PortWerk mit dem Projekt: Der Windows-Installer und das Linux-Paket registrieren die Dateiendung, und die portable Version kann das über *Projekt → .portwerk-Dateien mit PortWerk öffnen* selbst erledigen. Projektdateien lassen sich auch einfach ins Fenster ziehen.
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
| `mqtt_publish(thema, inhalt [, qos, retain, sitzung])` | über eine verbundene MQTT-Sitzung veröffentlichen |
| `http_get(url)`, `http_post(url, body)`, `http_request(methode, url, body, typ)` | HTTP-Anfragen, liefern Status und Antwort |
| `from_hex`, `to_hex`, `crc16_modbus`, `crc16_ccitt`, `crc32`, `crc8`, `sum8`, `xor8` | Helfer |

## Releases (Downloads für alle Systeme)

Eine neue Version erstellen Sie so:

1. Auf GitHub den Reiter **Actions** öffnen und links **Release** wählen.
2. Rechts auf **Run workflow** klicken und eine Version eintragen, zum Beispiel `0.2.0`.
3. Nach etwa 10–15 Minuten liegt unter **Releases** die neue Version mit allen Dateien.

Statt Schritt 1 und 2 können Sie auch einen Tag wie `v0.2.0` hochladen.

| System | Datei |
|---|---|
| Windows | `…-windows-x64.msi` (Installer) und `…-windows-x64-portable.exe` (ohne Installation) |
| macOS (Intel + Apple Silicon) | `…-macos-universal.dmg` und `…-macos-universal.zip` |
| Linux x64 / ARM64 | `…-linux-x64.deb`, `…-linux-arm64.deb` und `.tar.gz` |

Bei jedem Hochladen auf `main` baut und testet der Workflow **CI** das Programm automatisch unter Windows, macOS und Linux.

## Bauen

```
cargo build --release
```

Das fertige Programm liegt dann unter `target/release/`.

Tests laufen mit `cargo test`.
