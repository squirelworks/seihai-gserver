# Lab world extras

Files to copy into a lab world, not a world of their own.

- `weapons/-LabChat.txt`: a chat line for 5.x and 6.x, which have no chat
  bar unless the server sends one (#45, #163). Type, then Enter says the
  line; Backspace deletes, Escape clears. 5.x and 6.x come in through the
  lobby as guests, so give it with `WEAPON -LabChat` in
  `accounts/guest.txt` only; 1.x and 2.x keep their own chat bar and would
  get it as GS1 text. It is GS1, compiled to GS2 for 5.24 on and sent by
  197 and 140. The input line is image 200 moved to the screen layer
  (`changeimgvis 200,4`): on 5.x/6.x a GS2 `showtext` is drawn on the
  level, in tiles, whatever the index, so without it the line was far
  off screen on 5.24r4 and 6.03r7 (#174).
