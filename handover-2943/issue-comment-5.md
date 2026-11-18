Follow-up (mac-e688), M3 (`velt build` from the unit):

- **Debug goldens with builds from disk: 4234 files, 118 failures in 72 files**, down from 137 failures in 96 files. No file that passed before fails now.
- Fixed:
  - definitions the program's build synthesizes (object-literal accessors and function fields) are now in the program and lowering's live set;
  - a build that reaches `catchUnwind` demands std's panic path.
- What's left:
  - `proxy_throws*`: the error types of std's `Proxy` glue come from error-set variables solved over the whole program (the handler traps), and std's stage solves them without the app;
  - error tests whose message differs, as in the check sweep;
  - a few sharing and transfer panics, and sema rejections that are already among the known units differences.
- Tried and reverted: running lazy std's live walk over the set to demand the same hooks as lazy std. The goldens came out identical and builds were slower; it comes back once that walk replaces the reach walk.

Warm rebuild after a one-line edit, todo_api: lazy 9.5 G vs disk 6.8 G instructions.
