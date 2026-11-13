# Änderungen

## [0.1.0] – 2026-11-13

Erste Version von PortWerk.

### Verbindungen
- **Mehrere Sitzungen je Verbindungsart** als Reiter: neu anlegen, per Doppelklick umbenennen, duplizieren, schließen. Alle Sitzungen laufen gleichzeitig.
- **Seriell:** Port, Baudrate, Datenbits, Parität, Stoppbits und Handshake. DTR, RTS und BREAK schalten, CTS, DSR, RI und CD anzeigen.
- **TCP-Client** und **TCP-Server** (mehrere Clients, an alle oder einzelne senden, Clients trennen).
- **UDP** mit Broadcast und Antwort an den letzten Absender.
- **Modbus-Client RTU und TCP:** Funktionen 01–06, 15 und 16, zyklisches Lesen, Anzeige als 16 oder 32 Bit, Float oder Text, Coils per Klick umschalten.
- **Modbus-TCP-Server** als Geräte-Simulator: Werte in einer Tabelle bearbeiten, Werte hochzählen oder zufällig ändern, mehrere Clients gleichzeitig.
- **MQTT-Client (3.1.1):** TCP oder TLS (auch eigene CA-Datei), Abos mit `+` und `#`, Nachrichtenliste als Text, Hex oder JSON, Themenübersicht, Veröffentlichen mit QoS und Retain, automatische Wiederholung.
- **REST-Client:** alle gängigen Methoden, Parameter, Header, JSON-, Text- und Formular-Inhalt, Anmeldung, Variablen wie `{{baseUrl}}`, Verlauf, Export als cURL-Befehl.

### Senden und Anzeigen
- Anzeige als Text, Hex, Hex + Text oder Dezimal, mit Uhrzeit und Steuerzeichen. Dazu Echo, Pause, Speichern und laufende Mitschrift.
- Beliebig viele Sendezeilen als Text (mit `$0D`, `#13`, `\r\n`) oder Hex, mit automatischer Wiederholung.
- Schleifen: mehrere Zeilen nacheinander mit eigener Pause, x-mal oder endlos.
- Lua-Skripte für Protokolle: senden, auf Antworten warten, Prüfsummen, HTTP-Anfragen und `mqtt_publish`. Beispiele sind enthalten.
- ASCII-Tabelle: ein Klick fügt das Zeichen in die Sendezeile ein.

### Werkzeuge und Projekte
- Umrechner für Text, Hex, Dezimal und Binär, Prüfsummen (Summe, XOR, CRC-8, CRC-16 Modbus/CCITT, CRC-32) und ein Zahlenumrechner.
- **Projektdateien (`.portwerk`)** mit allen Sitzungen und Einstellungen zum Weitergeben. Ein Doppelklick öffnet PortWerk mit dem Projekt: Unter Windows und Linux richtet der Installer bzw. das Paket das ein, die portable Version über das Menü „Projekt“.
- Downloads für Windows (MSI-Installer und portable EXE), macOS (Intel und Apple Silicon) sowie Linux (.deb und .tar.gz, x64 und ARM64).
