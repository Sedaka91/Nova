# Design

Entschieden: **Leitspur**, die Kommandobrücke als Hauptfenster + Overlay über dem KDE-Desktop. Diese Datei ist die Designvorgabe für die Umsetzung in Phase 7.
Interaktiver Entwurf: https://claude.ai/artifact/W9fN34ZZdGoxcdS8uKaPu4 (Artboards „A Leitspur“). Bei Widerspruch gilt diese Datei.

## Leitidee

Nova zeigt sichtbar, woran sie gerade arbeitet: Leuchtende Spuren laufen von Novas Satz im Gespräch zu den Notizen im Vault-Graphen, die als Kontext geladen sind. Das ist das eine auffällige Element, alles andere bleibt ruhig.

## Layout Kommandobrücke

Kopfzeile (52 px) über drei Spalten. Die Spalten stoßen aneinander und sind nur durch Rahmenlinien getrennt, es gibt keine einzelnen Karten.

| Bereich | Breite | Inhalt |
|---|---|---|
| Kopfzeile | ganze Breite | Orb, „Nova“, aktueller Zustand |
| Gespräch | 300–340 px | Verlauf, unten ausgerichtet; Eingabe „Sprich oder tippe …“ mit Mikrofon-Knopf |
| Vault-Graph | Rest | Graph mit Legende „Als Kontext geladen“ und „Im Vault“ |
| Rechte Spalte | 280–320 px | Tabs Status, Charakter, Lernen |

- **Status:** aktive Stufe, VRAM, Abo-Kontingent mit Fünf-Stunden-Fenster, Dienste.
- **Charakter:** Regler für Sarkasmus, Gesprächigkeit, Förmlichkeit, Humor-Häufigkeit und Ansprache. Darunter der Hinweis, dass Warnungen, Fehler und Löschbestätigungen immer sachlich bleiben.
- **Lernen:** Mindestanzahl Wiederholungen, Knopf „Auswertung starten“, Vorschläge mit „Übernehmen“ und „Ablehnen“. Die Auswertung läuft nur nach diesem Klick.

## Zustände

| Zustand | Anzeige | Orb, Pegel | Graph und Spuren |
|---|---|---|---|
| Bereit | „Bereit“ | Cyan, Pegel ruhig | Keine Hervorhebung |
| Hört zu | „Hört zu“ | Cyan, Pegel schnell | Keine Hervorhebung |
| Denkt nach | „Denkt nach“ | Cyan, Pegel pulsiert als Welle | Kontext-Notizen leuchten, Spuren laufen |
| Spricht | „Spricht“ | Cyan, Pegel folgt der Stimme | Kontext-Notizen leuchten, Spuren laufen |
| Bestätigung nötig | „Bestätigung nötig“ | Gefahr-Farbe, kein Leuchten | Fenster entsättigt, Dialog darüber |

Die Spuren starten an der Nachricht, die die Anfrage ausgelöst hat, und enden an den Kontext-Notizen. Sie sind gepunktet und fließen in Richtung Graph. Nach der Antwort verschwinden sie.

Die aktive Stufe zeigt die laufende oder zuletzt bearbeitete Anfrage.

## Ernst-Modus und Löschen

Der Ernst-Modus hat keine Animation, kein Leuchten und keine lockere Sprache. Das Fenster dahinter wird entsättigt und abgedunkelt.

Der Löschdialog nennt immer den konkreten Pfad und bietet „Datei löschen“ und „Behalten“. Löschen geht nur per Klick. Die Stimme kann diesen Dialog nicht bestätigen.

## Overlay

Pille unten mittig: Orb, „Nova“, 16 Pegelbalken, Zustand. Daneben optional ein Mini-Graph, der mit dem Hauptfenster zusammen die Kontext-Notizen zeigt. Die Pille hat einen eigenen dunklen Grund, damit sie auf hellen und dunklen Fenstern gleich gut lesbar bleibt.

## Tokens

| Rolle | Wert |
|---|---|
| Hintergrund | `#080D1A` |
| Panel | `#0D1424` (86 % deckend, mit Weichzeichner) |
| Dialog | `#121826` |
| Spur (Meter-Grund) | `#1B2740` |
| Rahmen | `rgba(255,255,255,0.08)` |
| Text | `#E6F1FF` |
| Text gedämpft | `#93A4BF` |
| Akzent (Cyan) | `#5CE1FF` |
| Text auf Akzent | `#06202B` |
| Akzent 2 (Violett, Kontingent) | `#8F7CFF` |
| Status ok | `#5EF2B0` |
| Gefahr | `#FF8A7A` |
| Text auf Gefahr | `#2A0C07` |
| Graph-Knoten | `#6E8BB0` |
| Schrift UI | Chakra Petch 400, 500, 600 |
| Schrift Werte, Sprecher, Graph-Beschriftung | IBM Plex Mono 400, 500 |
| Eckenradius Felder, Knöpfe, Dialog | 4 px |

Grundschrift 15 px, Zeilenhöhe 1,5. Texte nicht unter 12 px.

Geprüfte Kontraste: Text auf Panel 16,1:1, gedämpfter Text auf Panel 7,3:1, Text auf Akzent 10,9:1, Text auf Gefahr 7,9:1.

## Regeln

- Beschriftungen in normaler Schreibung, nicht in Großbuchstaben.
- Bewegung nur für Zustände: Pegel und Spuren. Keine Einblendungen beim Laden.
- Bei `prefers-reduced-motion` entfällt alle Bewegung; Spuren bleiben als ruhende Linien sichtbar.
- Sichtbarer Tastaturfokus in Cyan, Klickziele mindestens 44 px.
- Farbe nie allein: Zustände stehen immer auch als Text da.

## Noch offen

Framework und Overlay-Technik sind nicht entschieden. Vor der Wahl muss geprüft werden, ob das schwebende Overlay unter KDE Wayland so möglich ist.
