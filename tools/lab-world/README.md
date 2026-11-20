# Lab world extras

Files to copy into a lab world, not a world of their own.

- `weapons/-LabChat.txt`: a chat line for 5.x and 6.x, which have no chat
  bar unless the server sends one (#45, #163, #174). Enter opens the line,
  Enter again says it (on an empty line it just closes); Backspace deletes.
  Weapons are off while the line is open, so letters do not also fire one.
  Escape is not used: 5.24r4 opens its own quit box on it. 5.x and 6.x come
  in through the lobby as guests, so give it with `WEAPON -LabChat` in
  `accounts/guest.txt` only; 1.x and 2.x keep their own chat bar and would
  get it as GS1 text. It is GS1, compiled to GS2 for 5.24 on and sent by
  197 and 140. The line is image 200 moved to the screen layer
  (`changeimgvis 200,4`): on 5.x/6.x a GS2 `showtext` is drawn on the
  level, in tiles, whatever the index. Lab: `docs/lab/labchat-2026-11-20.md`.
