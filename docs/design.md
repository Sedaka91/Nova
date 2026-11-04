# Design

Entschieden: **Kommandobrücke als Hauptfenster + Overlay** über dem KDE-Desktop. Der Kern-Entwurf entfällt.
Entwürfe: https://claude.ai/artifact/MwAZQR8fMHf8Qz9Ab6zQDd (Artboard „2 + 3: Kommandobrücke mit Overlay“).
Das endgültige Design entsteht in Phase 7 mit Frontend Design und UI/UX Pro Max; die Werte unten stammen aus dem Entwurf.

## Layout Kommandobrücke

Drei Glas-Panels: **Gespräch** (links, mit Eingabe „Sprich oder tippe …“) · **Vault-Graph** (Mitte, als Kontext geladene Notizen hervorgehoben) · **Systemstatus** (rechts: aktive Stufe, VRAM, Abo-Kontingent, Dienste).

## Overlay

Pille unten mittig: Orb, „Nova“, Pegelbalken, Status („HÖRT ZU“). Daneben optional ein Mini-Graph.

## Tokens (Entwurf)

| Rolle | Wert |
|---|---|
| Hintergrund | `#080D1A` |
| Panel | `#0D1424` |
| Rahmen | `rgba(255,255,255,0.08)` |
| Text | `#E6F1FF` |
| Text gedämpft | `#93A4BF` |
| Akzent (Cyan) | `#5CE1FF` |
| Akzent 2 (Violett, Kontingent) | `#8F7CFF` |
| Status ok | `#5EF2B0` |
| Graph-Knoten | `#6E8BB0` |
| Schrift | Chakra Petch (UI), IBM Plex Mono (Labels, Werte) |
